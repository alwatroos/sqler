// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Records the statements of a real in-memory `SQLite` session in a temp-dir history store.
//!
//! The session is handed to the workspace like the app does; the events of the run go through
//! `Workspace::query_event`, and the `RecordHistory` effects are applied to a `HistoryService`.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use sqler_config::{ConfigDir, HistoryOptions, HistoryOutcome};
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};
use sqler_driver_api::{DriverRegistry, Session};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_ui::history::{HISTORY_PAGE, HistorySearch, HistoryService};
use sqler_ui::query::{BufferSnapshot, RunMode};
use sqler_ui::sidebar::SidebarModel;
use sqler_ui::tabs::TabId;
use sqler_ui::workspace::{Effect, Workspace, WorkspaceAction};
use tokio::runtime::{Builder, Runtime};

/// A directory that is removed with the guard.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("sqler-history-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn runtime() -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

async fn session() -> Box<dyn Session> {
    let mut registry = DriverRegistry::new();
    registry.register(Box::new(SqliteDriver::new())).unwrap();
    let config = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), ":memory:");
    registry.connect(&config).await.unwrap()
}

/// A workspace with one open editor tab on a real session.
fn workspace(rt: &Runtime) -> (Workspace, TabId) {
    let driver = DriverId::new(DRIVER_ID).unwrap();
    let mut w = Workspace::new(SidebarModel::from_connections(vec![ConnectionConfig::new(
        "mem", driver, ":memory:",
    )]));
    let Effect::Connect { request, .. } = w.dispatch(WorkspaceAction::OpenConnection(0)) else {
        panic!("expected Connect");
    };
    let s = rt.block_on(session());
    w.connect_finished(request, Ok(s));
    let tab = w.tabs().active().unwrap().id();
    (w, tab)
}

/// Runs `text` to the end and applies every `RecordHistory` effect to `service`.
fn run(rt: &Runtime, w: &mut Workspace, service: &HistoryService, tab: TabId, text: &str) {
    // The grids stay alive until the run ends: dropping a grid cancels its ingestion.
    let Effect::RunQuery {
        job,
        grids,
        mut events,
    } = w.run_query(tab, RunMode::All, BufferSnapshot::at_cursor(text, 0))
    else {
        panic!("expected RunQuery");
    };
    let run = job.run;
    let done = rt.block_on(job.run());
    while let Ok(event) = events.try_recv() {
        if let Effect::RecordHistory(entry) = w.query_event(tab, run, event) {
            service.record(entry).unwrap();
        }
    }
    w.query_finished(done);
    drop(grids);
}

fn all(tab: TabId) -> HistorySearch {
    HistorySearch {
        tab,
        request: 1,
        text: String::new(),
        connection: None,
        limit: HISTORY_PAGE,
    }
}

#[test]
fn every_statement_of_a_script_is_recorded_newest_first() {
    let dir = TempDir::new("script");
    let service = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    let rt = runtime();
    let (mut w, tab) = workspace(&rt);
    run(
        &rt,
        &mut w,
        &service,
        tab,
        "create table t(a); insert into t values (1),(2); select * from t",
    );

    let entries = service.search(&all(tab));
    let sqls: Vec<_> = entries
        .iter()
        .map(sqler_config::HistoryEntry::sql)
        .collect();
    assert_eq!(
        sqls,
        [
            "select * from t",
            "insert into t values (1),(2)",
            "create table t(a)"
        ]
    );
    assert!(entries.iter().all(|e| e.outcome() == HistoryOutcome::Ok));
    assert_eq!(entries[0].rows(), Some(2));
    assert_eq!(entries[1].rows(), Some(2));
    assert!(entries.iter().all(|e| !e.truncated()));
    let id = w.sidebar().entries()[0].id();
    assert!(entries.iter().all(|e| e.connection() == id));

    // The rows read back from the file by a fresh service (as after a restart).
    let again = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    assert_eq!(again.search(&all(tab)).len(), 3);
}

#[test]
fn a_failing_statement_records_an_error_without_its_message_and_stops_the_script() {
    let dir = TempDir::new("error");
    let service = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    let rt = runtime();
    let (mut w, tab) = workspace(&rt);
    run(
        &rt,
        &mut w,
        &service,
        tab,
        "select 1; select * from no_such_table; select 3",
    );
    let entries = service.search(&all(tab));
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].sql(), "select * from no_such_table");
    assert_eq!(entries[0].outcome(), HistoryOutcome::Error);
    assert_eq!(entries[0].rows(), None);
    assert_eq!(entries[1].outcome(), HistoryOutcome::Ok);
    let text = std::fs::read_to_string(dir.0.join("history.jsonl")).unwrap();
    assert!(!text.contains("no such table"), "{text}");
}

#[test]
fn a_credential_statement_is_stored_redacted() {
    let dir = TempDir::new("redact");
    let service = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    let rt = runtime();
    let (mut w, tab) = workspace(&rt);
    // SQLite rejects it; the statement is recorded anyway, and never with the password.
    run(
        &rt,
        &mut w,
        &service,
        tab,
        "ALTER USER x PASSWORD 'hunter2'",
    );
    let entries = service.search(&all(tab));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].sql(), "ALTER USER x PASSWORD '<redacted>'");
    let text = std::fs::read_to_string(dir.0.join("history.jsonl")).unwrap();
    assert!(!text.contains("hunter2"), "{text}");
}
