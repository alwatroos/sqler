// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated checks of the editing path on `PostgreSQL`, `MySQL` and `MariaDB` (AGENTS.md §3):
//! the schema probe (bound name, temporary-table shadow, odd names), and the apply job on the
//! lent session and on a session it opens. Without `SQLER_TEST_POSTGRES_URL` /
//! `SQLER_TEST_MYSQL_URL` the tests skip; `SQLER_TEST_REQUIRE_DB=postgres,mysql` makes a missing
//! variable a failure.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::sync::Arc;

use sqler_config::ConnectionSpec;
use sqler_driver_api::sqler_core::{
    Column, ConnectionConfig, DbObject, DriverId, ObjectKind, ObjectPath, Query, Row, Table, Value,
};
use sqler_driver_api::{BoxFuture, DriverRegistry, Session, collect};
use sqler_sql::Dialect;
use sqler_ui::connections::{
    ConnectFailure, ConnectRequest, Connector, HostKeyPrompt, SecretSource,
};
use sqler_ui::grid::edit::{
    ApplyId, ApplyJob, ApplyOutcome, ApplySession, ApplyStop, EditRequest, EditSource, Editability,
    GridKey, ModelRow, ReadOnlyReason, SchemaProbe, StagedEdits, check_editable,
};
use sqler_ui::tabs::{TabConnection, TabKind, TabStrip};

fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

/// The value of `var` if the tests for `db` run; panics if `db` is required but `var` is unset.
fn db_env(var: &str, db: &str) -> Option<String> {
    if let Ok(value) = std::env::var(var)
        && !value.is_empty()
    {
        return Some(value);
    }
    let required = std::env::var("SQLER_TEST_REQUIRE_DB")
        .is_ok_and(|list| list.split(',').any(|id| id.trim() == db));
    assert!(
        !required,
        "{var} is not set, but {db} is listed in SQLER_TEST_REQUIRE_DB"
    );
    println!("skipped: {var} is not set (see AGENTS.md §3)");
    None
}

fn registry() -> DriverRegistry {
    let mut registry = DriverRegistry::new();
    registry
        .register(Box::new(sqler_driver_postgres::PostgresDriver::new()))
        .unwrap();
    registry
        .register(Box::new(sqler_driver_mysql::MySqlDriver::new()))
        .unwrap();
    registry
}

async fn connect(driver: &str, url: &str) -> Box<dyn Session> {
    let config = ConnectionConfig::new("t", DriverId::new(driver).unwrap(), url);
    registry().connect(&config).await.unwrap()
}

/// Opens a real session for the URL in the request's target; counts nothing, fails never.
struct UrlConnector;

impl Connector for UrlConnector {
    fn driver_ids(&self) -> Vec<DriverId> {
        vec![
            DriverId::new("postgres").unwrap(),
            DriverId::new("mysql").unwrap(),
        ]
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
        Box::pin(async move {
            let config = ConnectionConfig::new(
                "t",
                request.spec.driver.clone(),
                request.spec.target.clone(),
            );
            Ok(registry().connect(&config).await?)
        })
    }

    fn trust_host_key(
        &self,
        _key: HostKeyPrompt,
    ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn request(driver: &str, url: &str) -> ConnectRequest {
    ConnectRequest {
        spec: ConnectionSpec::new("c", DriverId::new(driver).unwrap(), url),
        db_password: SecretSource::None,
        ssh_secret: SecretSource::None,
    }
}

fn grid_key() -> GridKey {
    let connection = TabConnection {
        name: "t".to_owned(),
        driver: DriverId::new("postgres").unwrap(),
        color: None,
        environment: None,
    };
    GridKey {
        tab: TabStrip::default().open(connection, TabKind::Query),
        result: Some(0),
    }
}

async fn rows(s: &mut dyn Session, sql: &str) -> Vec<Row> {
    let q = Query::new(sql);
    let mut stream = s.execute(&q).await.unwrap();
    collect(&mut *stream).await.unwrap().rows().to_vec()
}

async fn exec(s: &mut dyn Session, sql: &str) {
    rows(s, sql).await;
}

/// The names of `t`, ordered by id, as text.
async fn names(s: &mut dyn Session, table: &str) -> Vec<String> {
    rows(s, &format!("SELECT name FROM {table} ORDER BY id"))
        .await
        .into_iter()
        .map(|r| match &r.0[0] {
            Value::Text(t) => t.clone(),
            Value::Null => "<null>".to_owned(),
            other => other.to_string(),
        })
        .collect()
}

async fn describe(s: &mut dyn Session, schema: &str, name: &str) -> Table {
    let object = DbObject::new(
        ObjectKind::Table,
        ObjectPath::root().in_schema(schema),
        name,
    );
    s.introspection()
        .unwrap()
        .table(&object)
        .await
        .unwrap()
        .unwrap()
}

fn columns(table: &Table) -> Vec<Column> {
    table
        .columns
        .iter()
        .map(|c| Column::new(c.name.clone(), c.data_type))
        .collect()
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

fn editable(dialect: Dialect, table: &Table, sql: &str, resolved: Option<&str>) -> Editability {
    check_editable(&EditRequest {
        dialect: Some(dialect),
        source: EditSource::QueryResult { sql },
        table: Some(table),
        unique_keys: &[],
        columns: &columns(table),
        allow_keyless: false,
        resolved_schema: resolved,
    })
}

async fn resolve(dialect: Dialect, s: &mut dyn Session, sql: &str) -> Option<String> {
    SchemaProbe::for_sql(dialect, sql).unwrap().run(s).await
}

/// Applies `edits` on the lent session; returns the outcome and the session.
async fn apply_lent(
    session: Box<dyn Session>,
    edits: &StagedEdits,
) -> (ApplyOutcome, Box<dyn Session>) {
    let job = ApplyJob {
        key: grid_key(),
        run: ApplyId::new(1),
        session: ApplySession::Lent(session),
        plan: edits.plan().unwrap().confirm_non_production(),
        stop: ApplyStop::new(),
    };
    let done = job.run(Arc::new(UrlConnector)).await;
    (done.outcome, done.session.unwrap())
}

/// Row `(id, name, qty)` as the grid holds it after `SELECT * FROM t`.
fn row3(id: i64, name: &str, qty: i64) -> Row {
    Row(vec![Value::Int(id), text(name), Value::Int(qty)])
}

/// The staged changes every flow applies: rename row 1, delete row 2, add row 10.
fn stage(edits: &mut StagedEdits) {
    edits
        .set_cell(ModelRow(0), &row3(1, "a", 1), 1, text("A"))
        .unwrap();
    edits.delete_row(ModelRow(1), &row3(2, "b", 2)).unwrap();
    let n = edits.add_row();
    edits.set_new_cell(n, 0, Some(Value::Int(10))).unwrap();
    edits.set_new_cell(n, 1, Some(text("z"))).unwrap();
}

fn target_of(e: Editability) -> StagedEdits {
    match e {
        Editability::Editable(t) => StagedEdits::new(t),
        other => panic!("not editable: {other:?}"),
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one linear scenario against one server
fn postgres_probe_and_apply() {
    let Some(url) = db_env("SQLER_TEST_POSTGRES_URL", "postgres") else {
        return;
    };
    run(async {
        let d = Dialect::Postgres;
        let mut s = connect("postgres", &url).await;
        exec(&mut *s, "DROP SCHEMA IF EXISTS sqler_t11 CASCADE").await;
        exec(&mut *s, "CREATE SCHEMA sqler_t11").await;
        exec(&mut *s, "SET search_path TO sqler_t11").await;
        exec(
            &mut *s,
            "CREATE TABLE t (id bigint PRIMARY KEY, name text, qty integer)",
        )
        .await;
        exec(
            &mut *s,
            "INSERT INTO t VALUES (1, 'a', 1), (2, 'b', 2), (3, 'c', 3)",
        )
        .await;
        exec(&mut *s, "CREATE TABLE \"x'; drop table t; --\" (id bigint)").await;
        exec(&mut *s, "CREATE TABLE \"MixedCase\" (id bigint)").await;

        // The probe: as written, folded like the server, quoted, odd, missing.
        assert_eq!(
            resolve(d, &mut *s, "SELECT * FROM t").await.as_deref(),
            Some("sqler_t11")
        );
        assert_eq!(
            resolve(d, &mut *s, "SELECT * FROM T").await.as_deref(),
            Some("sqler_t11")
        );
        assert_eq!(
            resolve(d, &mut *s, "SELECT * FROM \"x'; drop table t; --\"")
                .await
                .as_deref(),
            Some("sqler_t11")
        );
        assert_eq!(
            resolve(d, &mut *s, "SELECT * FROM \"MixedCase\"")
                .await
                .as_deref(),
            Some("sqler_t11")
        );
        // Unquoted `MixedCase` folds to `mixedcase`, which does not exist.
        assert_eq!(resolve(d, &mut *s, "SELECT * FROM MixedCase").await, None);
        assert_eq!(resolve(d, &mut *s, "SELECT * FROM nope").await, None);
        // The odd name did not run as SQL.
        assert_eq!(rows(&mut *s, "SELECT count(*) FROM t").await.len(), 1);

        let table = describe(&mut *s, "sqler_t11", "t").await;
        let sql = "SELECT * FROM t";
        let resolved = resolve(d, &mut *s, sql).await;
        let mut edits = target_of(editable(d, &table, sql, resolved.as_deref()));

        // A stale original: the row no longer matches, nothing is written.
        let mut stale = target_of(editable(d, &table, sql, resolved.as_deref()));
        stale
            .set_cell(ModelRow(0), &row3(99, "a", 1), 1, text("X"))
            .unwrap();
        let (outcome, back) = apply_lent(s, &stale).await;
        assert!(
            matches!(outcome, ApplyOutcome::RolledBack { at: Some(0), .. }),
            "{outcome:?}"
        );
        s = back;
        assert_eq!(names(&mut *s, "t").await, ["a", "b", "c"]);

        // The flow on the lent session.
        stage(&mut edits);
        let (outcome, back) = apply_lent(s, &edits).await;
        assert_eq!(
            outcome,
            ApplyOutcome::Committed { applied: 3 },
            "{outcome:?}"
        );
        s = back;
        assert_eq!(names(&mut *s, "t").await, ["A", "c", "z"]);

        // The flow on a session the job opens (a table-data tab has none).
        let mut edits = target_of(editable(d, &table, sql, resolved.as_deref()));
        edits
            .set_cell(ModelRow(0), &row3(3, "c", 3), 1, text("C"))
            .unwrap();
        let job = ApplyJob {
            key: grid_key(),
            run: ApplyId::new(2),
            session: ApplySession::Connect(request("postgres", &url)),
            plan: edits.plan().unwrap().confirm_non_production(),
            stop: ApplyStop::new(),
        };
        let done = job.run(Arc::new(UrlConnector)).await;
        assert_eq!(done.outcome, ApplyOutcome::Committed { applied: 1 });
        assert!(done.session.is_none());
        assert_eq!(names(&mut *s, "t").await, ["A", "C", "z"]);

        // A temporary table shadows the loaded one: the probe says so and the result is read-only.
        exec(
            &mut *s,
            "CREATE TEMP TABLE t (id bigint PRIMARY KEY, name text, qty integer)",
        )
        .await;
        let shadow = resolve(d, &mut *s, sql).await;
        assert!(
            shadow.as_deref().is_some_and(|n| n.starts_with("pg_temp")),
            "{shadow:?}"
        );
        assert!(matches!(
            editable(d, &table, sql, shadow.as_deref()),
            Editability::ReadOnly(ReadOnlyReason::TableMismatch)
        ));
        // A failed lookup is read-only too.
        assert!(matches!(
            editable(d, &table, sql, None),
            Editability::ReadOnly(ReadOnlyReason::TableMismatch)
        ));
        exec(&mut *s, "DROP SCHEMA sqler_t11 CASCADE").await;
    });
}

/// The `MySQL` and `MariaDB` flow; `SQLER_TEST_MYSQL_URL` points at either.
#[test]
fn mysql_probe_and_apply() {
    let Some(url) = db_env("SQLER_TEST_MYSQL_URL", "mysql") else {
        return;
    };
    run(async {
        let d = Dialect::MySql;
        let mut s = connect("mysql", &url).await;
        exec(&mut *s, "DROP DATABASE IF EXISTS sqler_t11").await;
        exec(&mut *s, "CREATE DATABASE sqler_t11").await;
        exec(&mut *s, "USE sqler_t11").await;
        exec(
            &mut *s,
            "CREATE TABLE t (id bigint PRIMARY KEY, name varchar(20), qty int)",
        )
        .await;
        exec(
            &mut *s,
            "INSERT INTO t VALUES (1, 'a', 1), (2, 'b', 2), (3, 'c', 3)",
        )
        .await;

        // The probe is the current database; a missing database gives None.
        let sql = "SELECT * FROM t";
        let resolved = resolve(d, &mut *s, sql).await;
        assert_eq!(resolved.as_deref(), Some("sqler_t11"));
        // A `USE` between two lookups on one session is seen by the second (the probe is not a
        // cached prepared statement), then back again.
        exec(&mut *s, "CREATE DATABASE IF NOT EXISTS sqler_t11b").await;
        exec(&mut *s, "USE sqler_t11b").await;
        assert_eq!(
            resolve(d, &mut *s, sql).await.as_deref(),
            Some("sqler_t11b")
        );
        exec(&mut *s, "USE sqler_t11").await;
        assert_eq!(resolve(d, &mut *s, sql).await.as_deref(), Some("sqler_t11"));
        exec(&mut *s, "DROP DATABASE sqler_t11b").await;
        let table = describe(&mut *s, "sqler_t11", "t").await;
        let mut edits = target_of(editable(d, &table, sql, resolved.as_deref()));

        // A stale original rolls back and writes nothing.
        let mut stale = target_of(editable(d, &table, sql, resolved.as_deref()));
        stale
            .set_cell(ModelRow(0), &row3(99, "a", 1), 1, text("X"))
            .unwrap();
        let (outcome, back) = apply_lent(s, &stale).await;
        assert!(
            matches!(outcome, ApplyOutcome::RolledBack { at: Some(0), .. }),
            "{outcome:?}"
        );
        s = back;
        assert_eq!(names(&mut *s, "t").await, ["a", "b", "c"]);

        // The flow on the lent session.
        stage(&mut edits);
        let (outcome, back) = apply_lent(s, &edits).await;
        assert_eq!(
            outcome,
            ApplyOutcome::Committed { applied: 3 },
            "{outcome:?}"
        );
        s = back;
        assert_eq!(names(&mut *s, "t").await, ["A", "c", "z"]);

        // The flow on a session the job opens (it uses the default database of the URL, and the
        // plan is qualified, so it still finds the table).
        let mut edits = target_of(editable(d, &table, sql, resolved.as_deref()));
        edits
            .set_cell(ModelRow(0), &row3(3, "c", 3), 1, text("C"))
            .unwrap();
        let job = ApplyJob {
            key: grid_key(),
            run: ApplyId::new(2),
            session: ApplySession::Connect(request("mysql", &url)),
            plan: edits.plan().unwrap().confirm_non_production(),
            stop: ApplyStop::new(),
        };
        let done = job.run(Arc::new(UrlConnector)).await;
        assert_eq!(done.outcome, ApplyOutcome::Committed { applied: 1 });
        assert_eq!(names(&mut *s, "t").await, ["A", "C", "z"]);

        // A temporary table of the same name hides the permanent one on this session. A qualified
        // name reaches the temporary table too, so a write on the same session changes what the
        // user sees (the reason a query result applies on its own session), and the permanent
        // table is not touched.
        exec(
            &mut *s,
            "CREATE TEMPORARY TABLE t (id bigint PRIMARY KEY, name varchar(20), qty int)",
        )
        .await;
        exec(&mut *s, "INSERT INTO t VALUES (1, 'tmp', 1)").await;
        let mut edits = target_of(editable(d, &table, sql, resolved.as_deref()));
        edits
            .set_cell(ModelRow(0), &row3(1, "tmp", 1), 1, text("TMP2"))
            .unwrap();
        let (outcome, back) = apply_lent(s, &edits).await;
        assert_eq!(
            outcome,
            ApplyOutcome::Committed { applied: 1 },
            "{outcome:?}"
        );
        s = back;
        assert_eq!(names(&mut *s, "t").await, ["TMP2"]);
        exec(&mut *s, "DROP TEMPORARY TABLE t").await;
        assert_eq!(names(&mut *s, "t").await, ["A", "C", "z"]);
        exec(&mut *s, "DROP DATABASE sqler_t11").await;
    });
}
