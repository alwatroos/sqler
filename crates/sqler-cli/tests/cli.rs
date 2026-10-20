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

// ---- export ----

fn tmp(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("export-{name}"));
    let _ = std::fs::remove_file(&path);
    path
}

fn seeded_db(name: &str) -> PathBuf {
    let db = temp_db(name);
    let db_str = db.to_str().unwrap();
    let out = cli(&["run", db_str, "create table t (id integer, name text)"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let out = cli(&["run", db_str, "insert into t values (1, 'a,b'), (2, NULL)"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    db
}

const SELECT_T: &str = "select id, name from t order by id";

fn expected(format: &str) -> &'static str {
    match format {
        "csv" => "id,name\r\n1,\"a,b\"\r\n2,\r\n",
        "markdown" => "| id | name |\n| --- | --- |\n| 1 | a,b |\n| 2 | NULL |\n",
        "json" => "[\n  {\"id\":1,\"name\":\"a,b\"},\n  {\"id\":2,\"name\":null}\n]\n",
        "sql" => "INSERT INTO \"t\" (\"id\", \"name\") VALUES\n  (1, 'a,b'),\n  (2, NULL);\n",
        _ => unreachable!(),
    }
}

/// Compares ignoring whitespace layout, so a writer's exact spacing is not pinned here.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn export_text_formats_to_file_and_stdout() {
    let db = seeded_db("export-formats");
    let db = db.to_str().unwrap();
    for format in ["csv", "json", "sql", "markdown"] {
        let path = tmp(&format!("formats.{format}"));
        let path_str = path.to_str().unwrap();
        let mut args = vec!["export", "--format", format, "--output", path_str];
        if format == "sql" {
            args.extend(["--table", "t"]);
        }
        args.extend([db, SELECT_T]);
        let out = cli(&args);
        assert_eq!(out.status.code(), Some(0), "{format}: {}", stderr(&out));
        assert_eq!(stdout(&out), "");
        assert!(stderr(&out).contains("(2 rows,"), "{}", stderr(&out));
        let file = std::fs::read_to_string(&path).unwrap();

        let mut args = vec!["export", "--format", format, "--output", "-"];
        if format == "sql" {
            args.extend(["--table", "t"]);
        }
        args.extend([db, SELECT_T]);
        let out = cli(&args);
        assert_eq!(out.status.code(), Some(0), "{format}: {}", stderr(&out));
        assert_eq!(stdout(&out), file, "{format}: stdout equals the file");
        assert_eq!(squash(&file), squash(expected(format)), "{format}");
    }
}

#[test]
fn export_flags_accept_the_equals_form() {
    let path = tmp("equals.csv");
    let out = cli(&[
        "export",
        "--format=csv",
        &format!("--output={}", path.to_str().unwrap()),
        ":memory:",
        "select 1 as a",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\r\n1\r\n");
}

#[test]
fn export_usage_errors_exit_2() {
    // Binary format to stdout (or compiled out): usage either way.
    let out = cli(&[
        "export", "--format", "parquet", "--output", "-", ":memory:", "select 1",
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("Usage:"));

    let out = cli(&[
        "export", "--format", "nope", "--output", "-", ":memory:", "select 1",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unknown format"));

    let out = cli(&[
        "export", "--format", "sql", "--output", "-", ":memory:", "select 1",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--table"));

    let out = cli(&[
        "export",
        "--format",
        "csv",
        "--output",
        "-",
        ":memory:",
        "select 1; select 2",
    ]);
    assert_eq!(out.status.code(), Some(2));

    let out = cli(&["export", "--output", "-", ":memory:", "select 1"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--format"));
}

#[test]
fn export_write_gate_refuses_with_exit_3() {
    let db = seeded_db("export-gate");
    let db = db.to_str().unwrap();
    let path = tmp("gate.csv");
    let out = cli(&[
        "export",
        "--format",
        "csv",
        "--output",
        path.to_str().unwrap(),
        db,
        "delete from t",
    ]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("--allow-write"));
    assert!(!path.exists());
    // Nothing ran.
    let out = cli(&["run", db, "select count(*) from t"]);
    assert_eq!(stdout(&out), "count(*)\n2\n");
}

#[test]
fn export_allow_write_runs_and_warns() {
    let db = seeded_db("export-allow");
    let db = db.to_str().unwrap();
    let path = tmp("allow.csv");
    let out = cli(&[
        "export",
        "--allow-write",
        "--format",
        "csv",
        "--output",
        path.to_str().unwrap(),
        db,
        "delete from t where id = 1",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("warning: --allow-write: running a statement of class write: delete from t"),
        "{err}"
    );
    assert!(err.contains("(1 rows affected)"), "{err}");
    assert!(
        !path.exists(),
        "a statement without a result set writes no file"
    );
    let out = cli(&["run", db, "select count(*) from t"]);
    assert_eq!(stdout(&out), "count(*)\n1\n");
}

#[test]
fn export_one_million_rows() {
    let path = tmp("million.csv");
    let out = cli(&[
        "export",
        "--format",
        "csv",
        "--output",
        path.to_str().unwrap(),
        ":memory:",
        "with recursive c as (select 1 as x union all select x + 1 from c where x < 1000000) \
         select x from c",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).contains("(1000000 rows,"), "{}", stderr(&out));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 1_000_001);
}

fn no_partials(path: &std::path::Path) {
    let dir = path.parent().unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap().file_name().into_string().unwrap();
        assert!(
            !(entry.starts_with(&format!(".{name}.")) && entry.ends_with(".partial")),
            "left behind: {entry}"
        );
    }
}

#[test]
fn export_failure_leaves_no_file_and_keeps_an_existing_one() {
    let path = tmp("fail.csv");
    let out = cli(&[
        "export",
        "--format",
        "csv",
        "--output",
        path.to_str().unwrap(),
        ":memory:",
        "select * from no_such_table",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("error:"));
    assert!(!path.exists());
    no_partials(&path);

    std::fs::write(&path, "precious").unwrap();
    let out = cli(&[
        "export",
        "--format",
        "csv",
        "--output",
        path.to_str().unwrap(),
        ":memory:",
        "select * from no_such_table",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious");
    no_partials(&path);
}

#[cfg(unix)]
#[test]
fn export_ctrl_c_removes_the_partial_file() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let path = tmp("cancel.csv");
    let child = Command::new(env!("CARGO_BIN_EXE_sqler-cli"))
        .args([
            "export",
            "--format",
            "csv",
            "--output",
            path.to_str().unwrap(),
            ":memory:",
            "with recursive c as (select 1 as x union all select x + 1 from c) select x from c",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let partial = path.with_file_name(format!(".export-cancel.csv.{}.partial", child.id()));
    let start = Instant::now();
    loop {
        if std::fs::metadata(&partial).is_ok_and(|m| m.len() > 0) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the export never produced output"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = signal_and_wait(child, "-INT");
    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    assert!(stderr(&out).contains("cancelled"));
    assert!(!path.exists());
    assert!(!partial.exists());
}

/// Sends `signal` to `child` and waits for it, killing it (and failing) after 30 s so that a
/// regression does not hang the test run.
#[cfg(unix)]
fn signal_and_wait(mut child: std::process::Child, signal: &str) -> Output {
    use std::time::{Duration, Instant};

    let status = Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("the export did not stop after {signal}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[cfg(unix)]
#[test]
fn export_sigterm_removes_the_partial_file() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let path = tmp("term.csv");
    let child = Command::new(env!("CARGO_BIN_EXE_sqler-cli"))
        .args([
            "export",
            "--format",
            "csv",
            "--output",
            path.to_str().unwrap(),
            ":memory:",
            "with recursive c as (select 1 as x union all select x + 1 from c) select x from c",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let partial = path.with_file_name(format!(".export-term.csv.{}.partial", child.id()));
    let start = Instant::now();
    while !std::fs::metadata(&partial).is_ok_and(|m| m.len() > 0) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the export never produced output"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = signal_and_wait(child, "-TERM");
    assert_eq!(out.status.code(), Some(143), "{}", stderr(&out));
    assert!(stderr(&out).contains("terminated"));
    assert!(!path.exists());
    assert!(!partial.exists());
}

/// A signal at any point after start-up ends the export cleanly or lets it finish, and never
/// leaves a partial file. Before the handlers exist the default action may end the process, but
/// then no file exists yet.
#[cfg(unix)]
#[test]
fn export_early_sigint_never_leaves_a_partial_file() {
    use std::process::Stdio;

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("export-early-sigint");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("early.csv");
    for round in 0..40_u64 {
        let child = Command::new(env!("CARGO_BIN_EXE_sqler-cli"))
            .args([
                "export",
                "--format",
                "csv",
                "--output",
                path.to_str().unwrap(),
                ":memory:",
                "with recursive c as (select 1 as x union all select x + 1 from c limit 200000) \
                 select x from c",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Spread the signal over the start-up: 0 to about 40 ms after the spawn.
        std::thread::sleep(std::time::Duration::from_micros(round * 1000));
        let out = signal_and_wait(child, "-INT");
        let code = out.status.code();
        assert!(
            matches!(code, Some(0 | 130) | None),
            "round {round}: {code:?} {}",
            stderr(&out)
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != "early.csv")
            .collect();
        assert!(names.is_empty(), "round {round}: left behind {names:?}");
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn export_unknown_driver_is_a_runtime_error_not_a_refusal() {
    let out = cli(&[
        "export", "--driver", "nope", "--format", "csv", "--output", "-", "x", "select 1",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("unknown driver"));
    assert!(!stderr(&out).contains("allow-write"));
}

#[test]
fn export_sql_starting_with_a_comment_after_double_dash() {
    let db = seeded_db("dashdash");
    let out = cli(&[
        "export",
        "--format",
        "csv",
        "--output",
        "-",
        "--",
        db.to_str().unwrap(),
        "-- note\nselect id from t order by id",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "id\r\n1\r\n2\r\n");
}

#[cfg(feature = "driver-postgres")]
#[test]
fn export_postgres_failure_does_not_leak_the_password() {
    let path = tmp("pg.csv");
    let out = cli(&[
        "export",
        "--driver",
        "postgres",
        "--format",
        "csv",
        "--output",
        path.to_str().unwrap(),
        "postgres://u:s3cret@127.0.0.1:1/db",
        "select 1",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!stderr(&out).contains("s3cret"), "{}", stderr(&out));
    assert!(!stdout(&out).contains("s3cret"));
    assert!(!path.exists());
}

const LEAK_URL: &str = "postgres://leakuser:s3cretpw@db.leak.example:5432/leakdb";

/// No stderr text may carry any piece of the URL. (Stdout is the export itself: a `--table` name
/// legitimately appears in the generated `INSERT`.)
fn assert_no_leak(args: &[&str]) {
    let out = cli(args);
    let text = stderr(&out);
    for needle in ["s3cretpw", "leakuser", "db.leak.example", "leakdb"] {
        assert!(!text.contains(needle), "{args:?} leaked {needle}: {text}");
    }
}

#[test]
fn export_never_echoes_arguments_in_errors() {
    let dir = tmp("noleak");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("out.csv");
    let file = file.to_str().unwrap();
    let eq_driver = format!("--driver={LEAK_URL}");
    let eq_format = format!("--format={LEAK_URL}");
    let url_file = format!("{LEAK_URL}/x.csv");
    let dash_url = format!("--{LEAK_URL}");
    let base = ["--format", "csv", "--output", "-"];
    let with = |head: &[&str], tail: &[&str]| {
        let mut v = vec!["export"];
        v.extend_from_slice(head);
        v.extend_from_slice(tail);
        assert_no_leak(&v);
    };
    // The URL in the --driver slot, in every form, and a --driver without its value.
    with(
        &["--driver", LEAK_URL],
        &[base[0], base[1], base[2], base[3], ":memory:", "select 1"],
    );
    with(
        &[&eq_driver],
        &[base[0], base[1], base[2], base[3], ":memory:", "select 1"],
    );
    with(&base, &["--driver", LEAK_URL, "select 1"]);
    with(&base, &["--driver"]);
    with(
        &["--driver", "--format", "csv", "--output", "-"],
        &[LEAK_URL, "select 1"],
    );
    // The URL in the other value slots.
    with(
        &["--format", LEAK_URL, "--output", "-"],
        &[":memory:", "select 1"],
    );
    with(&[&eq_format, "--output", "-"], &[":memory:", "select 1"]);
    with(
        &["--format", "sql", "--table", LEAK_URL, "--output", "-"],
        &[":memory:", "select 1"],
    );
    with(
        &[
            "--format", "sql", "--table", "t", "--schema", LEAK_URL, "--output", "-",
        ],
        &[":memory:", "select 1"],
    );
    with(
        &["--format", "csv", "--output", LEAK_URL],
        &[":memory:", "select 1"],
    );
    with(
        &["--format", "csv", "--output", &url_file],
        &[":memory:", "select 1"],
    );
    with(
        &["--format", "csv", "--output", file, "--bogus"],
        &[LEAK_URL],
    );
    with(&[&dash_url, "--format", "csv", "--output", "-"], &[]);
    with(&base, &[":memory:", "select 1", LEAK_URL]);
    // A server URL given to the sqlite driver, and a query that fails.
    with(&base, &[LEAK_URL, "selekt"]);
    // The first word.
    assert_no_leak(&[LEAK_URL]);
    assert_no_leak(&[&dash_url]);
}

#[test]
fn run_never_echoes_arguments_in_errors() {
    let eq_driver = format!("--driver={LEAK_URL}");
    let bad_sql = format!("selekt {LEAK_URL}");
    assert_no_leak(&["run", "--driver", LEAK_URL, ":memory:", "select 1"]);
    assert_no_leak(&["run", &eq_driver, ":memory:", "select 1"]);
    assert_no_leak(&["run", "--driver"]);
    assert_no_leak(&["run", "--driver", "nope", LEAK_URL, "select 1"]);
    assert_no_leak(&["run", LEAK_URL, "select 1"]);
    assert_no_leak(&["run", LEAK_URL]);
    assert_no_leak(&["run", "--bogus", LEAK_URL, "select 1"]);
    assert_no_leak(&["run", ":memory:", "select 1", LEAK_URL]);
    assert_no_leak(&["run", ":memory:", &bad_sql]);
}
