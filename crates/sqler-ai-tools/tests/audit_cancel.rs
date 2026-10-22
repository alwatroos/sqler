// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A call whose future is dropped still leaves one audit entry, outcome `cancelled` (ADR-0037).
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{ScriptedApprover, SqliteSource, limits};
use serde_json::json;
use sqler_ai_api::{BoxFuture, ToolCall};
use sqler_ai_tools::{
    ApprovalOutcome, ApprovalRequest, Approver, AuditEntry, CallOutcome, ConnectionInfo, Decision,
    MemoryAudit, SessionSource, Surface, ToolError, ToolSet, names,
};
use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::Query;
use sqler_driver_api::{CancelHandle, DriverError, Introspect, QueryStream, Session};

#[derive(Clone, Copy, PartialEq)]
enum Hang {
    /// Never finishes acquiring a session.
    Acquire,
    /// Never finishes executing a statement.
    Execute,
}

/// The SQLite source, except that one step never finishes.
struct HangSource {
    inner: Arc<SqliteSource>,
    hang: Hang,
    entered: AtomicBool,
}

struct HangSession(Box<dyn Session>);

impl Session for HangSession {
    fn cancel_handle(&self) -> CancelHandle {
        self.0.cancel_handle()
    }

    fn execute<'s>(
        &'s mut self,
        _query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(std::future::pending())
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        None
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        self.0.close()
    }
}

impl SessionSource for HangSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        self.inner.connections()
    }

    fn catalog(&self, connection: &str) -> Option<Catalog> {
        self.inner.catalog(connection)
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        Box::pin(async move {
            if self.hang == Hang::Acquire {
                self.entered.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
            }
            let session = self.inner.acquire(connection).await?;
            self.entered.store(true, Ordering::SeqCst);
            Ok(Box::new(HangSession(session)) as Box<dyn Session>)
        })
    }

    fn release(&self, connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        self.inner.release(connection, session)
    }
}

/// An approver that never answers.
struct Silent;

impl Approver for Silent {
    fn approve<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        Box::pin(std::future::pending())
    }
}

const SEED: &[&str] = &["create table creds (id integer primary key, apiKey text, note text)"];

async fn rig(
    hang: Hang,
    approver: Arc<dyn Approver>,
) -> (ToolSet, Arc<MemoryAudit>, Arc<HangSource>) {
    let inner = SqliteSource::seeded("cancel", None, Some(true), SEED).await;
    let source = Arc::new(HangSource {
        inner,
        hang,
        entered: AtomicBool::new(false),
    });
    let audit = Arc::new(MemoryAudit::new());
    let ts = ToolSet::new(source.clone(), limits())
        .with_approver(approver)
        .with_audit(audit.clone(), Surface::Mcp);
    (ts, audit, source)
}

fn sql_call(sql: &str) -> ToolCall {
    ToolCall::new(
        "c1",
        names::RUN_QUERY,
        json!({"connection": "db", "sql": sql}).to_string(),
    )
}

/// Polls the call until it has been waiting for 200 ms, then drops it.
async fn cancel(ts: &ToolSet, call: &ToolCall) {
    let timed = tokio::time::timeout(Duration::from_millis(200), ts.call_detailed(call)).await;
    assert!(timed.is_err(), "the call was expected to hang");
}

fn only(audit: &MemoryAudit) -> AuditEntry {
    let mut entries = audit.entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    entries.pop().unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_mid_execution_records_the_approved_write_as_run() {
    let (ts, audit, source) = rig(Hang::Execute, ScriptedApprover::always(Decision::Approve)).await;
    cancel(
        &ts,
        &sql_call("insert into creds (id) values (1); select 1"),
    )
    .await;
    assert!(source.entered.load(Ordering::SeqCst));
    let e = only(&audit);
    assert_eq!(e.outcome, CallOutcome::Cancelled);
    assert!(e.error.is_none());
    assert_eq!(e.tool, names::RUN_QUERY);
    assert_eq!(e.surface, Surface::Mcp);
    assert_eq!(e.connection.as_ref().unwrap().id, "db");
    assert_eq!(e.approval, ApprovalOutcome::Approved);
    assert_eq!(e.statements.len(), 2);
    assert_eq!(e.statements[0].approval, ApprovalOutcome::Approved);
    assert_eq!(e.statements[1].approval, ApprovalOutcome::NotNeeded);
    assert!(e.statements.iter().all(|s| s.ran), "sent or possibly sent");
    assert_eq!(e.statements[0].class, "write");
    assert_eq!(e.result_bytes, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_after_approval_before_execution_runs_nothing() {
    let (ts, audit, _) = rig(Hang::Acquire, ScriptedApprover::always(Decision::Approve)).await;
    cancel(&ts, &sql_call("insert into creds (id) values (1)")).await;
    let e = only(&audit);
    assert_eq!(e.outcome, CallOutcome::Cancelled);
    assert_eq!(e.statements[0].approval, ApprovalOutcome::Approved);
    assert!(!e.statements[0].ran);
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_before_approval_records_the_plan_unapproved() {
    let (ts, audit, source) = rig(Hang::Execute, Arc::new(Silent)).await;
    cancel(&ts, &sql_call("delete from creds")).await;
    assert!(!source.entered.load(Ordering::SeqCst));
    let e = only(&audit);
    assert_eq!(e.outcome, CallOutcome::Cancelled);
    assert_eq!(e.approval, ApprovalOutcome::NotAsked);
    assert_eq!(e.statements[0].approval, ApprovalOutcome::NotAsked);
    assert!(!e.statements[0].ran);
}

#[tokio::test(flavor = "current_thread")]
async fn a_completed_call_is_not_recorded_twice() {
    let inner = SqliteSource::seeded("cancel-done", None, Some(true), SEED).await;
    let audit = Arc::new(MemoryAudit::new());
    let ts = ToolSet::new(inner, limits())
        .with_approver(ScriptedApprover::always(Decision::Approve))
        .with_audit(audit.clone(), Surface::Mcp);
    let report = ts.call_detailed(&sql_call("select 1")).await;
    assert!(!report.result.is_error);
    drop(report);
    let e = only(&audit);
    assert_eq!(e.outcome, CallOutcome::Ok);
}

#[tokio::test(flavor = "current_thread")]
async fn the_cancelled_entry_is_scrubbed_like_any_other() {
    let (ts, audit, _) = rig(Hang::Execute, ScriptedApprover::always(Decision::Approve)).await;
    let call = ToolCall::new(
        "c1",
        names::RUN_QUERY,
        json!({
            "connection": "db",
            "sql": "update creds set apiKey = 'CANARY-SQL', note = 'Bearer CANARY-TOKEN' \
                    where id = 1",
        })
        .to_string(),
    );
    cancel(&ts, &call).await;
    let e = only(&audit);
    assert_eq!(e.outcome, CallOutcome::Cancelled);
    let text = format!("{e:?}");
    assert!(!text.contains("CANARY"), "{text}");
    assert!(e.statements[0].ran);
}

struct Panicking;

impl sqler_ai_tools::AuditSink for Panicking {
    fn record(&self, _: &AuditEntry) {
        panic!("sink failure");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_panicking_sink_does_not_panic_out_of_the_drop() {
    let (ts, _, _) = rig(Hang::Execute, ScriptedApprover::always(Decision::Approve)).await;
    let ts = ts.with_audit(Arc::new(Panicking), Surface::Mcp);
    cancel(&ts, &sql_call("delete from creds")).await;
}
