// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Query history store tests on temporary directories: no network, no keychain.

#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::many_single_char_names
)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sqler_config::{
    ConfigDir, ConnectionId, ConnectionSpec, ConnectionStore, DEFAULT_HISTORY_CAP,
    HistoryIssueKind, HistoryOptions, HistoryOutcome, HistoryQuery, HistoryStore,
    MAX_HISTORY_SQL_BYTES, NewHistoryEntry,
};
use sqler_core::DriverId;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Fresh per-test directory (not created), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "sqler-config-{}-history-{n}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&p);
        Self(p)
    }
    fn dir(&self) -> ConfigDir {
        ConfigDir::new(&self.0)
    }
    fn file(&self) -> PathBuf {
        self.dir().history_path()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Real ids come from the connection store, as in the app.
fn conn(name: &str) -> ConnectionId {
    let mut s = ConnectionStore::new();
    s.add_connection(
        ConnectionSpec::new(name, DriverId::new("sqlite").unwrap(), "x.db"),
        None,
    )
    .unwrap()
}

fn new(sql: &str, c: &ConnectionId, n: u64) -> NewHistoryEntry {
    NewHistoryEntry {
        sql: sql.to_owned(),
        connection: c.clone(),
        started_at: UNIX_EPOCH + Duration::from_millis(1_760_950_000_000 + n),
        duration: Duration::from_millis(n % 1000),
        outcome: HistoryOutcome::Ok,
        rows: Some(n),
    }
}

fn open(s: &Scratch, max: usize) -> HistoryStore {
    s.dir()
        .open_history(HistoryOptions {
            enabled: true,
            max_entries: max,
        })
        .unwrap()
}

fn all(store: &HistoryStore) -> Vec<sqler_config::HistoryEntry> {
    store.search(&HistoryQuery::default())
}

#[test]
fn missing_file_is_empty_and_open_does_not_create_it() {
    let s = Scratch::new();
    let h = open(&s, 10);
    assert!(h.is_empty());
    assert!(h.issues().is_empty());
    assert_eq!(h.options().max_entries, 10);
    assert!(!s.file().exists());
}

#[test]
fn default_options() {
    let o = HistoryOptions::default();
    assert!(o.enabled);
    assert_eq!(o.max_entries, DEFAULT_HISTORY_CAP);
    assert_eq!(DEFAULT_HISTORY_CAP, 10_000);
}

#[test]
fn max_entries_is_clamped() {
    let s = Scratch::new();
    assert_eq!(open(&s, 0).options().max_entries, 1);
    assert_eq!(open(&s, usize::MAX).options().max_entries, 100_000);
}

#[test]
fn every_field_round_trips() {
    let s = Scratch::new();
    let c = conn("Billing");
    let h = open(&s, 100);
    let mut e = new("select 1\nfrom \"t\"  -- é€", &c, 7);
    e.outcome = HistoryOutcome::Cancelled;
    e.rows = None;
    assert!(h.record(e.clone()).unwrap());
    let mut f = new("insert", &c, 8);
    f.outcome = HistoryOutcome::Error;
    assert!(h.record(f).unwrap());
    drop(h);

    let h = open(&s, 100);
    assert!(h.issues().is_empty());
    let got = h.search(&HistoryQuery {
        text: "SELECT",
        ..HistoryQuery::default()
    });
    assert_eq!(got.len(), 1);
    let g = &got[0];
    assert_eq!(g.sql(), e.sql);
    assert_eq!(g.connection(), &c);
    assert_eq!(g.started_at(), e.started_at);
    assert_eq!(g.duration(), e.duration);
    assert_eq!(g.outcome(), HistoryOutcome::Cancelled);
    assert_eq!(g.rows(), None);
    assert!(!g.truncated());
    let line = fs::read_to_string(s.file()).unwrap();
    assert!(line.starts_with(r#"{"v":1,"started_ms":1760950000007,"connection":"billing","duration_ms":7,"outcome":"cancelled","sql":"#));
    assert!(line.contains(r#""outcome":"error""#) && line.contains(r#""rows":8"#));
}

#[test]
fn long_sql_is_cut_and_flagged() {
    let s = Scratch::new();
    let c = conn("a");
    let h = open(&s, 10);
    h.record(new(&"é".repeat(MAX_HISTORY_SQL_BYTES), &c, 1))
        .unwrap();
    let e = &all(&h)[0];
    assert!(e.truncated());
    assert!(e.sql().len() <= MAX_HISTORY_SQL_BYTES);
    drop(h);
    let e = &all(&open(&s, 10))[0];
    assert!(e.truncated());
}

#[test]
fn disabled_records_nothing() {
    let s = Scratch::new();
    let c = conn("a");
    let h = open(&s, 10);
    h.set_options(HistoryOptions {
        enabled: false,
        max_entries: 10,
    });
    assert!(!h.record(new("select 1", &c, 1)).unwrap());
    assert!(h.is_empty());
    assert!(!s.file().exists());
    h.set_options(HistoryOptions::default());
    assert!(h.record(new("select 2", &c, 2)).unwrap());
    assert_eq!(h.len(), 1);
}

#[test]
fn redaction_is_applied_on_record_and_in_the_file() {
    let s = Scratch::new();
    let c = conn("a");
    let h = open(&s, 10);
    h.record(new("ALTER USER bob PASSWORD 'hunter2'", &c, 1))
        .unwrap();
    h.record(new("select 'plain'", &c, 2)).unwrap();
    assert_eq!(all(&h)[1].sql(), "ALTER USER bob PASSWORD '<redacted>'");
    assert_eq!(all(&h)[0].sql(), "select 'plain'");
    let text = fs::read_to_string(s.file()).unwrap();
    assert!(!text.contains("hunter2"));
}

#[test]
fn corrupt_and_torn_lines_are_reported_kept_and_not_glued() {
    let s = Scratch::new();
    let c = conn("a");
    let good = |sql: &str, n: u64| {
        let h = HistoryStore::open(s.file(), HistoryOptions::default()).unwrap();
        h.record(new(sql, &c, n)).unwrap();
    };
    good("select 1", 1);
    let mut bytes = fs::read(s.file()).unwrap();
    bytes.extend_from_slice(b"not json at all\n");
    bytes.extend_from_slice(b"\xff\xfe\n");
    bytes.extend_from_slice(b"   \n");
    bytes.extend_from_slice(br#"{"v":2,"future":true}"#);
    bytes.push(b'\n');
    let tail = b"{\"v\":1,\"started_ms\":1,\"conn";
    fs::write(s.file(), &bytes).unwrap();
    good("select 2", 2);
    let mut with_torn = fs::read(s.file()).unwrap();
    with_torn.extend_from_slice(tail);
    fs::write(s.file(), &with_torn).unwrap();

    let h = open(&s, 3);
    assert_eq!(h.len(), 2);
    let issues = h.issues();
    let lines: Vec<usize> = issues.iter().map(|i| i.line).collect();
    assert_eq!(lines, [2, 3, 5, 7]);
    assert!(matches!(issues[0].kind, HistoryIssueKind::Malformed { .. }));
    assert!(matches!(issues[1].kind, HistoryIssueKind::Malformed { .. }));
    assert_eq!(
        issues[2].kind,
        HistoryIssueKind::NewerVersion { version: 2 }
    );
    assert!(matches!(issues[3].kind, HistoryIssueKind::Malformed { .. }));
    // Loading does not rewrite.
    assert_eq!(fs::read(s.file()).unwrap(), with_torn);

    // The next append ends the torn line first.
    h.record(new("select 3", &c, 3)).unwrap();
    let after = fs::read(s.file()).unwrap();
    assert!(after.starts_with(&with_torn));
    assert_eq!(after[with_torn.len()], b'\n');
    let h2 = open(&s, 100);
    assert_eq!(h2.len(), 3);
    assert_eq!(h2.issues().len(), 4);

    // Compaction (cap 3 + slack 100 reached) keeps every bad line byte for byte.
    let h3 = open(&s, 1);
    for n in 0..150 {
        h3.record(new("select x", &c, 10 + n)).unwrap();
    }
    let fin = fs::read(s.file()).unwrap();
    let contains = |needle: &[u8]| fin.windows(needle.len()).any(|w| w == needle);
    assert!(contains(b"not json at all\n"));
    assert!(contains(b"\xff\xfe\n"));
    assert!(contains(br#"{"v":2,"future":true}"#));
    assert!(contains(tail));
    assert!(!contains(b"select 1"));
    let h4 = open(&s, 5);
    assert_eq!(h4.len(), 5);
    assert_eq!(h4.issues().len(), 4);
}

#[test]
fn issue_messages_never_echo_the_line() {
    let s = Scratch::new();
    fs::create_dir_all(&s.0).unwrap();
    fs::write(s.file(), b"{\"v\":1,\"sql\":\"topsecret\" oops\n").unwrap();
    let h = open(&s, 10);
    let msg = h.issues()[0].to_string();
    assert!(msg.starts_with("line 1: "), "{msg}");
    assert!(!msg.contains("topsecret"));
}

#[test]
fn cap_is_enforced_in_memory_and_on_disk() {
    let s = Scratch::new();
    let c = conn("a");
    let h = open(&s, 50);
    for n in 0..200 {
        h.record(new(&format!("select {n}"), &c, n)).unwrap();
    }
    assert_eq!(h.len(), 50);
    let top = h.search(&HistoryQuery {
        limit: 1,
        ..HistoryQuery::default()
    });
    assert_eq!(top[0].sql(), "select 199");
    // 200 > 50 + 100 compacted once at 151 records, so the file is well under 200 lines.
    let lines = fs::read_to_string(s.file()).unwrap().lines().count();
    assert!((50..200).contains(&lines), "{lines}");
    assert!(!s.dir().root().join("history.jsonl.tmp").exists());
    drop(h);
    let h = open(&s, 50);
    assert_eq!(h.len(), 50);
    assert_eq!(all(&h)[0].sql(), "select 199");
    assert_eq!(all(&h)[49].sql(), "select 150");
}

#[test]
fn lowering_the_cap_trims_memory_at_once() {
    let s = Scratch::new();
    let c = conn("a");
    let h = open(&s, 100);
    for n in 0..20 {
        h.record(new(&format!("q{n}"), &c, n)).unwrap();
    }
    h.set_options(HistoryOptions {
        enabled: true,
        max_entries: 5,
    });
    assert_eq!(h.len(), 5);
    assert_eq!(all(&h)[0].sql(), "q19");
}

#[test]
fn clear_and_clear_connection() {
    let s = Scratch::new();
    let (a, b) = (conn("a"), conn("b"));
    let h = open(&s, 100);
    for n in 0..6 {
        h.record(new("select", if n % 3 == 0 { &a } else { &b }, n))
            .unwrap();
    }
    assert_eq!(h.clear_connection(&a).unwrap(), 2);
    assert_eq!(h.len(), 4);
    assert!(all(&h).iter().all(|e| e.connection() == &b));
    assert_eq!(h.clear_connection(&a).unwrap(), 0);
    drop(h);
    let h = open(&s, 100);
    assert_eq!(h.len(), 4);

    // `clear` also drops kept bad lines.
    let mut bytes = fs::read(s.file()).unwrap();
    bytes.extend_from_slice(b"garbage\n");
    fs::write(s.file(), bytes).unwrap();
    let h = open(&s, 100);
    assert_eq!(h.issues().len(), 1);
    h.clear_connection(&b).unwrap();
    assert!(fs::read_to_string(s.file()).unwrap().contains("garbage"));
    h.clear().unwrap();
    assert!(h.is_empty() && h.issues().is_empty());
    assert_eq!(fs::read(s.file()).unwrap(), b"");
    assert!(open(&s, 100).issues().is_empty());
    h.record(new("select", &a, 1)).unwrap();
    assert_eq!(open(&s, 100).len(), 1);
}

#[test]
fn search_and_terms_case_insensitive_filter_order_limit() {
    let s = Scratch::new();
    let (a, b) = (conn("a"), conn("b"));
    let h = open(&s, 100);
    h.record(new("SELECT name FROM users", &a, 1)).unwrap();
    h.record(new("select id from orders", &b, 2)).unwrap();
    h.record(new("select NAME, id from Users", &b, 3)).unwrap();
    let q = |text, connection, limit| HistoryQuery {
        text,
        connection,
        limit,
    };
    let sqls = |v: Vec<sqler_config::HistoryEntry>| {
        v.iter().map(|e| e.sql().to_owned()).collect::<Vec<_>>()
    };
    assert_eq!(
        sqls(h.search(&q("USERS name", None, usize::MAX))),
        ["select NAME, id from Users", "SELECT name FROM users"]
    );
    assert_eq!(h.search(&q("  ", None, usize::MAX)).len(), 3);
    assert_eq!(h.search(&q("users orders", None, usize::MAX)).len(), 0);
    assert_eq!(
        sqls(h.search(&q("id", Some(&b), 1))),
        ["select NAME, id from Users"]
    );
    assert_eq!(h.search(&q("select", Some(&a), usize::MAX)).len(), 1);
    assert_eq!(h.search(&q("select", None, 0)).len(), 0);
}

#[test]
fn concurrent_records_all_land() {
    let s = Scratch::new();
    let c = conn("a");
    let h = Arc::new(open(&s, 100_000));
    let handles: Vec<_> = (0..8_u64)
        .map(|t| {
            let (h, c) = (Arc::clone(&h), c.clone());
            std::thread::spawn(move || {
                for i in 0..200 {
                    h.record(new(&format!("select {t} /* {i} */"), &c, t * 1000 + i))
                        .unwrap();
                }
            })
        })
        .collect();
    for t in handles {
        t.join().unwrap();
    }
    assert_eq!(h.len(), 1600);
    drop(h);
    let h = open(&s, 100_000);
    assert_eq!(h.len(), 1600);
    assert!(h.issues().is_empty());
}

#[test]
fn search_is_fast_at_ten_thousand_entries() {
    let s = Scratch::new();
    let c = conn("a");
    let h = open(&s, 10_000);
    let pad = "x".repeat(150);
    for n in 0..10_000_u64 {
        h.record(new(
            &format!("select {n} from t_{n} where c = '{pad}'"),
            &c,
            n,
        ))
        .unwrap();
    }
    assert_eq!(h.len(), 10_000);
    drop(h);
    let h = open(&s, 10_000);
    let start = Instant::now();
    let hits = h.search(&HistoryQuery {
        text: "T_9999 SELECT",
        ..HistoryQuery::default()
    });
    let took = start.elapsed();
    assert_eq!(hits.len(), 1);
    assert!(took < Duration::from_millis(250), "{took:?}");
}

#[test]
fn append_failure_keeps_the_entry_in_memory_and_reports_write() {
    let s = Scratch::new();
    let c = conn("a");
    let h = open(&s, 10);
    // A directory where the file should be makes the append fail.
    fs::create_dir_all(s.file()).unwrap();
    let err = h.record(new("select 1", &c, 1)).unwrap_err();
    assert!(
        matches!(err, sqler_config::ConfigError::Write { .. }),
        "{err}"
    );
    assert_eq!(h.len(), 1);
}

#[test]
fn unreadable_history_is_a_read_error() {
    let s = Scratch::new();
    fs::create_dir_all(s.file()).unwrap();
    let err = s.dir().open_history(HistoryOptions::default()).unwrap_err();
    assert!(
        matches!(err, sqler_config::ConfigError::Read { .. }),
        "{err}"
    );
}

#[test]
fn times_before_1970_are_stored_as_zero() {
    let s = Scratch::new();
    let c = conn("a");
    let h = open(&s, 10);
    let mut e = new("select 1", &c, 1);
    e.started_at = SystemTime::UNIX_EPOCH - Duration::from_secs(10);
    h.record(e).unwrap();
    assert_eq!(all(&h)[0].started_at(), UNIX_EPOCH);
}

#[test]
fn store_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<HistoryStore>();
}
