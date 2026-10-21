// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tool set: the one entry every AI surface calls.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sqler_ai_api::{ToolCall, ToolDefinition, ToolResult};
use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::DbObject;
use sqler_sql::StatementClass;

use crate::approve::Approver;
use crate::error::ToolError;
use crate::limits::Limits;
use crate::schema::{self, names};
use crate::source::{ConnectionInfo, SessionSource};
use crate::ui::UiHost;
use crate::{catalog_tools, query_tools};

/// The most entries of the function-definition cache; it is cleared when full.
const DEFINITION_CACHE_CAP: usize = 256;

/// Why a statement's rows stop short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Truncation {
    /// The statement returned more rows than the row cap.
    RowLimit,
    /// The deadline passed and the driver was cancelled.
    TimeLimit,
}

impl Truncation {
    /// The value in the result JSON: `row_limit` or `time_limit`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RowLimit => "row_limit",
            Self::TimeLimit => "time_limit",
        }
    }
}

/// A statement that was sent to the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExecutedStatement {
    /// The exact SQL sent.
    pub sql: String,
    /// Its class.
    pub class: StatementClass,
    /// Whether the approver explicitly approved it (a plain read needs none).
    pub approved: bool,
    /// Rows kept.
    pub rows: u64,
    /// Whether and why the rows were cut.
    pub truncated: Option<Truncation>,
}

/// A tool result plus what an audit log needs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CallReport {
    /// The result to give back to the model.
    pub result: ToolResult,
    /// The id of the connection the call used, if it named one.
    pub connection: Option<String>,
    /// The statements sent to the driver, in order.
    pub executed: Vec<ExecutedStatement>,
}

/// What a dispatched call leaves behind besides its content.
#[derive(Debug, Default)]
pub(crate) struct Ran {
    pub connection: Option<String>,
    pub executed: Vec<ExecutedStatement>,
}

type DefinitionKey = (String, DbObject);

pub(crate) struct Inner {
    pub source: Arc<dyn SessionSource>,
    pub limits: Limits,
    pub approver: Option<Arc<dyn Approver>>,
    pub ui: Option<Arc<dyn UiHost>>,
    definitions: Mutex<HashMap<DefinitionKey, Option<String>>>,
}

impl Inner {
    /// The connection named by `name` (an id, else a unique display name).
    pub(crate) fn resolve(&self, name: &str) -> Result<ConnectionInfo, ToolError> {
        let all = self.source.connections();
        if let Some(found) = all.iter().find(|c| c.id == name) {
            return Ok(found.clone());
        }
        let mut by_name = all.iter().filter(|c| c.name == name);
        match (by_name.next(), by_name.next()) {
            (Some(found), None) => Ok(found.clone()),
            _ => Err(ToolError::UnknownConnection(name.to_owned())),
        }
    }

    pub(crate) fn catalog(&self, info: &ConnectionInfo) -> Result<Catalog, ToolError> {
        self.source.catalog(&info.id).ok_or_else(|| {
            ToolError::NotAvailable(format!("no catalog for connection `{}`", info.id))
        })
    }

    pub(crate) fn ui(&self) -> Result<&dyn UiHost, ToolError> {
        self.ui.as_deref().ok_or_else(|| {
            ToolError::NotAvailable("this host has no editor or results tabs".to_owned())
        })
    }

    #[allow(clippy::option_option)] // outer: cache hit, inner: the definition may be absent
    pub(crate) fn cached_definition(&self, key: &DefinitionKey) -> Option<Option<String>> {
        self.definitions.lock().ok()?.get(key).cloned()
    }

    pub(crate) fn store_definition(&self, key: DefinitionKey, text: Option<String>) {
        if let Ok(mut cache) = self.definitions.lock() {
            if cache.len() >= DEFINITION_CACHE_CAP {
                cache.clear();
            }
            cache.insert(key, text);
        }
    }
}

/// The tools a model may call, with every guard behind them.
///
/// Built once per host from a [`SessionSource`] and [`Limits`], optionally with an
/// [`Approver`] (without one, every statement that is not a plain read is refused) and a
/// [`UiHost`] (without one, the UI-only tools are not listed and not callable). Cheap to clone.
///
/// # Guards
///
/// - Every statement is classified by `sqler-sql`; only `Read` runs unasked. Write, DDL, other
///   and unrecognized statements need approval per statement, showing the exact SQL.
/// - All approvals of a call are collected before the first statement runs; one denial stops
///   the call with nothing run.
/// - A `prod` connection asks twice per statement.
/// - `EXPLAIN ANALYZE` always needs approval.
/// - Row and time limits apply to every SQL call; on `prod` they cannot be switched off.
///   Exceeding them truncates, says so, and a timeout cancels the driver.
#[derive(Clone)]
pub struct ToolSet {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ToolSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSet")
            .field("limits", &self.inner.limits)
            .field("approver", &self.inner.approver.is_some())
            .field("ui_host", &self.inner.ui.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolSet {
    /// A tool set over `source` with `limits`, no approver and no UI host.
    #[must_use]
    pub fn new(source: Arc<dyn SessionSource>, limits: Limits) -> Self {
        Self {
            inner: Arc::new(Inner {
                source,
                limits,
                approver: None,
                ui: None,
                definitions: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn rebuilt(&self, change: impl FnOnce(&mut Inner)) -> Self {
        let mut inner = Inner {
            source: Arc::clone(&self.inner.source),
            limits: self.inner.limits,
            approver: self.inner.approver.clone(),
            ui: self.inner.ui.clone(),
            definitions: Mutex::new(HashMap::new()),
        };
        change(&mut inner);
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Sets the approver.
    #[must_use]
    pub fn with_approver(&self, approver: Arc<dyn Approver>) -> Self {
        self.rebuilt(|i| i.approver = Some(approver))
    }

    /// Sets the UI host.
    #[must_use]
    pub fn with_ui_host(&self, ui: Arc<dyn UiHost>) -> Self {
        self.rebuilt(|i| i.ui = Some(ui))
    }

    /// The tool definitions to export to providers and MCP.
    #[must_use]
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        schema::definitions(self.inner.ui.is_some())
    }

    /// Runs one tool call. Never fails: errors become error results.
    pub async fn call(&self, call: &ToolCall) -> ToolResult {
        self.call_detailed(call).await.result
    }

    /// Runs one tool call and reports what ran.
    pub async fn call_detailed(&self, call: &ToolCall) -> CallReport {
        let mut ran = Ran::default();
        let outcome = self.run(call, &mut ran).await;
        let result = match outcome {
            Ok(content) => ToolResult::ok(&call.id, content.to_string()),
            Err(err) => ToolResult::error(&call.id, error_json(&err).to_string()),
        };
        CallReport {
            result,
            connection: ran.connection,
            executed: ran.executed,
        }
    }

    async fn run(&self, call: &ToolCall, ran: &mut Ran) -> Result<Value, ToolError> {
        let args = if call.arguments.trim().is_empty() {
            json!({})
        } else {
            call.parse_arguments()
                .map_err(|e| ToolError::InvalidArguments(e.to_string()))?
        };
        self.dispatch(&call.name, args, ran).await
    }

    /// The one place a tool name becomes a tool.
    async fn dispatch(&self, name: &str, args: Value, ran: &mut Ran) -> Result<Value, ToolError> {
        let ts = &*self.inner;
        match name {
            names::LIST_CONNECTIONS => {
                schema::parse::<schema::NoArgs>(args)?;
                Ok(catalog_tools::list_connections(ts))
            }
            names::LIST_SCHEMAS => {
                let a: schema::ListSchemasArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::list_schemas(ts, &info, &a).await
            }
            names::LIST_TABLES => {
                let a: schema::ListTablesArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::list_tables(ts, &info, &a).await
            }
            names::DESCRIBE_TABLE => {
                let a: schema::DescribeArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::describe_table(ts, &info, &a).await
            }
            names::DESCRIBE_FUNCTION => {
                let a: schema::DescribeArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::describe_function(ts, &info, &a).await
            }
            names::SEARCH_SCHEMA => {
                let a: schema::SearchSchemaArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::search_schema(ts, &info, &a)
            }
            names::SAMPLE_ROWS => {
                let a: schema::SampleRowsArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                query_tools::sample_rows(ts, &info, &a, ran).await
            }
            names::RUN_QUERY => {
                let a: schema::RunQueryArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                query_tools::run_query(ts, &info, &a, ran).await
            }
            names::EXPLAIN_QUERY => {
                let a: schema::ExplainArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                query_tools::explain_query(ts, &info, &a, ran).await
            }
            names::OPEN_IN_EDITOR => {
                let a: schema::OpenInEditorArgs = schema::parse(args)?;
                query_tools::open_in_editor(ts, &a).await
            }
            names::OPEN_RESULTS_TAB => {
                let a: schema::OpenResultsTabArgs = schema::parse(args)?;
                query_tools::open_results_tab(ts, &a).await
            }
            names::SEARCH_DATA => Err(ToolError::NotAvailable(
                "search_data is not available in this host".to_owned(),
            )),
            other => Err(ToolError::UnknownTool(other.to_owned())),
        }
    }

    fn connection(&self, name: &str, ran: &mut Ran) -> Result<ConnectionInfo, ToolError> {
        let info = self.inner.resolve(name)?;
        ran.connection = Some(info.id.clone());
        Ok(info)
    }
}

/// The error result body: `{"error":{"code":..,"message":..}}`.
fn error_json(err: &ToolError) -> Value {
    json!({"error": {"code": err.code(), "message": err.to_string()}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_body_has_code_and_message() {
        let body = error_json(&ToolError::UnknownTool("zap".into()));
        assert_eq!(body["error"]["code"], "unknown_tool");
        assert_eq!(body["error"]["message"], "unknown tool: zap");
    }

    #[test]
    fn truncation_names() {
        assert_eq!(Truncation::RowLimit.as_str(), "row_limit");
        assert_eq!(Truncation::TimeLimit.as_str(), "time_limit");
    }
}
