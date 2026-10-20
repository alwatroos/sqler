// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Staged edits applied to a real in-memory `SQLite` session: commit, rollback on a wrong row
//! count, rollback on a failure half-way, NULL handling and the keyless opt-in.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use sqler_driver_api::sqler_core::{
    Column, ColumnInfo, ConnectionConfig, DataType, DbObject, DriverId, ObjectKind, ObjectPath,
    PrimaryKey, Query, Row, Table, Value,
};
use sqler_driver_api::{CancelFlag, DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_sql::Dialect;
use sqler_ui::grid::edit::{
    ApplyFailure, ApplyOutcome, EditError, EditRequest, EditSource, EditTarget, Editability,
    ModelRow, ReadOnlyReason, StagedEdits, apply, check_editable,
};
use tokio::runtime::{Builder, Runtime};

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

async fn exec(s: &mut dyn Session, sql: &str) {
    let q = Query::new(sql);
    let mut stream = s.execute(&q).await.unwrap();
    collect(&mut *stream).await.unwrap();
}

async fn fetch(s: &mut dyn Session, sql: &str) -> Vec<Row> {
    let q = Query::new(sql);
    let mut stream = s.execute(&q).await.unwrap();
    collect(&mut *stream).await.unwrap().rows().to_vec()
}

fn t_object(name: &str) -> DbObject {
    DbObject::new(
        ObjectKind::Table,
        ObjectPath::root().in_schema("main"),
        name,
    )
}

/// `t(id integer primary key, name text, note text)`, built by hand.
fn keyed_table() -> Table {
    Table::new(t_object("t"))
        .with_columns(vec![
            ColumnInfo::new("id", DataType::Integer).with_nullable(false),
            ColumnInfo::new("name", DataType::Text),
            ColumnInfo::new("note", DataType::Text),
        ])
        .with_primary_key(PrimaryKey::new(vec!["id".to_owned()]))
}

/// `k(name text, note text)`: no key at all.
fn keyless_table() -> Table {
    Table::new(t_object("k")).with_columns(vec![
        ColumnInfo::new("name", DataType::Text),
        ColumnInfo::new("note", DataType::Text),
    ])
}

fn columns(table: &Table) -> Vec<Column> {
    table
        .columns
        .iter()
        .map(|c| Column::new(c.name.clone(), c.data_type))
        .collect()
}

fn editable(table: &Table, sql: &str, keyless: bool) -> Editability {
    check_editable(&EditRequest {
        dialect: Some(Dialect::Sqlite),
        source: EditSource::QueryResult { sql },
        table: Some(table),
        unique_keys: &[],
        columns: &columns(table),
        allow_keyless: keyless,
        resolved_schema: Some("main"),
    })
}

fn target(table: &Table, sql: &str, keyless: bool) -> EditTarget {
    match editable(table, sql, keyless) {
        Editability::Editable(t) => t,
        other => panic!("not editable: {other:?}"),
    }
}

async fn setup_keyed(s: &mut dyn Session) {
    exec(
        s,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, note TEXT)",
    )
    .await;
    exec(
        s,
        "INSERT INTO t VALUES (1, 'one', 'n1'), (2, 'two', NULL), (3, 'three', 'n3')",
    )
    .await;
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

#[test]
fn update_insert_and_delete_commit_together() {
    runtime().block_on(async {
        let mut s = session().await;
        setup_keyed(&mut *s).await;
        let rows = fetch(&mut *s, "SELECT * FROM t ORDER BY id").await;
        let mut edits = StagedEdits::new(target(&keyed_table(), "SELECT * FROM t", false));
        edits
            .set_cell(
                ModelRow(0),
                &rows[0],
                1,
                text("it's \"new\"; DROP TABLE t; --"),
            )
            .unwrap();
        edits
            .set_cell(ModelRow(0), &rows[0], 2, Value::Null)
            .unwrap();
        edits
            .set_cell(ModelRow(1), &rows[1], 2, text("filled"))
            .unwrap();
        edits.delete_row(ModelRow(2), &rows[2]).unwrap();
        let new = edits.add_row();
        edits.set_new_cell(new, 0, Some(Value::Int(10))).unwrap();
        edits.set_new_cell(new, 1, Some(Value::Null)).unwrap();
        let plan = edits.plan().unwrap();
        assert_eq!(plan.statements().len(), 4);

        let stop = CancelFlag::new();
        let outcome = apply(&mut *s, plan.confirm_non_production(), &stop).await;
        assert_eq!(outcome, ApplyOutcome::Committed { applied: 4 });
        edits.clear();

        let after = fetch(&mut *s, "SELECT id, name, note FROM t ORDER BY id").await;
        assert_eq!(
            after,
            vec![
                Row(vec![
                    Value::Int(1),
                    text("it's \"new\"; DROP TABLE t; --"),
                    Value::Null
                ]),
                Row(vec![Value::Int(2), text("two"), text("filled")]),
                Row(vec![Value::Int(10), Value::Null, Value::Null]),
            ]
        );
    });
}

#[test]
fn a_zero_row_update_rolls_back_everything_and_keeps_the_edits() {
    runtime().block_on(async {
        let mut s = session().await;
        setup_keyed(&mut *s).await;
        let rows = fetch(&mut *s, "SELECT * FROM t ORDER BY id").await;
        let mut edits = StagedEdits::new(target(&keyed_table(), "SELECT * FROM t", false));
        edits
            .set_cell(ModelRow(0), &rows[0], 1, text("changed"))
            .unwrap();
        edits
            .set_cell(ModelRow(1), &rows[1], 1, text("changed"))
            .unwrap();
        let plan = edits.plan().unwrap();
        // The second row disappears between reading and applying.
        exec(&mut *s, "DELETE FROM t WHERE id = 2").await;

        let outcome = apply(&mut *s, plan.confirm_non_production(), &CancelFlag::new()).await;
        assert_eq!(
            outcome,
            ApplyOutcome::RolledBack {
                at: Some(1),
                failure: ApplyFailure::RowCount {
                    expected: 1,
                    actual: Some(0)
                },
                rollback_error: None
            }
        );
        assert_eq!(edits.change_count(), 2, "the staged edits are kept");
        let names = fetch(&mut *s, "SELECT name FROM t ORDER BY id").await;
        assert_eq!(
            names,
            vec![Row(vec![text("one")]), Row(vec![text("three")])],
            "the first update is not persisted"
        );
        // The session is usable and no transaction is left open.
        exec(&mut *s, "BEGIN").await;
        exec(&mut *s, "ROLLBACK").await;
    });
}

#[test]
fn a_failure_half_way_rolls_back_the_earlier_statements() {
    runtime().block_on(async {
        let mut s = session().await;
        setup_keyed(&mut *s).await;
        let rows = fetch(&mut *s, "SELECT * FROM t ORDER BY id").await;
        let mut edits = StagedEdits::new(target(&keyed_table(), "SELECT * FROM t", false));
        edits.delete_row(ModelRow(2), &rows[2]).unwrap();
        edits
            .set_cell(ModelRow(0), &rows[0], 1, text("changed"))
            .unwrap();
        // Moving row 2 onto the key of row 1 violates the primary key.
        edits
            .set_cell(ModelRow(1), &rows[1], 0, Value::Int(1))
            .unwrap();
        let plan = edits.plan().unwrap();

        let outcome = apply(&mut *s, plan.confirm_non_production(), &CancelFlag::new()).await;
        match outcome {
            ApplyOutcome::RolledBack {
                at,
                failure: ApplyFailure::Driver(message),
                rollback_error,
            } => {
                assert_eq!(at, Some(2));
                assert!(message.to_lowercase().contains("unique"), "{message}");
                assert_eq!(rollback_error, None);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(edits.change_count(), 3);
        let all = fetch(&mut *s, "SELECT id, name FROM t ORDER BY id").await;
        assert_eq!(all.len(), 3, "the delete was undone");
        assert_eq!(all[0], Row(vec![Value::Int(1), text("one")]));
    });
}

#[test]
fn a_cancel_before_the_first_statement_changes_nothing() {
    runtime().block_on(async {
        let mut s = session().await;
        setup_keyed(&mut *s).await;
        let rows = fetch(&mut *s, "SELECT * FROM t ORDER BY id").await;
        let mut edits = StagedEdits::new(target(&keyed_table(), "SELECT * FROM t", false));
        edits.delete_row(ModelRow(0), &rows[0]).unwrap();
        let plan = edits.plan().unwrap();
        let stop = CancelFlag::new();
        stop.cancel();
        let outcome = apply(&mut *s, plan.confirm_non_production(), &stop).await;
        assert_eq!(
            outcome,
            ApplyOutcome::RolledBack {
                at: Some(0),
                failure: ApplyFailure::Cancelled,
                rollback_error: None
            }
        );
        assert_eq!(fetch(&mut *s, "SELECT id FROM t").await.len(), 3);
    });
}

#[test]
fn null_values_are_set_and_matched() {
    runtime().block_on(async {
        let mut s = session().await;
        // A text primary key may hold NULL in SQLite: such a row cannot be found again.
        exec(&mut *s, "CREATE TABLE p (code TEXT PRIMARY KEY, v TEXT)").await;
        exec(&mut *s, "INSERT INTO p VALUES ('a', NULL), (NULL, 'x')").await;
        let table = Table::new(t_object("p"))
            .with_columns(vec![
                ColumnInfo::new("code", DataType::Text),
                ColumnInfo::new("v", DataType::Text),
            ])
            .with_primary_key(PrimaryKey::new(vec!["code".to_owned()]));
        let rows = fetch(&mut *s, "SELECT * FROM p ORDER BY code").await;
        let mut edits = StagedEdits::new(target(&table, "SELECT * FROM p", false));
        assert!(matches!(
            edits.set_cell(ModelRow(0), &rows[0], 1, text("y")),
            Err(EditError::NullKey { .. })
        ));
        assert!(matches!(
            edits.delete_row(ModelRow(0), &rows[0]),
            Err(EditError::NullKey { .. })
        ));
        // A NULL non-key value in the row is fine, and setting a cell to NULL binds NULL.
        edits
            .set_cell(ModelRow(1), &rows[1], 1, text("filled"))
            .unwrap();
        let outcome = apply(
            &mut *s,
            edits.plan().unwrap().confirm_non_production(),
            &CancelFlag::new(),
        )
        .await;
        assert_eq!(outcome, ApplyOutcome::Committed { applied: 1 });
        let v = fetch(&mut *s, "SELECT v FROM p WHERE code = 'a'").await;
        assert_eq!(v, vec![Row(vec![text("filled")])]);
    });
}

#[test]
fn keyless_is_read_only_until_the_user_opts_in() {
    runtime().block_on(async {
        let table = keyless_table();
        let reason = match editable(&table, "SELECT * FROM k", false) {
            Editability::ReadOnly(r) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(reason, ReadOnlyReason::NoKey);
        assert!(reason.can_opt_in());
        assert!(matches!(
            editable(&table, "SELECT * FROM k", true),
            Editability::Editable(_)
        ));
    });
}

#[test]
fn keyless_duplicate_rows_roll_back_and_unique_rows_with_nulls_commit() {
    runtime().block_on(async {
        let mut s = session().await;
        exec(&mut *s, "CREATE TABLE k (name TEXT, note TEXT)").await;
        exec(
            &mut *s,
            "INSERT INTO k VALUES ('dup', 'x'), ('dup', 'x'), ('solo', NULL)",
        )
        .await;
        let table = keyless_table();
        let rows = fetch(&mut *s, "SELECT * FROM k").await;

        // Two identical rows: the UPDATE matches 2 rows.
        let mut edits = StagedEdits::new(target(&table, "SELECT * FROM k", true));
        edits.set_cell(ModelRow(0), &rows[0], 1, text("y")).unwrap();
        let outcome = apply(
            &mut *s,
            edits.plan().unwrap().confirm_non_production(),
            &CancelFlag::new(),
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
        let notes = fetch(&mut *s, "SELECT note FROM k WHERE name = 'dup'").await;
        assert_eq!(notes, vec![Row(vec![text("x")]), Row(vec![text("x")])]);
        assert_eq!(edits.change_count(), 1);

        // The same for a DELETE.
        let mut edits = StagedEdits::new(target(&table, "SELECT * FROM k", true));
        edits.delete_row(ModelRow(1), &rows[1]).unwrap();
        let outcome = apply(
            &mut *s,
            edits.plan().unwrap().confirm_non_production(),
            &CancelFlag::new(),
        )
        .await;
        assert!(matches!(
            outcome,
            ApplyOutcome::RolledBack {
                failure: ApplyFailure::RowCount {
                    actual: Some(2),
                    ..
                },
                ..
            }
        ));
        assert_eq!(fetch(&mut *s, "SELECT * FROM k").await.len(), 3);

        // A unique row whose original note is NULL is matched with IS NULL.
        let mut edits = StagedEdits::new(target(&table, "SELECT * FROM k", true));
        edits
            .set_cell(ModelRow(2), &rows[2], 1, text("now"))
            .unwrap();
        let plan = edits.plan().unwrap();
        assert!(plan.statements()[0].query.sql.contains("IS NULL"));
        let outcome = apply(&mut *s, plan.confirm_non_production(), &CancelFlag::new()).await;
        assert_eq!(outcome, ApplyOutcome::Committed { applied: 1 });
        let solo = fetch(&mut *s, "SELECT note FROM k WHERE name = 'solo'").await;
        assert_eq!(solo, vec![Row(vec![text("now")])]);
    });
}

#[test]
fn views_joins_and_unloaded_tables_cannot_be_staged() {
    runtime().block_on(async {
        let table = keyed_table();
        let view = DbObject::new(ObjectKind::View, ObjectPath::root(), "v");
        let cols = columns(&table);
        let e = check_editable(&EditRequest {
            dialect: Some(Dialect::Sqlite),
            source: EditSource::TableData { object: &view },
            table: Some(&table),
            unique_keys: &[],
            columns: &cols,
            allow_keyless: true,
            resolved_schema: None,
        });
        assert!(matches!(
            e,
            Editability::ReadOnly(ReadOnlyReason::NotATable { .. })
        ));
        assert!(matches!(
            editable(&table, "SELECT * FROM t JOIN u USING (id)", true),
            Editability::ReadOnly(ReadOnlyReason::NotSingleTable)
        ));
        assert!(matches!(
            editable(&table, "SELECT id, count(*) FROM t", true),
            Editability::ReadOnly(ReadOnlyReason::NotSingleTable)
        ));
    });
}
