// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The audit file: round trip, torn tail, rotation, filters, cursors, versions, threads.
#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqler_ai_tools::{
    AUDIT_FILE_NAME, ApprovalOutcome, AuditConnection, AuditCursor, AuditEntry, AuditLog,
    AuditOptions, AuditQuery, AuditSink, AuditStatement, CallOutcome, Surface,
};
use sqler_config::ConfigDir;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temp directory removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("sqler-ai-audit-{tag}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self) -> PathBuf {
        self.0.join(AUDIT_FILE_NAME)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn entry(n: u64, tool: &str, pad: usize) -> AuditEntry {
    let mut e = AuditEntry::default();
    e.id = format!("id-{n}");
    e.ts_ms = 1_000 + n;
    tool.clone_into(&mut e.tool);
    e.surface = Surface::Mcp;
    e.connection = Some(AuditConnection::new("db", "Test DB"));
    let mut s = AuditStatement::default();
    s.sql = format!("select {n} {}", "x".repeat(pad));
    "read".clone_into(&mut s.class);
    e.statements = vec![s];
    e
}

fn ids(log: &AuditLog, q: &AuditQuery) -> Vec<String> {
    log.read(q)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.id)
        .collect()
}

fn small(path: &Path) -> AuditLog {
    AuditLog::open_at(path, AuditOptions::default().with_max_file_bytes(64 * 1024))
}

#[test]
fn append_and_read_round_trip_newest_first() {
    let dir = TempDir::new("rt");
    let log = AuditLog::open(&ConfigDir::new(&dir.0));
    assert_eq!(log.path(), dir.file());
    assert!(!dir.file().exists(), "open does no I/O");
    let mut a = entry(1, "run_query", 0);
    a.error = Some(sqler_ai_tools::AuditFailure::new("driver", "boom"));
    a.outcome = CallOutcome::Error;
    a.approval = ApprovalOutcome::Refused;
    a.arguments = serde_json::json!({"sql": "select 1"});
    log.append(&a).unwrap();
    log.append(&entry(2, "list_tables", 0)).unwrap();
    let page = log.read(&AuditQuery::new()).unwrap();
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].id, "id-2");
    assert_eq!(page.entries[1], a);
    assert!(page.next.is_none());
    assert_eq!((page.skipped_lines, page.newer_lines), (0, 0));
    assert_eq!(fs::read_to_string(dir.file()).unwrap().lines().count(), 2);
}

#[test]
fn a_missing_file_reads_empty_and_the_directory_is_created() {
    let dir = TempDir::new("missing");
    let path = dir.0.join("sub").join("deeper").join(AUDIT_FILE_NAME);
    let log = AuditLog::open_at(&path, AuditOptions::default());
    assert_eq!(log.read(&AuditQuery::new()).unwrap().entries, vec![]);
    log.append(&entry(1, "t", 0)).unwrap();
    assert_eq!(ids(&log, &AuditQuery::new()), ["id-1"]);
}

#[test]
fn a_torn_tail_is_skipped_and_the_next_append_is_readable() {
    let dir = TempDir::new("torn");
    let log = AuditLog::open(&ConfigDir::new(&dir.0));
    log.append(&entry(1, "t", 0)).unwrap();
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.file())
        .unwrap();
    std::io::Write::write_all(&mut f, br#"{"v":1,"id":"half","ts_ms":5,"sur"#).unwrap();
    drop(f);
    log.append(&entry(2, "t", 0)).unwrap();
    let page = log.read(&AuditQuery::new()).unwrap();
    assert_eq!(page.skipped_lines, 1);
    let got: Vec<_> = page.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(got, ["id-2", "id-1"]);
}

#[test]
fn rotation_keeps_current_plus_two_and_reads_across_files() {
    let dir = TempDir::new("rot");
    let log = small(&dir.file());
    for n in 0..120 {
        log.append(&entry(n, "t", 1_000)).unwrap();
    }
    let rotated = |k: u32| dir.0.join(format!("ai-audit.{k}.jsonl"));
    assert!(rotated(1).exists() && rotated(2).exists());
    assert!(!rotated(3).exists());
    assert!(fs::metadata(dir.file()).unwrap().len() <= 64 * 1024);
    let got = ids(&log, &AuditQuery::new());
    let want: Vec<String> = (0..120).rev().map(|n| format!("id-{n}")).collect();
    assert_eq!(got, want);

    // More entries push the oldest out; the three files stay the most.
    for n in 120..400 {
        log.append(&entry(n, "t", 1_000)).unwrap();
    }
    assert!(!rotated(3).exists());
    let got = ids(&log, &AuditQuery::new().with_limit(5_000));
    assert_eq!(got.first().map(String::as_str), Some("id-399"));
    assert!(got.len() < 400 && got.len() > 60, "{}", got.len());
    assert!(
        got.windows(2)
            .all(|w| w[0][3..].parse::<u64>().unwrap() > w[1][3..].parse::<u64>().unwrap())
    );
}

#[test]
fn each_filter() {
    let dir = TempDir::new("filters");
    let log = AuditLog::open(&ConfigDir::new(&dir.0));
    let mut a = entry(1, "run_query", 0);
    a.surface = Surface::Assist;
    a.approval = ApprovalOutcome::Approved;
    let mut b = entry(2, "list_tables", 0);
    b.connection = Some(AuditConnection::new("other", "Billing"));
    b.outcome = CallOutcome::Error;
    b.surface = Surface::Agent;
    let mut c = entry(3, "run_query", 0);
    c.connection = None;
    c.statements[0].sql = "DELETE FROM Orders".into();
    c.approval = ApprovalOutcome::Denied;
    for e in [&a, &b, &c] {
        log.append(e).unwrap();
    }
    let q = AuditQuery::new;
    assert_eq!(ids(&log, &q().with_connection("db")), ["id-1"]);
    assert_eq!(ids(&log, &q().with_connection("other")), ["id-2"]);
    assert_eq!(ids(&log, &q().with_surface(Surface::Agent)), ["id-2"]);
    assert_eq!(ids(&log, &q().with_tool("run_query")), ["id-3", "id-1"]);
    assert_eq!(
        ids(&log, &q().with_approval(ApprovalOutcome::Denied)),
        ["id-3"]
    );
    assert_eq!(ids(&log, &q().with_outcome(CallOutcome::Error)), ["id-2"]);
    assert_eq!(ids(&log, &q().with_since_ms(1_002)), ["id-3", "id-2"]);
    assert_eq!(ids(&log, &q().with_until_ms(1_002)), ["id-2", "id-1"]);
    assert_eq!(ids(&log, &q().with_text("delete from orders")), ["id-3"]);
    assert_eq!(ids(&log, &q().with_text("BILLING")), ["id-2"]);
    assert_eq!(ids(&log, &q().with_text("test db")), ["id-1"]);
    assert_eq!(ids(&log, &q().with_text("LIST_")), ["id-2"]);
    assert_eq!(
        ids(&log, &q().with_tool("run_query").with_connection("db")),
        ["id-1"]
    );
    assert_eq!(
        ids(&log, &q().with_text("nothing like this")),
        Vec::<String>::new()
    );
    assert_eq!(q().with_limit(0).limit, 1);
    assert_eq!(q().with_limit(99_999).limit, 5_000);
    assert_eq!(q().limit, 200);
}

#[test]
fn cursor_paging_across_a_rotation() {
    let dir = TempDir::new("cursor");
    let log = small(&dir.file());
    for n in 0..100 {
        log.append(&entry(n, "t", 1_000)).unwrap();
    }
    assert!(dir.0.join("ai-audit.1.jsonl").exists());
    let mut seen = Vec::new();
    let mut query = AuditQuery::new().with_limit(30);
    loop {
        let page = log.read(&query).unwrap();
        seen.extend(page.entries.iter().map(|e| e.id.clone()));
        match page.next {
            Some(c) => query = query.with_cursor(c),
            None => break,
        }
    }
    let want: Vec<String> = (0..100).rev().map(|n| format!("id-{n}")).collect();
    // The last page is full only if the count divides; every entry is seen once either way.
    assert_eq!(seen, want);
}

#[test]
fn a_cursor_whose_entry_was_rotated_away_falls_back_to_time() {
    let dir = TempDir::new("gone");
    let log = AuditLog::open(&ConfigDir::new(&dir.0));
    for n in 0..5 {
        log.append(&entry(n, "t", 0)).unwrap();
    }
    let got = ids(
        &log,
        &AuditQuery::new().with_cursor(AuditCursor::new(1_003, "id-gone")),
    );
    assert_eq!(got, ["id-2", "id-1", "id-0"]);
}

#[test]
fn a_newer_version_line_is_counted_not_parsed() {
    let dir = TempDir::new("v2");
    let log = AuditLog::open(&ConfigDir::new(&dir.0));
    log.append(&entry(1, "t", 0)).unwrap();
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.file())
        .unwrap();
    std::io::Write::write_all(&mut f, b"{\"v\":2,\"future\":\"CANARY-V2\"}\n\n   \n").unwrap();
    drop(f);
    let page = log.read(&AuditQuery::new()).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!((page.newer_lines, page.skipped_lines), (1, 0));
}

#[test]
fn two_threads_appending_give_four_hundred_parseable_lines() {
    let dir = TempDir::new("threads");
    let log = Arc::new(AuditLog::open(&ConfigDir::new(&dir.0)));
    let handles: Vec<_> = (0..2_u64)
        .map(|t| {
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                for n in 0..200 {
                    log.record(&entry(t * 1_000 + n, "t", 50));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(log.failed_writes(), 0);
    let text = fs::read_to_string(dir.file()).unwrap();
    assert_eq!(text.lines().count(), 400);
    let page = log.read(&AuditQuery::new().with_limit(5_000)).unwrap();
    assert_eq!((page.entries.len(), page.skipped_lines), (400, 0));
    let unique: HashSet<_> = page.entries.iter().map(|e| e.id.clone()).collect();
    assert_eq!(unique.len(), 400);
}

#[test]
fn a_failing_write_is_counted_and_never_panics() {
    let dir = TempDir::new("fail");
    // The "file" is a directory.
    fs::create_dir_all(dir.file()).unwrap();
    let log = AuditLog::open(&ConfigDir::new(&dir.0));
    assert!(log.append(&entry(1, "t", 0)).is_err());
    log.record(&entry(1, "t", 0));
    assert_eq!(log.failed_writes(), 1);
    assert!(log.last_error().is_some());
}

#[test]
fn rotation_options_are_clamped() {
    let o = AuditOptions::default()
        .with_max_file_bytes(1)
        .with_keep_files(0);
    assert_eq!((o.max_file_bytes, o.keep_files), (64 * 1024, 1));
    let o = AuditOptions::default()
        .with_max_file_bytes(u64::MAX)
        .with_keep_files(99);
    assert_eq!((o.max_file_bytes, o.keep_files), (1 << 30, 20));
    assert_eq!(AuditOptions::default().keep_files, 2);
}
