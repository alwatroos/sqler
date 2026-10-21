// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Every call leaves exactly one audit entry, with the right outcome and no secrets (ADR-0037).
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{ScriptedApprover, SqliteSource, call, limits};
use serde_json::{Value, json};
use sqler_ai_api::{BoxFuture, ToolCall};
use sqler_ai_tools::{
    ApprovalOutcome, AuditEntry, AuditLog, AuditOptions, AuditQuery, AuditSink, CallOutcome,
    Decision, MemoryAudit, Surface, ToolError, ToolSet, UiHost, names,
};
use sqler_driver_api::sqler_core::Environment;

const SEED: &[&str] = &[
    "create table creds (id integer primary key, user_password text default 'CANARY-DEF', \
     apiKey text, note text)",
    "insert into creds (id, user_password, apiKey, note) values \
     (1, 'CANARY-CELL', 'CANARY-CELL', 'NOTE-VISIBLE')",
];

struct NoUi;

impl UiHost for NoUi {
    fn open_in_editor<'a>(
        &'a self,
        _: Option<&'a str>,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn open_results_tab<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A unique temp directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "sqler-ai-audit-dispatch-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Rig {
    ts: ToolSet,
    audit: Arc<MemoryAudit>,
}

async fn rig(env: Option<Environment>, approver: Option<Arc<ScriptedApprover>>) -> Rig {
    let source = SqliteSource::seeded("audit", env, Some(true), SEED).await;
    let audit = Arc::new(MemoryAudit::new());
    let mut ts = ToolSet::new(source, limits())
        .with_ui_host(Arc::new(NoUi))
        .with_audit(audit.clone(), Surface::Mcp);
    if let Some(a) = approver {
        ts = ts.with_approver(a);
    }
    Rig { ts, audit }
}

fn run(sql: &str) -> Value {
    json!({"connection": "db", "sql": sql})
}

fn last(r: &Rig) -> AuditEntry {
    r.audit.entries().pop().unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn one_entry_per_call_for_every_tool() {
    let r = rig(None, None).await;
    let mut names_called: Vec<String> = r.ts.definitions().into_iter().map(|d| d.name).collect();
    names_called.push(names::SEARCH_DATA.to_owned());
    names_called.push("zap".to_owned());
    for (i, name) in names_called.iter().enumerate() {
        let args = json!({"connection": "db", "sql": "select 1", "table": "creds",
                          "name": "creds", "query": "creds"});
        let (report, _) = call(&r.ts, name, args).await;
        let entries = r.audit.entries();
        assert_eq!(entries.len(), i + 1, "{name}");
        let e = entries.last().unwrap();
        assert_eq!(&e.tool, name);
        assert_eq!(e.surface, Surface::Mcp);
        assert_eq!(report.audit.id, e.id);
    }
    // invalid JSON arguments
    let report =
        r.ts.call_detailed(&ToolCall::new("x", names::RUN_QUERY, "{not json"))
            .await;
    assert!(report.result.is_error);
    let entries = r.audit.entries();
    assert_eq!(entries.len(), names_called.len() + 1);
    assert_eq!(entries.last().unwrap().outcome, CallOutcome::Error);
    assert_eq!(
        entries.last().unwrap().error.as_ref().unwrap().code,
        "invalid_arguments"
    );
    let ids: HashSet<_> = entries.iter().map(|e| e.id.clone()).collect();
    assert_eq!(ids.len(), entries.len());
}

#[tokio::test(flavor = "current_thread")]
async fn ui_tools_record_the_resolved_connection() {
    let r = rig(None, None).await;
    call(
        &r.ts,
        names::OPEN_IN_EDITOR,
        json!({"connection": "db", "sql": "select 1"}),
    )
    .await;
    assert!(last(&r).connection.is_some());
    call(
        &r.ts,
        names::OPEN_RESULTS_TAB,
        json!({"connection": "db", "sql": "select 1"}),
    )
    .await;
    let e = last(&r);
    assert_eq!(e.tool, names::OPEN_RESULTS_TAB);
    assert!(e.connection.is_some());
    // Without a connection argument the editor entry has none.
    call(&r.ts, names::OPEN_IN_EDITOR, json!({"sql": "select 1"})).await;
    assert!(last(&r).connection.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_read_needs_no_approval() {
    let r = rig(None, None).await;
    let (report, _) = call(&r.ts, names::RUN_QUERY, run("select id from creds")).await;
    let e = last(&r);
    assert_eq!(e.approval, ApprovalOutcome::NotNeeded);
    assert_eq!(e.outcome, CallOutcome::Ok);
    assert_eq!(e.rows, 1);
    assert_eq!(e.statements.len(), 1);
    assert!(e.statements[0].ran);
    assert_eq!(e.statements[0].class, "read");
    assert_eq!(e.connection.as_ref().unwrap().id, "db");
    assert_eq!(e.result_bytes, report.result.content.len() as u64);
}

#[tokio::test(flavor = "current_thread")]
async fn an_approved_write_is_recorded_as_run() {
    let r = rig(None, Some(ScriptedApprover::always(Decision::Approve))).await;
    call(
        &r.ts,
        names::RUN_QUERY,
        run("insert into creds (id) values (5)"),
    )
    .await;
    let e = last(&r);
    assert_eq!(e.approval, ApprovalOutcome::Approved);
    assert!(e.statements[0].ran);
    assert!(!e.statements[0].failed);
    assert_eq!(e.statements[0].approval, ApprovalOutcome::Approved);
}

#[tokio::test(flavor = "current_thread")]
async fn a_denied_write_runs_nothing() {
    let r = rig(None, Some(ScriptedApprover::always(Decision::Deny))).await;
    call(
        &r.ts,
        names::RUN_QUERY,
        run("insert into creds (id) values (5)"),
    )
    .await;
    let e = last(&r);
    assert_eq!(e.approval, ApprovalOutcome::Denied);
    assert_eq!(e.outcome, CallOutcome::Error);
    assert!(e.statements.iter().all(|s| !s.ran));
    assert_eq!(e.rows, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn no_approver_refuses() {
    let r = rig(None, None).await;
    call(&r.ts, names::RUN_QUERY, run("delete from creds")).await;
    let e = last(&r);
    assert_eq!(e.approval, ApprovalOutcome::Refused);
    assert_eq!(e.error.as_ref().unwrap().code, "no_approver");
    assert!(e.statements.iter().all(|s| !s.ran));
}

#[tokio::test(flavor = "current_thread")]
async fn a_prod_write_with_double_approval_is_approved() {
    let approver = ScriptedApprover::always(Decision::Approve);
    let r = rig(Some(Environment::Prod), Some(approver.clone())).await;
    call(
        &r.ts,
        names::RUN_QUERY,
        run("insert into creds (id) values (6)"),
    )
    .await;
    assert_eq!(approver.requests().len(), 2);
    let e = last(&r);
    assert_eq!(e.approval, ApprovalOutcome::Approved);
    assert!(e.statements[0].ran);
}

#[tokio::test(flavor = "current_thread")]
async fn secrets_never_reach_the_audit_file() {
    let dir = TempDir::new();
    let log = Arc::new(AuditLog::open_at(
        dir.0.join("ai-audit.jsonl"),
        AuditOptions::default(),
    ));
    let source = SqliteSource::seeded("audit-file", None, Some(true), SEED).await;
    let ts = ToolSet::new(source, limits())
        .with_approver(ScriptedApprover::always(Decision::Approve))
        .with_audit(log.clone(), Surface::Assist);
    for sql in [
        "select * from creds",
        "select user_password as p from creds",
        "alter user u password 'CANARY-PW'",
    ] {
        call(&ts, names::RUN_QUERY, run(sql)).await;
    }
    call(
        &ts,
        names::DESCRIBE_TABLE,
        json!({"connection": "db", "schema": "main", "name": "creds"}),
    )
    .await;
    let bytes = std::fs::read_to_string(log.path()).unwrap();
    for canary in ["CANARY-CELL", "CANARY-DEF", "NOTE-VISIBLE", "CANARY-PW"] {
        assert!(
            !bytes.contains(canary),
            "{canary} leaked into the audit file"
        );
    }
    assert_eq!(bytes.lines().count(), 4);
    assert_eq!(log.failed_writes(), 0);
    let page = log.read(&AuditQuery::new()).unwrap();
    assert_eq!(page.entries.len(), 4);
    assert_eq!(page.skipped_lines, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_failing_sink_never_fails_the_call() {
    let dir = TempDir::new();
    // The "file" is a directory, so every append fails.
    let log = Arc::new(AuditLog::open_at(dir.0.clone(), AuditOptions::default()));
    let source = SqliteSource::seeded("audit-fail", None, Some(true), SEED).await;
    let ts = ToolSet::new(source, limits()).with_audit(log.clone(), Surface::Session);
    let (report, v) = call(&ts, names::RUN_QUERY, run("select id from creds")).await;
    assert!(!report.result.is_error, "{v}");
    assert_eq!(v["statements"][0]["row_count"], 1);
    assert_eq!(log.failed_writes(), 1);
    assert!(log.last_error().is_some());
}

#[test]
fn the_sink_trait_is_object_safe() {
    fn takes(_: Arc<dyn AuditSink>) {}
    takes(Arc::new(MemoryAudit::new()));
}
