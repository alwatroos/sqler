// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Adversarial inputs through the history flow: nothing sensitive may reach the disk.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use sqler_config::{ConfigDir, HistoryOptions, HistoryOutcome, SavedQueries, SavedQueryDraft};
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};
use sqler_driver_api::{DriverRegistry, Session};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_ui::history::{HISTORY_PAGE, HistorySearch, HistoryService};
use sqler_ui::query::{BufferSnapshot, RunMode};
use sqler_ui::sidebar::SidebarModel;
use sqler_ui::tabs::TabId;
use sqler_ui::workspace::{Effect, Workspace, WorkspaceAction};
use tokio::runtime::{Builder, Runtime};

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("sqler-hadv-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Self(d)
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

fn workspace(rt: &Runtime) -> (Workspace, TabId) {
    let driver = DriverId::new(DRIVER_ID).unwrap();
    let mut w = Workspace::new(SidebarModel::from_connections(vec![ConnectionConfig::new(
        "mem", driver, ":memory:",
    )]));
    let Effect::Connect { request, .. } = w.dispatch(WorkspaceAction::OpenConnection(0)) else {
        panic!("expected Connect");
    };
    w.connect_finished(request, Ok(rt.block_on(session())));
    let tab = w.tabs().active().unwrap().id();
    (w, tab)
}

/// Runs each statement as its own run (a failure stops a script).
fn run(rt: &Runtime, w: &mut Workspace, svc: &HistoryService, tab: TabId, text: &str) {
    let eff = w.run_query(tab, RunMode::All, BufferSnapshot::at_cursor(text, 0));
    let Effect::RunQuery {
        job,
        grids,
        mut events,
    } = eff
    else {
        // SQLite reads `$name` as a parameter and opens a prompt: record directly instead.
        w.dismiss_query_prompt(tab);
        let conn = w.sidebar().entries()[0].id().clone();
        svc.record(sqler_config::NewHistoryEntry {
            sql: text.into(),
            connection: conn,
            started_at: std::time::SystemTime::now(),
            duration: std::time::Duration::from_millis(1),
            outcome: HistoryOutcome::Error,
            rows: None,
        })
        .unwrap();
        return;
    };
    let run = job.run;
    let done = rt.block_on(job.run());
    while let Ok(event) = events.try_recv() {
        if let Effect::RecordHistory(entry) = w.query_event(tab, run, event) {
            svc.record(entry).unwrap();
        }
    }
    w.query_finished(done);
    drop(grids);
}

fn all(tab: TabId, text: &str) -> HistorySearch {
    HistorySearch {
        tab,
        request: 1,
        text: text.into(),
        connection: None,
        limit: HISTORY_PAGE,
    }
}

fn disk(dir: &std::path::Path) -> String {
    let mut out = String::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_file() {
            out.push_str(&String::from_utf8_lossy(&std::fs::read(&p).unwrap()));
        }
    }
    out
}

const SECRETS: &[(&str, &str)] = &[
    ("CREATE USER bob PASSWORD 'S3cretAlpha'", "S3cretAlpha"),
    (
        "ALTER ROLE r WITH LOGIN PASSWORD 'S3cretBeta'",
        "S3cretBeta",
    ),
    (
        "select 'postgres://u:S3cretGamma@host:5432/db'",
        "S3cretGamma",
    ),
    ("select 'mysql://root:S3cretDelta@h/x?ssl=1'", "S3cretDelta"),
    ("CREATE USER c PASSWORD $$S3cretEps$$", "S3cretEps"),
    (
        "CREATE USER d PASSWORD $tag$S3cretZeta; x$tag$",
        "S3cretZeta",
    ),
    ("CREATE USER e IDENTIFIED BY 'S3cretEta'", "S3cretEta"),
    ("select 'api_key=S3cretTheta'", "S3cretTheta"),
    ("select '{\"password\":\"S3cretIota\"}'", "S3cretIota"),
    ("CREATE USER f PASSWORD U&'S3cretKappa'", "S3cretKappa"),
    ("CREATE USER g PASSWORD 'S3cretLambda", "S3cretLambda"),
    ("CREATE USER h PASSWORD 'pä\u{202e}ssS3cretMu'", "S3cretMu"),
];

#[test]
fn secrets_never_reach_the_disk() {
    let dir = TempDir::new("secrets");
    let svc = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    let rt = runtime();
    let (mut w, tab) = workspace(&rt);
    let mut leaks = vec![];
    for (sql, secret) in SECRETS {
        run(&rt, &mut w, &svc, tab, sql);
        let text = disk(&dir.0);
        if text.contains(secret) {
            leaks.push(*sql);
        }
        // searching for the secret finds nothing either
        if !svc.search(&all(tab, secret)).is_empty() {
            leaks.push(*sql);
        }
    }
    assert!(leaks.is_empty(), "LEAKS: {leaks:#?}\n{}", disk(&dir.0));
    for e in svc.search(&all(tab, "")) {
        println!("STORED: {:?} {:?}", e.outcome(), e.sql());
    }
}

#[test]
fn huge_and_unicode_statements() {
    let dir = TempDir::new("huge");
    let svc = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    let rt = runtime();
    let (mut w, tab) = workspace(&rt);
    let big = format!(
        "select '{}' as x, 'PASSWORD ''S3cretHuge'''",
        "é😀".repeat(400_000)
    );
    run(&rt, &mut w, &svc, tab, &big);
    run(&rt, &mut w, &svc, tab, "select '日本語😀' as \"ключ\"");
    let hits = svc.search(&all(tab, "日本語"));
    assert_eq!(hits.len(), 1);
    let entries = svc.search(&all(tab, ""));
    assert!(entries.iter().any(sqler_config::HistoryEntry::truncated));
    assert!(!disk(&dir.0).contains("S3cretHuge"));
    let again = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    assert_eq!(again.search(&all(tab, "")).len(), entries.len());
}

#[test]
fn concurrent_search_record_clear() {
    let dir = TempDir::new("conc");
    let svc = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    let rt = runtime();
    let (mut w, tab) = workspace(&rt);
    run(&rt, &mut w, &svc, tab, "select 1");
    let entry = svc.search(&all(tab, "")).remove(0);
    let conn = entry.connection().clone();
    let mut hs = vec![];
    for i in 0..8 {
        let s = svc.clone();
        let c = conn.clone();
        hs.push(std::thread::spawn(move || {
            for j in 0..100 {
                match (i + j) % 4 {
                    0 => {
                        s.record(sqler_config::NewHistoryEntry {
                            sql: format!("select {i}{j} -- PASSWORD 'S3cretConc'"),
                            connection: c.clone(),
                            started_at: std::time::SystemTime::now(),
                            duration: std::time::Duration::from_millis(1),
                            outcome: HistoryOutcome::Ok,
                            rows: Some(1),
                        })
                        .unwrap();
                    }
                    1 | 2 => {
                        let _ = s.search(&HistorySearch {
                            tab,
                            request: 1,
                            text: "select".into(),
                            connection: None,
                            limit: 50,
                        });
                    }
                    _ if i == 0 => s.clear().unwrap(),
                    _ => {}
                }
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    let text = disk(&dir.0);
    assert!(!text.contains("S3cretConc"));
    // every line on disk parses, a fresh open has no issues
    let fresh = ConfigDir::new(&dir.0)
        .open_history(HistoryOptions::default())
        .unwrap();
    assert!(fresh.issues().is_empty(), "{:?}", fresh.issues());
}

#[test]
fn corrupt_store_files() {
    let dir = TempDir::new("corrupt");
    std::fs::create_dir_all(&dir.0).unwrap();
    std::fs::write(
        dir.0.join("history.jsonl"),
        b"{not json\n\xff\xfe\x00\n{\"sql\":1}\n",
    )
    .unwrap();
    std::fs::write(dir.0.join("queries.toml"), b"[[queries\nname = ").unwrap();
    let svc = HistoryService::new(Some(ConfigDir::new(&dir.0)), HistoryOptions::default());
    let notices = svc.open().unwrap();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(!notices[0].contains("not json"));
    assert!(svc.load_saved().is_err());
    let rt = runtime();
    let (mut w, tab) = workspace(&rt);
    run(&rt, &mut w, &svc, tab, "select 1");
    assert_eq!(svc.search(&all(tab, "")).len(), 1);
    // corrupt lines are kept in the file
    assert!(
        std::fs::read(dir.0.join("history.jsonl"))
            .unwrap()
            .windows(8)
            .any(|x| x == b"{not jso")
    );
    // a directory where the file should be
    let d2 = TempDir::new("corrupt2");
    std::fs::create_dir_all(d2.0.join("history.jsonl")).unwrap();
    let s2 = HistoryService::new(Some(ConfigDir::new(&d2.0)), HistoryOptions::default());
    assert!(s2.open().is_err());
    assert_eq!(s2.search(&all(tab, "")).len(), 0);
}

#[test]
fn saved_queries_clear_and_save_races() {
    let dir = TempDir::new("saved");
    let svc = Arc::new(HistoryService::new(
        Some(ConfigDir::new(&dir.0)),
        HistoryOptions::default(),
    ));
    let mut hs = vec![];
    for t in 0..6u64 {
        let s = svc.clone();
        hs.push(std::thread::spawn(move || {
            for g in 1..50u64 {
                let mut q = SavedQueries::default();
                q.create(SavedQueryDraft::new(format!("q{t}-{g}"), "select 1 -- x"))
                    .unwrap();
                s.save_saved(t * 100 + g, &q).unwrap();
                let _ = s.load_saved();
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    let loaded = svc.load_saved().unwrap();
    assert_eq!(loaded.queries().len(), 1);
    // no temp files left behind
    let names: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
}
