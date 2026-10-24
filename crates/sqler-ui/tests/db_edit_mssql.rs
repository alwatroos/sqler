// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated end-to-end check of inline editing on a `SQL Server` session (AGENTS.md §3):
//! the schema probe, the apply through the production gate, the row-count check against the
//! counts the driver reports, and the rollback when the count is unknown (`SET NOCOUNT ON`).
//! Without `SQLER_TEST_MSSQL_URL` the test skips; `SQLER_TEST_REQUIRE_DB=mssql` makes a missing
//! variable a failure.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;

use sqler_driver_api::sqler_core::{
    Column, ConnectionConfig, DbObject, DriverId, ObjectKind, ObjectPath, Query, Row, Table, Value,
};
use sqler_driver_api::{CancelFlag, Driver as _, Session, collect};
use sqler_driver_mssql::MssqlDriver;
use sqler_sql::Dialect;
use sqler_ui::grid::edit::{
    ApplyFailure, ApplyOutcome, EditRequest, EditSource, Editability, ModelRow, SchemaProbe,
    StagedEdits, apply, check_editable,
};

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

async fn rows(s: &mut dyn Session, sql: &str) -> Vec<Row> {
    let q = Query::new(sql);
    let mut stream = s.execute(&q).await.unwrap();
    collect(&mut *stream).await.unwrap().rows().to_vec()
}

async fn exec(s: &mut dyn Session, sql: &str) {
    rows(s, sql).await;
}

async fn describe(s: &mut dyn Session, name: &str) -> Table {
    let object = DbObject::new(ObjectKind::Table, ObjectPath::root().in_schema("dbo"), name);
    s.introspection()
        .unwrap()
        .table(&object)
        .await
        .unwrap()
        .unwrap()
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

fn editable(table: &Table, sql: &str, keyless: bool, resolved: Option<&str>) -> Editability {
    let columns: Vec<Column> = table
        .columns
        .iter()
        .map(|c| Column::new(c.name.clone(), c.data_type))
        .collect();
    check_editable(&EditRequest {
        dialect: Some(Dialect::TSql),
        source: EditSource::QueryResult { sql },
        table: Some(table),
        unique_keys: &[],
        columns: &columns,
        allow_keyless: keyless,
        resolved_schema: resolved,
    })
}

fn staged(table: &Table, sql: &str, keyless: bool, resolved: Option<&str>) -> StagedEdits {
    match editable(table, sql, keyless, resolved) {
        Editability::Editable(t) => StagedEdits::new(t),
        other => panic!("not editable: {other:?}"),
    }
}

/// The `name` column of `sqler_s4_t` ordered by id.
async fn names(s: &mut dyn Session) -> Vec<Value> {
    rows(s, "SELECT name FROM dbo.sqler_s4_t ORDER BY id")
        .await
        .into_iter()
        .map(|r| r.0[0].clone())
        .collect()
}

async fn reset(s: &mut dyn Session) {
    exec(s, "DROP TABLE IF EXISTS dbo.sqler_s4_t").await;
    exec(s, "DROP TABLE IF EXISTS dbo.sqler_s4_k").await;
}

#[test]
#[allow(clippy::too_many_lines)] // one linear scenario against one server
fn mssql_apply_checks_the_reported_row_counts() {
    let Some(url) = db_env("SQLER_TEST_MSSQL_URL", "mssql") else {
        return;
    };
    run(async {
        let config = ConnectionConfig::new("t", DriverId::new("sqlserver").unwrap(), url);
        let mut s = MssqlDriver::new().connect(&config).await.unwrap();
        reset(&mut *s).await;
        exec(
            &mut *s,
            "CREATE TABLE dbo.sqler_s4_t (id bigint PRIMARY KEY, name nvarchar(20), qty int)",
        )
        .await;
        exec(
            &mut *s,
            "INSERT INTO dbo.sqler_s4_t VALUES (1, N'a', 1), (2, N'b', 2), (3, N'c', 3)",
        )
        .await;
        exec(
            &mut *s,
            "CREATE TABLE dbo.sqler_s4_k (name nvarchar(20), qty int)",
        )
        .await;
        exec(
            &mut *s,
            "INSERT INTO dbo.sqler_s4_k VALUES (N'a', 1), (N'a', 1), (N'b', 2)",
        )
        .await;

        let sql = "SELECT * FROM sqler_s4_t";
        let resolved = SchemaProbe::for_sql(Dialect::TSql, sql)
            .unwrap()
            .run(&mut *s)
            .await;
        assert_eq!(resolved.as_deref(), Some("dbo"));
        let table = describe(&mut *s, "sqler_s4_t").await;
        let row1 = Row(vec![Value::Int(1), text("a"), Value::Int(1)]);
        let stop = CancelFlag::new();

        // (a) One edit commits and the row is changed.
        let mut edits = staged(&table, sql, false, resolved.as_deref());
        edits.set_cell(ModelRow(0), &row1, 1, text("A")).unwrap();
        let outcome = apply(
            &mut *s,
            edits.plan().unwrap().confirm_non_production(),
            &stop,
        )
        .await;
        assert_eq!(outcome, ApplyOutcome::Committed { applied: 1 });
        assert_eq!(
            names(&mut *s).await,
            [text("A"), text("b"), text("c")],
            "the edit is persisted"
        );

        // (b) A keyless table: an edit that matches two identical rows rolls back.
        let ksql = "SELECT * FROM sqler_s4_k";
        let ktable = describe(&mut *s, "sqler_s4_k").await;
        let mut kedits = staged(&ktable, ksql, true, Some("dbo"));
        kedits
            .set_cell(
                ModelRow(0),
                &Row(vec![text("a"), Value::Int(1)]),
                1,
                Value::Int(9),
            )
            .unwrap();
        let outcome = apply(
            &mut *s,
            kedits.plan().unwrap().confirm_non_production(),
            &stop,
        )
        .await;
        assert_eq!(
            outcome,
            ApplyOutcome::RolledBack {
                at: Some(0),
                failure: ApplyFailure::RowCount {
                    expected: 1,
                    actual: Some(2)
                },
                rollback_error: None
            }
        );
        let qty = rows(&mut *s, "SELECT qty FROM dbo.sqler_s4_k ORDER BY qty").await;
        assert_eq!(
            qty,
            vec![
                Row(vec![Value::Int(1)]),
                Row(vec![Value::Int(1)]),
                Row(vec![Value::Int(2)])
            ],
            "both duplicate rows are unchanged"
        );

        // (c) NOCOUNT ON hides the count: the apply cannot verify it and rolls back.
        exec(&mut *s, "SET NOCOUNT ON").await;
        let mut edits = staged(&table, sql, false, resolved.as_deref());
        edits
            .set_cell(
                ModelRow(1),
                &Row(vec![Value::Int(2), text("b"), Value::Int(2)]),
                1,
                text("B"),
            )
            .unwrap();
        let outcome = apply(
            &mut *s,
            edits.plan().unwrap().confirm_non_production(),
            &stop,
        )
        .await;
        assert!(
            matches!(
                outcome,
                ApplyOutcome::RolledBack {
                    failure: ApplyFailure::RowCount { actual: None, .. },
                    ..
                }
            ),
            "{outcome:?}"
        );
        exec(&mut *s, "SET NOCOUNT OFF").await;
        assert_eq!(
            names(&mut *s).await,
            [text("A"), text("b"), text("c")],
            "nothing is changed"
        );

        // (d) The production gate: no consent, no token; with consent the plan applies.
        let mut edits = staged(&table, sql, false, resolved.as_deref());
        edits
            .set_cell(
                ModelRow(2),
                &Row(vec![Value::Int(3), text("c"), Value::Int(3)]),
                1,
                text("C"),
            )
            .unwrap();
        assert!(edits.plan().unwrap().confirm(true, false).is_err());
        let confirmed = edits.plan().unwrap().confirm(true, true).unwrap();
        let outcome = apply(&mut *s, confirmed, &stop).await;
        assert_eq!(outcome, ApplyOutcome::Committed { applied: 1 });
        assert_eq!(names(&mut *s).await, [text("A"), text("b"), text("C")]);

        reset(&mut *s).await;
    });
}
