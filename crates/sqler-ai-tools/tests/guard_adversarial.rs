// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Adversarial inputs against the guard, on dialect-bearing and dialect-less drivers, with a
//! recording session that never touches a database.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{ScriptedApprover, call, limits};
use serde_json::{Value, json};
use sqler_ai_api::BoxFuture;
use sqler_ai_tools::{ConnectionInfo, Decision, SessionSource, ToolError, ToolSet, names};
use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::{DriverId, Environment, Query};
use sqler_driver_api::{
    CancelFlag, CancelHandle, DriverError, Introspect, QueryEvent, QueryStream, Session,
};

/// Yields `Done` at once.
struct Instant;

impl QueryStream for Instant {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(std::future::ready(Some(Ok(QueryEvent::Done {
            rows_affected: Some(0),
        }))))
    }
}

struct Rec(Arc<Mutex<Vec<String>>>);

impl Session for Rec {
    fn cancel_handle(&self) -> CancelHandle {
        CancelFlag::new().handle()
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        self.0.lock().unwrap().push(query.sql.clone());
        Box::pin(std::future::ready(Ok(
            Box::new(Instant) as Box<dyn QueryStream>
        )))
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        None
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

struct Source {
    info: ConnectionInfo,
    log: Arc<Mutex<Vec<String>>>,
    acquires: AtomicUsize,
}

impl SessionSource for Source {
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![self.info.clone()]
    }
    fn catalog(&self, _: &str) -> Option<Catalog> {
        None
    }
    fn acquire<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        self.acquires.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(
            Box::new(Rec(self.log.clone())) as Box<dyn Session>
        )))
    }
}

fn rig(driver: &str, env: Option<Environment>) -> (Arc<Source>, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let info = ConnectionInfo::new("db", "db", DriverId::new(driver).unwrap(), env);
    (
        Arc::new(Source {
            info,
            log: log.clone(),
            acquires: AtomicUsize::new(0),
        }),
        log,
    )
}

/// Runs `sql` with a denying approver; returns (asked, executed).
async fn deny_run(driver: &str, tool: &str, args: Value) -> (usize, Vec<String>, Value) {
    let (src, log) = rig(driver, None);
    let approver = ScriptedApprover::always(Decision::Deny);
    let ts = ToolSet::new(src, limits()).with_approver(approver.clone());
    let (_, v) = call(&ts, tool, args).await;
    let asked = approver.requests().len();
    let ran = log.lock().unwrap().clone();
    (asked, ran, v)
}

fn q(sql: &str) -> Value {
    json!({"connection": "db", "sql": sql})
}

/// Every one of these hides or is a write; with a denying approver nothing may reach the driver.
#[tokio::test(flavor = "current_thread")]
async fn hidden_writes_never_run_unapproved() {
    let common: &[&str] = &[
        "select 1; delete from t",
        "select 1;delete from t",
        "select 1 -- ;\n; delete from t",
        "select 1 /* c */; /* d */ delete from t",
        "/* x */ delete from t",
        "-- c\ndelete from t",
        "select 1;\n\n\t  DeLeTe from t",
        "select 1; ; ; drop table t",
        "select ';' ; delete from t",
        "select 1\u{a0};delete from t",
        "select 1\u{feff};delete from t",
        "select 1;\u{2028}delete from t",
        "select 'é' ; drop table t",
        "select 1; update t set a = 1",
        "select 1; insert into t values (1)",
        "select 1; truncate t",
        "select 1; grant all on t to x",
        "select 1 /* unterminated ; delete from t",
        "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
        "with d as (update t set a=1 returning *) select * from d",
        "with d as (insert into t values (1) returning *) select * from d",
        "with a as (select 1) delete from t",
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) DELETE FROM t",
        "EXPLAIN ANALYZE DELETE FROM t",
        "explain (analyze) delete from t",
        "EXPLAIN (ANALYZE, BUFFERS) INSERT INTO t VALUES (1)",
        "explain analyze verbose update t set a = 1",
        "SELECT * INTO newt FROM t",
        "select a into newt from t",
        "COPY t TO PROGRAM 'rm -rf /'",
        "copy (select 1) to '/tmp/x'",
        "COPY t FROM STDIN",
        "CALL do_stuff()",
        "call p(1); select 1",
        "DO $$ begin delete from t; end $$",
        "select $$ ; $$; delete from t",
        "select $a$ x $a$; drop table t",
        "select $$ delete $$ ; delete from t",
        // (nested block comments are Postgres-only; see the dedicated test)
        "select 'a\\' ; delete from t; --'",
        "select \"a;\" ; delete from t",
        "prepare p as delete from t",
        "execute p",
        "set role admin",
        "lock table t",
        "vacuum",
        "refresh materialized view v",
        "merge into t using s on true when matched then delete",
        "replace into t values (1)",
        "select 1 into outfile '/tmp/x'",
        "load data infile '/x' into table t",
        "handler t open",
        "alter table t add column c int",
        "create table x as select 1",
        "attach database 'x' as y",
        "pragma writable_schema = 1",
        "begin; delete from t; commit",
    ];
    for driver in ["postgres", "mysql", "sqlite"] {
        for sql in common {
            let (asked, ran, v) = deny_run(driver, names::RUN_QUERY, q(sql)).await;
            // The leading read of a hidden-write script must not run either.
            assert!(
                ran.is_empty(),
                "[{driver}] reached the driver unapproved: {sql:?} -> {ran:?} ({v})"
            );
            assert!(asked >= 1, "[{driver}] approver never asked: {sql:?}");
        }
    }
    // No dialect: everything, even a select, is one unknown statement and needs approval.
    for driver in ["mssql", "duckdb"] {
        for sql in common.iter().chain(&["select 1", "select 1; select 2"]) {
            let (asked, ran, _) = deny_run(driver, names::RUN_QUERY, q(sql)).await;
            assert!(ran.is_empty(), "[{driver}] {sql:?} ran unapproved");
            assert_eq!(asked, 1, "[{driver}] {sql:?}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn mysql_executable_comments_and_hash_comments() {
    let cases = [
        "/*!50000 delete from t */",
        "select 1 /*!; delete from t */",
        "select /*! 1; delete from t ; */ 1",
        "select 1 # ;\n; delete from t",
        "select `;` ; delete from t",
        "/*M!100100 delete from t */",
    ];
    for sql in cases {
        let (asked, ran, v) = deny_run("mysql", names::RUN_QUERY, q(sql)).await;
        assert!(
            ran.is_empty() && (asked >= 1 || v["error"]["code"] == "invalid_arguments"),
            "mysql bypass: {sql:?} asked={asked} ran={ran:?} {v}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explain_tool_cannot_smuggle_a_write() {
    let cases = [
        ("postgres", "delete from t", false),
        ("postgres", "delete from t", true),
        ("postgres", "select 1", true),
        ("postgres", "select 1; delete from t", false),
        ("postgres", "select 1; delete from t", true),
        ("postgres", "analyze delete from t", false),
        ("mysql", "delete from t", false),
        ("mysql", "update t set a=1", true),
        ("mysql", "select 1", true),
        ("sqlite", "delete from t", false),
        ("sqlite", "select 1; delete from t", false),
    ];
    for (driver, sql, analyze) in cases {
        let (asked, ran, v) = deny_run(
            driver,
            names::EXPLAIN_QUERY,
            json!({"connection": "db", "sql": sql, "analyze": analyze}),
        )
        .await;
        assert!(
            ran.is_empty(),
            "[{driver}] explain({sql:?}, {analyze}) ran {ran:?}"
        );
        let _ = (asked, v);
    }
    // analyze of a plain select must ask even though a select is a read
    let (asked, ran, _) = deny_run(
        "postgres",
        names::EXPLAIN_QUERY,
        json!({"connection": "db", "sql": "select 1", "analyze": true}),
    )
    .await;
    assert_eq!((asked, ran.len()), (1, 0));
    // mssql and duckdb have no explain
    for d in ["mssql", "duckdb"] {
        let (asked, ran, v) = deny_run(
            d,
            names::EXPLAIN_QUERY,
            json!({"connection": "db", "sql": "select 1"}),
        )
        .await;
        assert_eq!((asked, ran.len()), (0, 0), "{v}");
        assert_eq!(v["error"]["code"], "unsupported");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn sample_rows_hostile_names_stay_one_quoted_identifier() {
    for driver in ["postgres", "mysql", "sqlite"] {
        for table in [
            "t; delete from t",
            "t\"; delete from t; --",
            "t`; delete from t; --",
            "t$$; delete from t; $$",
            "a\"\"b",
            "t\u{0}x",
        ] {
            let (src, log) = rig(driver, None);
            let ts =
                ToolSet::new(src, limits()).with_approver(ScriptedApprover::always(Decision::Deny));
            let (_, v) = call(
                &ts,
                names::SAMPLE_ROWS,
                json!({"connection": "db", "table": table, "schema": "s\"; drop table x; --"}),
            )
            .await;
            let ran = log.lock().unwrap().clone();
            // Either rejected or one statement that classifies as a plain read (no approval asked).
            assert!(ran.len() <= 1, "[{driver}] {table:?}: {ran:?} {v}");
            if let Some(sql) = ran.first() {
                assert!(sql.starts_with("SELECT * FROM "), "{sql}");
                assert!(
                    !sql.contains("; delete") || sql.contains('"') || sql.contains('`'),
                    "{sql}"
                );
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prod_double_approval_each_statement_in_order_and_error_paths() {
    // Both stages asked per statement, in statement order, before anything runs.
    let (src, log) = rig("postgres", Some(Environment::Prod));
    let approver = ScriptedApprover::always(Decision::Approve);
    let ts = ToolSet::new(src, limits()).with_approver(approver.clone());
    let (_, v) = call(
        &ts,
        names::RUN_QUERY,
        q("select 1; delete from a; drop table b"),
    )
    .await;
    let seen: Vec<_> = approver
        .requests()
        .iter()
        .map(|r| (r.index, format!("{:?}", r.stage)))
        .collect();
    assert_eq!(
        seen,
        [
            (1, "Confirm".to_owned()),
            (1, "ProdSecond".to_owned()),
            (2, "Confirm".to_owned()),
            (2, "ProdSecond".to_owned())
        ],
        "{v}"
    );
    assert_eq!(log.lock().unwrap().len(), 3);

    // Denying the first stage never reaches the second, and a deny on statement 3's second stage
    // runs nothing at all (not even statement 1 or 2).
    let (src, log) = rig("postgres", Some(Environment::Prod));
    let approver = ScriptedApprover::new(|r| {
        if r.index == 2 && format!("{:?}", r.stage) == "ProdSecond" {
            Decision::Deny
        } else {
            Decision::Approve
        }
    });
    let ts = ToolSet::new(src, limits()).with_approver(approver.clone());
    let (report, v) = call(
        &ts,
        names::RUN_QUERY,
        q("select 1; delete from a; drop table b"),
    )
    .await;
    assert!(report.result.is_error, "{v}");
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(approver.requests().len(), 4);

    // Read-only beats approval, even on prod.
    let (src, log) = rig("postgres", Some(Environment::Prod));
    let ro = Arc::new(RoWrap(src));
    let approver = ScriptedApprover::always(Decision::Approve);
    let ts = ToolSet::new(ro, limits()).with_approver(approver.clone());
    let (_, v) = call(&ts, names::RUN_QUERY, q("select 1; delete from a")).await;
    assert_eq!(v["error"]["code"], "read_only");
    assert!(log.lock().unwrap().is_empty() && approver.requests().is_empty());
}

struct RoWrap(Arc<Source>);

impl SessionSource for RoWrap {
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![self.0.info.clone().with_read_only(true)]
    }
    fn catalog(&self, c: &str) -> Option<Catalog> {
        self.0.catalog(c)
    }
    fn acquire<'a>(&'a self, c: &'a str) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        self.0.acquire(c)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_driver_and_unknown_connection() {
    // An unknown driver id has no dialect: one unknown statement, approval required.
    let (asked, ran, _) = deny_run("frobdb", names::RUN_QUERY, q("select 1")).await;
    assert_eq!((asked, ran.len()), (1, 0));
    let (src, _) = rig("postgres", None);
    let ts = ToolSet::new(src, limits());
    let (_, v) = call(
        &ts,
        names::RUN_QUERY,
        json!({"connection": "nope", "sql": "select 1"}),
    )
    .await;
    assert_eq!(v["error"]["code"], "unknown_connection");
    let (_, v) = call(&ts, "drop_everything", json!({})).await;
    assert!(v["error"].is_object(), "{v}");
}

/// Postgres nests block comments, MySQL and SQLite do not: the hidden write must be seen where
/// the engine would run it.
#[tokio::test(flavor = "current_thread")]
async fn nested_block_comment_is_dialect_correct() {
    let sql = "select 1 /* /* */ ; delete from t */";
    for driver in ["mysql", "sqlite"] {
        let (asked, ran, v) = deny_run(driver, names::RUN_QUERY, q(sql)).await;
        assert!(ran.is_empty() && asked >= 1, "[{driver}] {v}");
    }
    // Postgres: the whole text is one comment-tailed read, as the server would parse it.
    let (asked, ran, _) = deny_run("postgres", names::RUN_QUERY, q(sql)).await;
    assert_eq!((asked, ran.len()), (0, 1));
}

/// `EXPLAIN ANALYZE` executes the query. However it is spelled, and whichever tool carries it,
/// it needs approval: refused with no approver, refused when denied, never run unapproved.
#[tokio::test(flavor = "current_thread")]
async fn explain_analyze_is_never_run_unapproved() {
    let run_query_cases = [
        "EXPLAIN ANALYZE SELECT 1",
        "explain analyse select 1",
        "EXPLAIN (ANALYZE, BUFFERS) SELECT 1",
        "EXPLAIN (FORMAT JSON, ANALYZE) SELECT 1",
        "EXPLAIN/**/ANALYZE SELECT 1",
        "/* a */ EXPLAIN -- b\n ANALYZE SELECT 1",
        "select 1; EXPLAIN ANALYZE SELECT 1",
    ];
    let explain_cases = ["ANALYZE SELECT 1", "(ANALYZE) SELECT 1", "analyse select 1"];
    for driver in ["postgres", "mysql"] {
        for sql in run_query_cases {
            let (src, log) = rig(driver, None);
            let ts = ToolSet::new(src, limits());
            let (_, v) = call(&ts, names::RUN_QUERY, q(sql)).await;
            assert_eq!(v["error"]["code"], "no_approver", "[{driver}] {sql:?} {v}");
            assert!(log.lock().unwrap().is_empty(), "[{driver}] {sql:?}");
            let (asked, ran, v) = deny_run(driver, names::RUN_QUERY, q(sql)).await;
            assert!(asked >= 1 && ran.is_empty(), "[{driver}] {sql:?} {v}");
            assert_eq!(v["error"]["code"], "denied", "[{driver}] {sql:?}");
        }
        for sql in explain_cases {
            for analyze in [false, true] {
                let args = json!({"connection": "db", "sql": sql, "analyze": analyze});
                let (src, log) = rig(driver, None);
                let ts = ToolSet::new(src, limits());
                let (_, v) = call(&ts, names::EXPLAIN_QUERY, args.clone()).await;
                assert!(log.lock().unwrap().is_empty(), "[{driver}] {sql:?} {v}");
                assert!(v["error"].is_object(), "[{driver}] {sql:?} {v}");
                let (asked, ran, v) = deny_run(driver, names::EXPLAIN_QUERY, args).await;
                assert!(ran.is_empty() && asked == 0, "[{driver}] {sql:?} {v}");
                assert_eq!(v["error"]["code"], "invalid_arguments", "{v}");
            }
        }
    }
}

/// A plain `EXPLAIN` with no `ANALYZE` stays a read, so the guard is not blanket-refusing it.
#[tokio::test(flavor = "current_thread")]
async fn plain_explain_still_runs_without_approval() {
    for driver in ["postgres", "mysql"] {
        let (src, log) = rig(driver, None);
        let ts = ToolSet::new(src, limits());
        let (_, v) = call(&ts, names::RUN_QUERY, q("EXPLAIN (FORMAT JSON) SELECT 1")).await;
        assert!(v.get("error").is_none(), "[{driver}] {v}");
        assert_eq!(log.lock().unwrap().len(), 1);
    }
}
