// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The guard paths of `run_query`, `sample_rows` and `explain_query`, and the UI-only tools.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use common::{ScriptedApprover, SqliteSource, call, limits};
use serde_json::{Value, json};
use sqler_ai_api::BoxFuture;
use sqler_ai_tools::{ApprovalReason, ApprovalStage, Decision, ToolError, ToolSet, UiHost, names};
use sqler_driver_api::sqler_core::Environment;
use sqler_sql::StatementClass;

async fn setup(
    env: Option<Environment>,
    approver: Option<Arc<ScriptedApprover>>,
) -> (ToolSet, Arc<SqliteSource>) {
    let source = SqliteSource::new("guard", env).await;
    let mut ts = ToolSet::new(source.clone(), limits());
    if let Some(a) = approver {
        ts = ts.with_approver(a);
    }
    (ts, source)
}

fn run(sql: &str) -> Value {
    json!({"connection": "db", "sql": sql})
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test(flavor = "current_thread")]
async fn read_runs_without_asking() {
    let approver = ScriptedApprover::always(Decision::Deny);
    let (ts, s) = setup(None, Some(approver.clone())).await;
    let (report, v) = call(
        &ts,
        names::RUN_QUERY,
        run("select id, name from users order by id"),
    )
    .await;
    assert!(!report.result.is_error, "{v}");
    assert_eq!(approver.requests().len(), 0);
    let st = &v["statements"][0];
    assert_eq!(st["class"], "read");
    assert_eq!(st["approved"], false);
    assert_eq!(st["row_count"], 5);
    assert_eq!(st["rows"][0], json!([1, "ann"]));
    assert_eq!(st["columns"][0], json!({"name": "id", "type": "integer"}));
    assert_eq!(st["truncated"], json!(null));
    assert_eq!(st["notice"], json!(null));
    assert_eq!(v["not_run"], 0);
    assert_eq!(v["limits"], json!({"max_rows": 1000, "timeout_ms": 30000}));
    assert_eq!(s.executed(), ["select id, name from users order by id"]);
    assert_eq!(report.executed.len(), 1);
    assert_eq!(report.executed[0].class, StatementClass::Read);
    assert_eq!(report.connection.as_deref(), Some("db"));
    assert_eq!(s.acquire_count(), 1);
    assert_eq!(s.releases.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn non_read_statements_need_approval() {
    let cases = [
        (
            "insert into users (id, name) values (9, 'zed')",
            StatementClass::Write,
            ApprovalReason::Write,
        ),
        (
            "create table t (a integer)",
            StatementClass::Ddl,
            ApprovalReason::Ddl,
        ),
        (
            "pragma user_version = 3",
            StatementClass::Other,
            ApprovalReason::Other,
        ),
        (
            "frobnicate the database",
            StatementClass::Unknown,
            ApprovalReason::Unknown,
        ),
    ];
    for (sql, class, reason) in cases {
        // approved
        let approver = ScriptedApprover::always(Decision::Approve);
        let (ts, s) = setup(None, Some(approver.clone())).await;
        let (report, v) = call(&ts, names::RUN_QUERY, run(sql)).await;
        let reqs = approver.requests();
        assert_eq!(reqs.len(), 1, "{sql}");
        assert_eq!(reqs[0].sql, sql);
        assert_eq!(reqs[0].class, class, "{sql}");
        assert_eq!(reqs[0].reason, reason);
        assert_eq!(reqs[0].stage, ApprovalStage::Confirm);
        assert_eq!(reqs[0].tool, names::RUN_QUERY);
        assert_eq!((reqs[0].index, reqs[0].count), (0, 1));
        assert_eq!(reqs[0].connection.id, "db");
        assert_eq!(s.executed(), [sql], "{sql}: {v}");
        assert_eq!(v["statements"][0]["approved"], true);
        assert!(report.executed[0].approved);

        // denied
        let approver = ScriptedApprover::always(Decision::Deny);
        let (ts, s) = setup(None, Some(approver.clone())).await;
        let (report, v) = call(&ts, names::RUN_QUERY, run(sql)).await;
        assert!(report.result.is_error);
        assert_eq!(code(&v), "denied");
        assert_eq!(s.executed().len(), 0, "{sql}");
        assert_eq!(s.acquire_count(), 0);
        assert_eq!(report.executed.len(), 0);

        // no approver
        let (ts, s) = setup(None, None).await;
        let (report, v) = call(&ts, names::RUN_QUERY, run(sql)).await;
        assert!(report.result.is_error);
        assert_eq!(code(&v), "no_approver");
        assert_eq!(s.executed().len(), 0, "{sql}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prod_asks_twice_and_a_second_deny_stops() {
    let sql = "delete from orders";
    let approver = ScriptedApprover::always(Decision::Approve);
    let (ts, s) = setup(Some(Environment::Prod), Some(approver.clone())).await;
    let (_, v) = call(&ts, names::RUN_QUERY, run(sql)).await;
    let stages: Vec<_> = approver.requests().iter().map(|r| r.stage).collect();
    assert_eq!(stages, [ApprovalStage::Confirm, ApprovalStage::ProdSecond]);
    assert_eq!(s.executed(), [sql], "{v}");

    let approver = ScriptedApprover::new(|r| {
        if r.stage == ApprovalStage::ProdSecond {
            Decision::Deny
        } else {
            Decision::Approve
        }
    });
    let (ts, s) = setup(Some(Environment::Prod), Some(approver.clone())).await;
    let (report, v) = call(&ts, names::RUN_QUERY, run(sql)).await;
    assert!(report.result.is_error);
    assert_eq!(code(&v), "denied");
    assert_eq!(approver.requests().len(), 2);
    assert_eq!(s.executed().len(), 0);

    // staging asks once; a read on prod asks nothing
    let approver = ScriptedApprover::always(Decision::Approve);
    let (ts, _s) = setup(Some(Environment::Staging), Some(approver.clone())).await;
    call(&ts, names::RUN_QUERY, run(sql)).await;
    assert_eq!(approver.requests().len(), 1);
    let approver = ScriptedApprover::always(Decision::Approve);
    let (ts, _s) = setup(Some(Environment::Prod), Some(approver.clone())).await;
    call(&ts, names::RUN_QUERY, run("select 1")).await;
    assert_eq!(approver.requests().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn multi_statement_asks_per_statement_and_runs_nothing_on_a_deny() {
    let script = "select 1; insert into users (id, name) values (7,'g'); drop table orders;";
    let approver = ScriptedApprover::new(|r| {
        if r.class == StatementClass::Ddl {
            Decision::Deny
        } else {
            Decision::Approve
        }
    });
    let (ts, s) = setup(None, Some(approver.clone())).await;
    let (report, v) = call(&ts, names::RUN_QUERY, run(script)).await;
    assert!(report.result.is_error);
    assert_eq!(code(&v), "denied");
    assert_eq!(
        v["error"]["message"],
        "statement 2 was not approved; nothing was run"
    );
    let reqs = approver.requests();
    assert_eq!(reqs.len(), 2, "the read is not asked");
    assert_eq!((reqs[0].index, reqs[0].count), (1, 3));
    assert_eq!((reqs[1].index, reqs[1].count), (2, 3));
    assert_eq!(s.executed().len(), 0, "nothing runs, not even the read");

    let approver = ScriptedApprover::always(Decision::Approve);
    let (ts, s) = setup(None, Some(approver.clone())).await;
    let (_, v) = call(&ts, names::RUN_QUERY, run(script)).await;
    assert_eq!(approver.requests().len(), 2);
    assert_eq!(v["statements"].as_array().unwrap().len(), 3);
    assert_eq!(
        s.executed(),
        [
            "select 1",
            "insert into users (id, name) values (7,'g')",
            "drop table orders"
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_failing_statement_stops_the_rest() {
    let approver = ScriptedApprover::always(Decision::Approve);
    let (ts, s) = setup(None, Some(approver)).await;
    let (report, v) = call(
        &ts,
        names::RUN_QUERY,
        run("select 1; select * from missing_table; delete from users"),
    )
    .await;
    assert!(!report.result.is_error);
    assert_eq!(v["statements"].as_array().unwrap().len(), 2);
    assert!(v["statements"][1]["error"].is_string());
    assert_eq!(v["not_run"], 1);
    assert_eq!(s.executed().len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_connections_refuse_before_asking() {
    use sqler_ai_tools::{ConnectionInfo, SessionSource};
    struct ReadOnly(Arc<SqliteSource>);
    impl SessionSource for ReadOnly {
        fn connections(&self) -> Vec<ConnectionInfo> {
            vec![self.0.info.clone().with_read_only(true)]
        }
        fn catalog(&self, c: &str) -> Option<sqler_catalog::Catalog> {
            self.0.catalog(c)
        }
        fn acquire<'a>(
            &'a self,
            c: &'a str,
        ) -> BoxFuture<'a, Result<Box<dyn sqler_driver_api::Session>, ToolError>> {
            self.0.acquire(c)
        }
    }
    let inner = SqliteSource::new("ro", None).await;
    let approver = ScriptedApprover::always(Decision::Approve);
    let ts =
        ToolSet::new(Arc::new(ReadOnly(inner.clone())), limits()).with_approver(approver.clone());
    let (_, v) = call(&ts, names::LIST_CONNECTIONS, json!({})).await;
    assert_eq!(v["connections"][0]["read_only"], true);
    let (_, v) = call(&ts, names::RUN_QUERY, run("delete from users")).await;
    assert_eq!(code(&v), "read_only");
    assert_eq!(approver.requests().len(), 0);
    let (report, _) = call(&ts, names::RUN_QUERY, run("select 1")).await;
    assert!(!report.result.is_error);
}

#[tokio::test(flavor = "current_thread")]
async fn empty_sql_and_zero_rows_are_invalid() {
    let (ts, _s) = setup(None, None).await;
    for args in [
        run(""),
        run("-- nothing"),
        json!({"connection": "db", "sql": "select 1", "max_rows": 0}),
    ] {
        let (report, v) = call(&ts, names::RUN_QUERY, args).await;
        assert!(report.result.is_error);
        assert_eq!(code(&v), "invalid_arguments");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn sample_rows_reads_a_table() {
    let (ts, s) = setup(None, None).await;
    let (report, v) = call(
        &ts,
        names::SAMPLE_ROWS,
        json!({"connection": "db", "schema": "main", "table": "users", "limit": 2}),
    )
    .await;
    assert!(!report.result.is_error, "{v}");
    assert_eq!(v["statements"][0]["row_count"], 2);
    assert_eq!(s.executed(), [r#"SELECT * FROM "main"."users" LIMIT 2"#]);
    assert_eq!(v["statements"][0]["approved"], false);

    let (_, v) = call(
        &ts,
        names::SAMPLE_ROWS,
        json!({"connection": "db", "table": "users"}),
    )
    .await;
    assert_eq!(v["statements"][0]["row_count"], 5);

    // a hostile table name is quoted, not executed
    let (report, v) = call(
        &ts,
        names::SAMPLE_ROWS,
        json!({"connection": "db", "table": "users; drop table users"}),
    )
    .await;
    assert!(!report.result.is_error);
    assert!(v["statements"][0]["error"].is_string());
    assert_eq!(
        s.executed().last().map(String::as_str),
        Some(r#"SELECT * FROM "users; drop table users" LIMIT 50"#)
    );
    let (_, v) = call(&ts, names::RUN_QUERY, run("select count(*) from users")).await;
    assert_eq!(v["statements"][0]["rows"][0], json!([5]));

    let (report, v) = call(
        &ts,
        names::SAMPLE_ROWS,
        json!({"connection": "db", "table": "users", "limit": 0}),
    )
    .await;
    assert!(report.result.is_error);
    assert_eq!(code(&v), "invalid_arguments");
}

#[tokio::test(flavor = "current_thread")]
async fn explain_plain_and_analyze() {
    let approver = ScriptedApprover::always(Decision::Approve);
    let (ts, s) = setup(None, Some(approver.clone())).await;
    let (report, v) = call(
        &ts,
        names::EXPLAIN_QUERY,
        json!({"connection": "db", "sql": "select * from users where id = 1"}),
    )
    .await;
    assert!(!report.result.is_error, "{v}");
    assert_eq!(
        approver.requests().len(),
        0,
        "plain explain of a read needs no approval"
    );
    assert_eq!(
        s.executed(),
        ["EXPLAIN QUERY PLAN select * from users where id = 1"]
    );
    assert!(v["statements"][0]["row_count"].as_u64().unwrap() >= 1);

    let (report, v) = call(
        &ts,
        names::EXPLAIN_QUERY,
        json!({"connection": "db", "sql": "select 1", "analyze": true}),
    )
    .await;
    assert!(report.result.is_error);
    assert_eq!(code(&v), "unsupported");

    let (report, v) = call(
        &ts,
        names::EXPLAIN_QUERY,
        json!({"connection": "db", "sql": "select 1; select 2"}),
    )
    .await;
    assert!(report.result.is_error);
    assert_eq!(code(&v), "invalid_arguments");

    // explaining a write is asked, because the classifier does not trust it
    let denier = ScriptedApprover::always(Decision::Deny);
    let (ts, s2) = setup(None, Some(denier.clone())).await;
    let (_, v) = call(
        &ts,
        names::EXPLAIN_QUERY,
        json!({"connection": "db", "sql": "delete from users"}),
    )
    .await;
    assert_eq!(code(&v), "denied");
    assert_eq!(s2.executed().len(), 0);
}

struct RecordingUi(Mutex<Vec<String>>);

impl UiHost for RecordingUi {
    fn open_in_editor<'a>(
        &'a self,
        connection: Option<&'a str>,
        sql: &'a str,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        self.0
            .lock()
            .unwrap()
            .push(format!("editor {connection:?} {sql}"));
        Box::pin(std::future::ready(Ok(())))
    }

    fn open_results_tab<'a>(
        &'a self,
        connection: &'a str,
        sql: &'a str,
        title: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        self.0
            .lock()
            .unwrap()
            .push(format!("tab {connection} {sql} {title:?}"));
        Box::pin(std::future::ready(Ok(())))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ui_tools_need_a_ui_host() {
    let (ts, _s) = setup(None, None).await;
    assert!(
        !ts.definitions()
            .iter()
            .any(|d| d.name == names::OPEN_IN_EDITOR)
    );
    let (report, v) = call(&ts, names::OPEN_IN_EDITOR, json!({"sql": "select 1"})).await;
    assert!(report.result.is_error);
    assert_eq!(code(&v), "not_available");
    let (_, v) = call(&ts, names::OPEN_RESULTS_TAB, run("select 1")).await;
    assert_eq!(code(&v), "not_available");

    let ui = Arc::new(RecordingUi(Mutex::new(Vec::new())));
    let ts = ts.with_ui_host(ui.clone());
    assert!(
        ts.definitions()
            .iter()
            .any(|d| d.name == names::OPEN_RESULTS_TAB)
    );
    let (_, v) = call(
        &ts,
        names::OPEN_IN_EDITOR,
        json!({"sql": "delete from users", "connection": "db"}),
    )
    .await;
    assert_eq!(v, json!({"opened": true}));
    let (_, v) = call(
        &ts,
        names::OPEN_RESULTS_TAB,
        json!({"connection": "db", "sql": "select 1", "title": "One"}),
    )
    .await;
    assert_eq!(v, json!({"opened": true}));
    let (report, v) = call(&ts, names::OPEN_RESULTS_TAB, run("delete from users")).await;
    assert!(report.result.is_error);
    assert_eq!(code(&v), "invalid_arguments");
    assert_eq!(
        *ui.0.lock().unwrap(),
        [
            "editor Some(\"db\") delete from users".to_owned(),
            "tab db select 1 Some(\"One\")".to_owned()
        ]
    );
}
