// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What a guest driver sends, as core types, and the checks on a batch of rows.

use sqler_driver_api::sqler_core::{
    Column, ColumnInfo, DataType, DbObject, ObjectKind, Row, Value,
};

use crate::query::{MAX_RESULT_BYTES, cell_cost, value_from_wit};
use crate::runtime::wit::types as wit;

/// Rows asked of the guest per `next-batch` call, and the most it may send back.
pub(crate) const BATCH_ROWS: u32 = 500;

/// The core column type.
pub(crate) fn data_type_from_wit(t: wit::DataType) -> DataType {
    match t {
        wit::DataType::Boolean => DataType::Boolean,
        wit::DataType::Integer => DataType::Integer,
        wit::DataType::Float => DataType::Float,
        wit::DataType::Decimal => DataType::Decimal,
        wit::DataType::Text => DataType::Text,
        wit::DataType::Binary => DataType::Binary,
        wit::DataType::Date => DataType::Date,
        wit::DataType::Time => DataType::Time,
        wit::DataType::Timestamp => DataType::Timestamp,
        wit::DataType::TimestampTz => DataType::TimestampTz,
        wit::DataType::Json => DataType::Json,
        wit::DataType::Other => DataType::Other,
    }
}

/// The core object kind.
pub(crate) fn kind_from_wit(k: wit::ObjectKind) -> ObjectKind {
    match k {
        wit::ObjectKind::Database => ObjectKind::Database,
        wit::ObjectKind::Schema => ObjectKind::Schema,
        wit::ObjectKind::Table => ObjectKind::Table,
        wit::ObjectKind::View => ObjectKind::View,
        wit::ObjectKind::MaterializedView => ObjectKind::MaterializedView,
        wit::ObjectKind::Function => ObjectKind::Function,
        wit::ObjectKind::Procedure => ObjectKind::Procedure,
        wit::ObjectKind::Trigger => ObjectKind::Trigger,
        wit::ObjectKind::Sequence => ObjectKind::Sequence,
        wit::ObjectKind::Type => ObjectKind::Type,
        wit::ObjectKind::Index => ObjectKind::Index,
        wit::ObjectKind::Extension => ObjectKind::Extension,
        wit::ObjectKind::Role => ObjectKind::Role,
    }
}

/// A result column. An empty type name is no native type.
pub(crate) fn column_from_wit(c: wit::Column) -> Column {
    let column = Column::new(c.name, data_type_from_wit(c.data_type));
    if c.type_name.is_empty() {
        column
    } else {
        column.with_native_type(c.type_name)
    }
}

/// A table column of the catalog.
pub(crate) fn column_info_from_wit(c: super::wit_catalog::TableColumn) -> ColumnInfo {
    let mut info =
        ColumnInfo::new(c.name, data_type_from_wit(c.data_type)).with_nullable(c.nullable);
    if !c.type_name.is_empty() {
        info = info.with_native_type(c.type_name);
    }
    if let Some(d) = c.default {
        info = info.with_default(d);
    }
    info
}

/// A catalog object of the guest.
pub(crate) fn object_from_wit(o: wit::DbObject) -> DbObject {
    DbObject::new(
        kind_from_wit(o.kind),
        crate::query::path_from_wit(o.parent),
        o.name,
    )
}

/// Why a batch was refused. The user sees only the fixed text; the variant is for the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchError {
    TooManyRows,
    Width,
    TooManyBytes,
    BadValue,
}

/// Checks a batch against the rules of the stream and converts it: at most [`BATCH_ROWS`] rows, every row as wide
/// as the columns, the summed cell cost within [`MAX_RESULT_BYTES`], and every temporal or decimal value parses.
pub(crate) fn rows_from_wit(
    batch: Vec<Vec<wit::Value>>,
    width: usize,
) -> Result<Vec<Row>, BatchError> {
    if batch.len() > BATCH_ROWS as usize {
        return Err(BatchError::TooManyRows);
    }
    let mut bytes = 0_usize;
    let mut rows = Vec::with_capacity(batch.len());
    for row in batch {
        if row.len() != width {
            return Err(BatchError::Width);
        }
        let mut values: Vec<Value> = Vec::with_capacity(width);
        for cell in row {
            bytes = bytes.saturating_add(cell_cost(&cell));
            if bytes > MAX_RESULT_BYTES {
                return Err(BatchError::TooManyBytes);
            }
            values.push(value_from_wit(cell).map_err(|_| BatchError::BadValue)?);
        }
        rows.push(Row(values));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPES: [wit::DataType; 12] = [
        wit::DataType::Boolean,
        wit::DataType::Integer,
        wit::DataType::Float,
        wit::DataType::Decimal,
        wit::DataType::Text,
        wit::DataType::Binary,
        wit::DataType::Date,
        wit::DataType::Time,
        wit::DataType::Timestamp,
        wit::DataType::TimestampTz,
        wit::DataType::Json,
        wit::DataType::Other,
    ];

    #[test]
    fn data_types_round_trip() {
        for t in TYPES {
            let core = data_type_from_wit(t);
            assert_eq!(
                crate::query::type_name(None, core),
                match t {
                    wit::DataType::Boolean => "boolean",
                    wit::DataType::Integer => "integer",
                    wit::DataType::Float => "float",
                    wit::DataType::Decimal => "decimal",
                    wit::DataType::Text => "text",
                    wit::DataType::Binary => "binary",
                    wit::DataType::Date => "date",
                    wit::DataType::Time => "time",
                    wit::DataType::Timestamp => "timestamp",
                    wit::DataType::TimestampTz => "timestamp-tz",
                    wit::DataType::Json => "json",
                    wit::DataType::Other => "other",
                }
            );
        }
    }

    #[test]
    fn every_object_kind_round_trips() {
        for k in ObjectKind::ALL {
            let wit = crate::query::kind_to_wit(k).expect("known kind");
            assert_eq!(kind_from_wit(wit), k);
        }
    }

    #[test]
    fn an_empty_type_name_is_no_native_type() {
        let c = column_from_wit(wit::Column {
            name: "a".into(),
            data_type: wit::DataType::Integer,
            type_name: String::new(),
        });
        assert_eq!(c.native_type, None);
        let c = column_from_wit(wit::Column {
            name: "a".into(),
            data_type: wit::DataType::Integer,
            type_name: "INT4".into(),
        });
        assert_eq!(c.native_type.as_deref(), Some("INT4"));
    }

    fn int_row(n: usize) -> Vec<wit::Value> {
        vec![wit::Value::Integer(1); n]
    }

    #[test]
    fn a_good_batch_converts() {
        let rows = rows_from_wit(vec![int_row(2), int_row(2)], 2).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, vec![Value::Int(1), Value::Int(1)]);
        assert_eq!(rows_from_wit(Vec::new(), 3), Ok(Vec::new()));
    }

    #[test]
    fn too_many_rows_are_refused() {
        let ok = vec![int_row(1); BATCH_ROWS as usize];
        assert!(rows_from_wit(ok, 1).is_ok());
        let over = vec![int_row(1); BATCH_ROWS as usize + 1];
        assert_eq!(rows_from_wit(over, 1), Err(BatchError::TooManyRows));
    }

    #[test]
    fn a_wrong_width_is_refused() {
        assert_eq!(
            rows_from_wit(vec![int_row(2), int_row(1)], 2),
            Err(BatchError::Width)
        );
    }

    #[test]
    fn too_many_bytes_are_refused() {
        let big = wit::Value::Text("x".repeat(MAX_RESULT_BYTES / 2));
        let batch = vec![vec![big.clone()], vec![big.clone()], vec![big]];
        assert_eq!(rows_from_wit(batch, 1), Err(BatchError::TooManyBytes));
    }

    #[test]
    fn a_bad_temporal_value_is_refused() {
        let batch = vec![vec![wit::Value::Date("2024-13-40".into())]];
        assert_eq!(rows_from_wit(batch, 1), Err(BatchError::BadValue));
    }
}
