// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `search_data` tool: a model describes data, the host writes one read-only query, and the
//! query runs through the same guarded path as `run_query`.

use std::sync::Arc;

use serde_json::{Value, json};
use sqler_ai_api::BoxFuture;

use super::prompts::Task;
use super::{Assist, Conversation};
use crate::error::ToolError;
use crate::guard;
use crate::query_tools::{positive, run_sql};
use crate::schema::{SearchDataArgs, names};
use crate::source::ConnectionInfo;
use crate::toolset::{Inner, Ran};

/// The SQL a [`DataSearch`] wrote for a question.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GeneratedSql {
    /// The statement to run.
    pub sql: String,
    /// Why this statement answers the question.
    pub rationale: String,
}

impl GeneratedSql {
    /// A generated statement.
    #[must_use]
    pub fn new(sql: impl Into<String>, rationale: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            rationale: rationale.into(),
        }
    }
}

/// Implemented by hosts that can turn a question into SQL for `search_data`.
///
/// Attach one with [`crate::ToolSet::with_data_search`]; without one the tool is neither listed
/// nor callable. The tool set the implementation itself calls must be a different one (without
/// a `DataSearch`), or the two would hold each other.
pub trait DataSearch: Send + Sync {
    /// Writes SQL for `question` on `connection`. Never runs it.
    fn generate<'a>(
        &'a self,
        connection: &'a ConnectionInfo,
        question: &'a str,
    ) -> BoxFuture<'a, Result<GeneratedSql, ToolError>>;

    /// Called with the generated SQL before it runs, so a host can show it (ADR-0003). It cannot
    /// stop the run: anything that is not a plain read is refused before this is called.
    fn shown<'a>(&'a self, _connection: &'a ConnectionInfo, _sql: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

type Notice = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// A [`DataSearch`] backed by an [`Assist`] engine.
pub struct AssistSearch {
    assist: Assist,
    notice: Option<Notice>,
}

impl std::fmt::Debug for AssistSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistSearch")
            .field("assist", &self.assist)
            .field("notice", &self.notice.is_some())
            .finish()
    }
}

impl AssistSearch {
    /// A search host over `assist`. The engine's tool set must not have a `DataSearch`.
    #[must_use]
    pub fn new(assist: Assist) -> Self {
        Self {
            assist,
            notice: None,
        }
    }

    /// Sets the closure called with `(connection id, sql)` before generated SQL runs.
    #[must_use]
    pub fn with_notice(mut self, notice: Notice) -> Self {
        self.notice = Some(notice);
        self
    }
}

impl DataSearch for AssistSearch {
    fn generate<'a>(
        &'a self,
        connection: &'a ConnectionInfo,
        question: &'a str,
    ) -> BoxFuture<'a, Result<GeneratedSql, ToolError>> {
        Box::pin(async move {
            let mut conversation = Conversation::new();
            let answer = self
                .assist
                .nl_to_sql_with(
                    Task::SearchData,
                    &connection.id,
                    question,
                    &mut conversation,
                )
                .await
                .map_err(|e| ToolError::Host(format!("could not write SQL: {e}")))?;
            Ok(GeneratedSql::new(answer.sql, answer.rationale))
        })
    }

    fn shown<'a>(&'a self, connection: &'a ConnectionInfo, sql: &'a str) -> BoxFuture<'a, ()> {
        if let Some(notice) = &self.notice {
            notice(&connection.id, sql);
        }
        Box::pin(std::future::ready(()))
    }
}

/// Runs `search_data`: generate, refuse anything but a plain read, show, run under the limits.
pub(crate) async fn search_data(
    ts: &Inner,
    search: &dyn DataSearch,
    info: &ConnectionInfo,
    args: &SearchDataArgs,
    ran: &mut Ran,
) -> Result<Value, ToolError> {
    positive("max_rows", args.max_rows)?;
    if args.question.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "`question` must not be empty".to_owned(),
        ));
    }
    let generated = search.generate(info, &args.question).await?;
    let plan = guard::plan(info, &generated.sql, None)?;
    ran.planned = plan
        .iter()
        .map(|p| (p.sql.clone(), p.class, p.requires_approval()))
        .collect();
    guard::require_unapproved_free(names::SEARCH_DATA, &plan)?;
    search.shown(info, &generated.sql).await;
    let mut body = run_sql(
        ts,
        names::SEARCH_DATA,
        info,
        &generated.sql,
        None,
        args.max_rows,
        ran,
    )
    .await?;
    if let Some(map) = body.as_object_mut() {
        map.insert("sql".to_owned(), json!(generated.sql));
        map.insert("rationale".to_owned(), json!(generated.rationale));
    }
    Ok(body)
}
