// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Fixtures shared by the unit tests of this module.

use sqler_driver_api::sqler_core::{
    Column, ColumnInfo, DataType, DbObject, ObjectKind, ObjectPath, PrimaryKey, Row, Table, Value,
};
use sqler_sql::Dialect;

use super::{EditRequest, EditSource, EditTarget, Editability, check_editable};

/// An editable `SQLite` target over `t(id integer not null, name text)`; keyed by `id`, or keyless
/// with the opt-in.
pub(crate) fn target(keyless: bool) -> EditTarget {
    target_for(Dialect::Sqlite, keyless)
}

/// [`target`] for another dialect (the table is `main.t`, or `public.t` on `PostgreSQL`).
pub(crate) fn target_for(dialect: Dialect, keyless: bool) -> EditTarget {
    let schema = if dialect == Dialect::Postgres {
        "public"
    } else {
        "main"
    };
    let mut table = Table::new(DbObject::new(
        ObjectKind::Table,
        ObjectPath::root().in_schema(schema),
        "t",
    ))
    .with_columns(vec![
        ColumnInfo::new("id", DataType::Integer).with_nullable(false),
        ColumnInfo::new("name", DataType::Text),
    ]);
    if !keyless {
        table = table.with_primary_key(PrimaryKey::new(vec!["id".to_owned()]));
    }
    let columns = [
        Column::new("id", DataType::Integer),
        Column::new("name", DataType::Text),
    ];
    let e = check_editable(&EditRequest {
        dialect: Some(dialect),
        source: EditSource::QueryResult {
            sql: "SELECT * FROM t",
        },
        table: Some(&table),
        unique_keys: &[],
        columns: &columns,
        allow_keyless: keyless,
        resolved_schema: Some(schema),
    });
    match e {
        Editability::Editable(t) => t,
        other => panic!("{other:?}"),
    }
}

/// A row `(id, name)`.
pub(crate) fn row(id: i64, name: Option<&str>) -> Row {
    Row(vec![
        Value::Int(id),
        name.map_or(Value::Null, |n| Value::Text(n.to_owned())),
    ])
}

/// A loaded model of `(id, name)` rows, in the state `state`.
pub(crate) fn model_of(rows: Vec<Row>, state: crate::grid::IngestState) -> crate::grid::GridModel {
    use crate::grid::model::Msg;
    use crate::grid::store::Chunk;
    use crate::grid::{IngestState, grid_channel};

    let (mut model, sink) = grid_channel();
    let columns = vec![
        Column::new("id", DataType::Integer),
        Column::new("name", DataType::Text),
    ];
    assert!(sink.send(Msg::Columns(columns)));
    if !rows.is_empty() {
        assert!(sink.send(Msg::Rows(Chunk::from_rows(rows, 2))));
    }
    assert!(sink.send(match state {
        IngestState::Paused => Msg::Paused,
        IngestState::Cancelled => Msg::Cancelled,
        IngestState::Failed(m) => Msg::Failed(m),
        _ => Msg::Done(None),
    }));
    model.poll(usize::MAX);
    // A paused model must not see its sender disappear (that reads as a failed ingestion).
    std::mem::forget(sink);
    model
}
