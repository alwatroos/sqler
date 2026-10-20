// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end tests of the `sqler-cli` binary.

#![cfg(feature = "driver-sqlite")]
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sqler-cli"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

fn temp_db(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{name}.db"));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn memory_select() {
    let out = cli(&["run", ":memory:", "select 1 as a, 'x' as b, NULL as c"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "a\tb\tc\n1\tx\tNULL\n");
    assert!(stderr(&out).contains("(1 rows)"));
}

#[test]
fn file_database_persists() {
    let db = temp_db("persist");
    let db = db.to_str().unwrap();

    let out = cli(&["run", db, "create table t (id integer, name text)"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let out = cli(&["run", db, "insert into t values (1, 'a'), (2, 'b')"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).contains("(2 rows affected)"));
    assert_eq!(stdout(&out), "");

    let out = cli(&[
        "run",
        "--driver",
        "sqlite",
        db,
        "select * from t order by id",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "id\tname\n1\ta\n2\tb\n");
}

#[test]
fn bad_sql_exits_1() {
    let out = cli(&["run", ":memory:", "selekt nonsense"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("error:"));
    assert_eq!(stdout(&out), "");
}

#[test]
fn unknown_driver_exits_1() {
    let out = cli(&["run", "--driver", "nope", ":memory:", "select 1"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.starts_with("error:"));
    assert!(err.contains("unknown driver"));
}

#[test]
fn no_args_exits_2() {
    let out = cli(&[]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.starts_with("error:"));
    assert!(err.contains("Usage:"));
}

#[test]
fn wrong_arity_exits_2() {
    let out = cli(&["run", ":memory:"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn help_and_version() {
    let out = cli(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("Usage:"));

    let out = cli(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        format!("sqler-cli {}\n", env!("CARGO_PKG_VERSION"))
    );
}
