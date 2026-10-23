// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The SQL tools (`run_query`, `sample_rows`, `explain_query`) and the UI-only tools.

use serde_json::{Value, json};
use sqler_driver_api::sqler_core::{DbObject, ObjectKind};
use sqler_sql::{Dialect, qualified_name};

use crate::approve::ApprovalReason;
use crate::error::ToolError;
use crate::exec::{self, Execution};
use crate::guard;
use crate::json as cells;
use crate::schema::{
    ExplainArgs, OpenInEditorArgs, OpenResultsTabArgs, RunQueryArgs, SampleRowsArgs, addr_path,
};
use crate::source::ConnectionInfo;
use crate::toolset::{ExecutedStatement, Inner, Phase, Ran, Truncation};

/// The default number of rows `sample_rows` reads.
const DEFAULT_SAMPLE_ROWS: u32 = 50;

/// Plans, asks for approvals, runs and renders one call.
pub(crate) async fn run_sql(
    ts: &Inner,
    tool: &'static str,
    info: &ConnectionInfo,
    sql: &str,
    force: Option<ApprovalReason>,
    requested_rows: Option<u32>,
    ran: &mut Ran,
) -> Result<Value, ToolError> {
    let plan = guard::plan(info, sql, force)?;
    ran.set_planned(
        plan.iter()
            .map(|p| (p.sql.clone(), p.class, p.requires_approval()))
            .collect(),
    );
    let approved = guard::approve_all(tool, ts.approver.as_deref(), info, &plan).await?;
    ran.set_phase(Phase::Approved);
    let caps = ts.limits.effective(info.environment);
    let row_cap = caps.rows_for(requested_rows);
    // The deadline covers the call from here: acquiring the session counts, approvals (a person
    // may take as long as they need) do not.
    let deadline = caps.timeout.map(|t| tokio::time::Instant::now() + t);
    let mut session = match exec::bounded(deadline, ts.source.acquire(&info.id)).await {
        Ok(acquired) => acquired?,
        Err(exec::TimedOut) => {
            return Err(ToolError::Host(
                "no session was available within the time limit; nothing was run".to_owned(),
            ));
        }
    };
    ran.set_phase(Phase::Executing);
    let execution = exec::execute(&mut *session, &plan, deadline, row_cap).await;
    if execution
        .statements
        .iter()
        .any(|s| s.truncated == Some(Truncation::TimeLimit))
    {
        // The deadline has passed: the hand-back gets a short grace, not an open end.
        let _ = tokio::time::timeout(exec::GRACE, ts.source.release(&info.id, session)).await;
    } else {
        ts.source.release(&info.id, session).await;
    }
    let Execution {
        statements,
        not_run,
    } = execution;
    let timeout_ms = caps
        .timeout
        .map(|t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
    let mut rendered = Vec::with_capacity(statements.len());
    for out in statements {
        let Some(planned) = plan.get(out.index) else {
            continue;
        };
        let approved = approved.get(out.index).copied().unwrap_or(false);
        let notice = match out.truncated {
            Some(Truncation::RowLimit) => Some(format!(
                "Truncated at {} rows (row limit).",
                row_cap.unwrap_or(0)
            )),
            Some(Truncation::TimeLimit) => Some(format!(
                "Stopped after {} ms (time limit); the query was cancelled.",
                timeout_ms.unwrap_or(0)
            )),
            _ => None,
        };
        let row_count = out.rows.len();
        ran.push_executed(ExecutedStatement {
            sql: planned.sql.clone(),
            class: planned.class,
            approved,
            rows: row_count as u64,
            truncated: out.truncated,
            failed: out.error.is_some(),
        });
        rendered.push(json!({
            "index": out.index,
            "sql": planned.sql,
            "class": planned.class.as_str(),
            "approved": approved,
            "columns": out.columns.iter().map(cells::column).collect::<Vec<_>>(),
            "rows": out.rows.iter()
                .map(|r| r.0.iter().map(cells::cell).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            "row_count": row_count,
            "rows_affected": out.rows_affected,
            "truncated": out.truncated.map(Truncation::as_str),
            "notice": notice,
            "error": out.error,
        }));
    }
    Ok(json!({
        "connection": info.id,
        "limits": {"max_rows": caps.max_rows, "timeout_ms": timeout_ms},
        "not_run": not_run,
        "statements": rendered,
    }))
}

pub(crate) async fn run_query(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &RunQueryArgs,
    ran: &mut Ran,
) -> Result<Value, ToolError> {
    positive("max_rows", args.max_rows)?;
    run_sql(
        ts,
        crate::schema::names::RUN_QUERY,
        info,
        &args.sql,
        None,
        args.max_rows,
        ran,
    )
    .await
}

pub(crate) fn positive(name: &str, value: Option<u32>) -> Result<(), ToolError> {
    if value == Some(0) {
        return Err(ToolError::InvalidArguments(format!(
            "`{name}` must be at least 1"
        )));
    }
    Ok(())
}

fn dialect_of(info: &ConnectionInfo, tool: &str) -> Result<Dialect, ToolError> {
    info.dialect.ok_or_else(|| {
        ToolError::Unsupported(format!(
            "{tool} is not supported for the {} driver",
            info.driver
        ))
    })
}

pub(crate) async fn sample_rows(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &SampleRowsArgs,
    ran: &mut Ran,
) -> Result<Value, ToolError> {
    positive("limit", args.limit)?;
    let dialect = dialect_of(info, crate::schema::names::SAMPLE_ROWS)?;
    let path = addr_path(args.database.as_deref(), args.schema.as_deref());
    let table = DbObject::new(ObjectKind::Table, path, args.table.as_str());
    let name = qualified_name(dialect, &table)
        .map_err(|e| ToolError::InvalidArguments(format!("bad table name: {e}")))?;
    let caps = ts.limits.effective(info.environment);
    let limit = caps
        .rows_for(Some(args.limit.unwrap_or(DEFAULT_SAMPLE_ROWS)))
        .unwrap_or(DEFAULT_SAMPLE_ROWS);
    let sql = match dialect {
        Dialect::TSql => format!("SELECT TOP ({limit}) * FROM {name}"),
        _ => format!("SELECT * FROM {name} LIMIT {limit}"),
    };
    run_sql(
        ts,
        crate::schema::names::SAMPLE_ROWS,
        info,
        &sql,
        None,
        None,
        ran,
    )
    .await
}

pub(crate) async fn explain_query(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &ExplainArgs,
    ran: &mut Ran,
) -> Result<Value, ToolError> {
    let tool = crate::schema::names::EXPLAIN_QUERY;
    let dialect = dialect_of(info, tool)?;
    if dialect == Dialect::TSql {
        return Err(ToolError::Unsupported(
            "explain_query is not supported for T-SQL".to_owned(),
        ));
    }
    let analyze = args.analyze.unwrap_or(false);
    if guard::starts_with_explain_like(dialect, &args.sql) {
        return Err(ToolError::InvalidArguments(
            "`sql` must be the statement to explain, without EXPLAIN, ANALYZE or a leading `(`; \
             use the `analyze` flag"
                .to_owned(),
        ));
    }
    let statements = guard::plan(info, &args.sql, None)?;
    let [one] = statements.as_slice() else {
        return Err(ToolError::InvalidArguments(
            "explain_query takes exactly one statement".to_owned(),
        ));
    };
    let text = match dialect {
        Dialect::Sqlite if analyze => {
            return Err(ToolError::Unsupported(
                "SQLite has no EXPLAIN ANALYZE".to_owned(),
            ));
        }
        Dialect::Sqlite => format!("EXPLAIN QUERY PLAN {}", one.sql),
        _ if analyze => format!("EXPLAIN ANALYZE {}", one.sql),
        _ => format!("EXPLAIN {}", one.sql),
    };
    let force = analyze.then_some(ApprovalReason::ExplainAnalyze);
    run_sql(ts, tool, info, &text, force, None, ran).await
}

pub(crate) async fn open_in_editor(
    ts: &Inner,
    info: Option<&ConnectionInfo>,
    args: &OpenInEditorArgs,
) -> Result<Value, ToolError> {
    let ui = ts.ui()?;
    ui.open_in_editor(info.map(|i| i.id.as_str()), &args.sql)
        .await?;
    Ok(json!({"opened": true}))
}

pub(crate) async fn open_results_tab(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &OpenResultsTabArgs,
) -> Result<Value, ToolError> {
    let ui = ts.ui()?;
    let plan = guard::plan(info, &args.sql, None)?;
    guard::require_unapproved_free(crate::schema::names::OPEN_RESULTS_TAB, &plan)?;
    ui.open_results_tab(&info.id, &args.sql, args.title.as_deref())
        .await?;
    Ok(json!({"opened": true}))
}
