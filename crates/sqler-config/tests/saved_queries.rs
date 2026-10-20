// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Saved-query store tests on temporary directories: no network, no keychain.

#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::many_single_char_names
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqler_config::{
    ConfigDir, ConfigError, ConnectionId, ConnectionSpec, ConnectionStore, SavedQueries,
    SavedQuery, SavedQueryDraft, SavedQueryError,
};
use sqler_core::DriverId;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Fresh per-test directory (not created), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "sqler-config-{}-queries-{n}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&p);
        Self(p)
    }
    fn dir(&self) -> ConfigDir {
        ConfigDir::new(&self.0)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn conn(name: &str) -> ConnectionId {
    let mut s = ConnectionStore::new();
    s.add_connection(
        ConnectionSpec::new(name, DriverId::new("sqlite").unwrap(), "x.db"),
        None,
    )
    .unwrap()
}

fn draft(name: &str, sql: &str) -> SavedQueryDraft {
    SavedQueryDraft::new(name, sql)
}

#[test]
fn missing_file_is_empty_and_path_is_queries_toml() {
    let s = Scratch::new();
    assert!(s.dir().load_saved_queries().unwrap().queries().is_empty());
    assert_eq!(s.dir().saved_queries_path(), s.0.join("queries.toml"));
}

#[test]
fn create_rename_set_sql_update_delete_list() {
    let mut q = SavedQueries::new();
    let a = q.create(draft("Daily", "select 1")).unwrap();
    let b = q.create(draft("Weekly", "select 2")).unwrap();
    let c = q.create(draft("Monthly", "select 3")).unwrap();
    q.rename(&b, "Weekly report").unwrap();
    q.set_sql(&a, "select 10").unwrap();
    let mut d = draft("Monthly", "select 30");
    d.folder = Some("reports / 2026".into());
    d.tags = vec!["finance".into(), "finance".into(), "kpi".into()];
    d.connection = Some(conn("billing"));
    q.update(&c, d).unwrap();
    let gone = q.delete(&b).unwrap();
    assert_eq!(gone.name(), "Weekly report");
    let names: Vec<_> = q.queries().iter().map(SavedQuery::name).collect();
    assert_eq!(names, ["Daily", "Monthly"]);
    assert_eq!(q.get(&a).unwrap().sql(), "select 10");
    let m = q.get(&c).unwrap();
    assert_eq!(m.folder(), Some("reports/2026"));
    assert_eq!(m.tags(), ["finance", "kpi"]);
    assert_eq!(m.connection().unwrap().as_str(), "billing");
    assert_eq!(q.folders(), ["reports/2026"]);
    // Ids are not reused below the highest one.
    assert_eq!(q.create(draft("New", "")).unwrap().as_str(), "q-4");
    assert!(matches!(
        q.create(draft("Daily", "x")),
        Err(SavedQueryError::DuplicateName(_))
    ));
}

#[test]
fn save_then_load_is_equal_and_leaves_no_tmp() {
    let s = Scratch::new();
    let mut q = SavedQueries::new();
    let mut d = draft("Multi", "select *\nfrom t\nwhere a = 'x'\n");
    d.folder = Some("a/b".into());
    d.tags = vec!["t1".into()];
    d.connection = Some(conn("My DB"));
    q.create(d).unwrap();
    q.create(draft("Plain", "select 1")).unwrap();
    s.dir().save_saved_queries(&q).unwrap();
    assert_eq!(s.dir().load_saved_queries().unwrap(), q);
    assert!(!s.0.join("queries.toml.tmp").exists());
    let text = fs::read_to_string(s.dir().saved_queries_path()).unwrap();
    assert!(text.starts_with("version = 1"), "{text}");
    assert!(text.contains("[[query]]"));
    // Saving twice replaces atomically.
    q.rename(&q.queries()[1].id().clone(), "Renamed").unwrap();
    s.dir().save_saved_queries(&q).unwrap();
    assert_eq!(s.dir().load_saved_queries().unwrap(), q);
    assert!(!s.0.join("queries.toml.tmp").exists());
}

#[test]
fn hand_written_file_loads() {
    let s = Scratch::new();
    fs::create_dir_all(&s.0).unwrap();
    fs::write(
        s.dir().saved_queries_path(),
        r#"version = 1

[[query]]
id = "q-5"
name = "Top customers"
folder = "sales"
connection = "billing"
tags = ["kpi"]
sql = """
select * from customers
order by revenue desc
"""

[[query]]
id = "q-2"
name = "Ping"
sql = "select 1"
"#,
    )
    .unwrap();
    let mut q = s.dir().load_saved_queries().unwrap();
    assert_eq!(q.queries()[0].id().as_str(), "q-5");
    assert_eq!(q.queries()[1].id().as_str(), "q-2");
    assert!(
        q.queries()[0]
            .sql()
            .starts_with("select * from customers\n")
    );
    assert_eq!(q.create(draft("Next", "")).unwrap().as_str(), "q-6");
}

#[test]
fn newer_version_and_corrupt_files_are_typed_errors_and_untouched() {
    let s = Scratch::new();
    fs::create_dir_all(&s.0).unwrap();
    let path = s.dir().saved_queries_path();
    for (text, want_invalid) in [
        ("version = 2\n[[query]]\nid = \"q-1\"\nweird = true\n", true),
        (
            "version = 1\n[[query]]\nid = \"q-1\"\nname = \"a\"\nsql = \"x\"\nbogus = 1\n",
            false,
        ),
        ("this is = = not toml", false),
        ("[[query]]\nid = \"q-1\"\nname = \"a\"\nsql = \"x\"\n", true),
    ] {
        fs::write(&path, text).unwrap();
        let err = s.dir().load_saved_queries().unwrap_err();
        if want_invalid {
            assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
        } else {
            assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), text, "file changed");
    }
    fs::write(&path, "version = 2\n").unwrap();
    let msg = s.dir().load_saved_queries().unwrap_err().to_string();
    assert!(
        msg.contains("newer SQLer (version 2, this build reads up to 1)"),
        "{msg}"
    );
}

#[test]
fn parse_errors_do_not_echo_the_source_line() {
    let s = Scratch::new();
    fs::create_dir_all(&s.0).unwrap();
    fs::write(
        s.dir().saved_queries_path(),
        "version = 1\n[[query]]\nid = \"q-1\"\nname = \"a\"\nsql = \"select 'topsecret'\" junk\n",
    )
    .unwrap();
    let msg = s.dir().load_saved_queries().unwrap_err().to_string();
    assert!(msg.contains("line 5"), "{msg}");
    assert!(!msg.contains("topsecret"), "{msg}");
}

#[test]
fn dangling_connection_binding_is_kept() {
    let s = Scratch::new();
    let mut q = SavedQueries::new();
    let mut d = draft("a", "x");
    d.connection = Some(conn("deleted-since"));
    q.create(d).unwrap();
    s.dir().save_saved_queries(&q).unwrap();
    let back = s.dir().load_saved_queries().unwrap();
    assert_eq!(
        back.queries()[0].connection().unwrap().as_str(),
        "deleted-since"
    );
}
