// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Type mapping, row decoding, parameter binding and error mapping.

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};
use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteRow};
use sqlx::{
    AssertSqlSafe, Column as _, Row as _, SqlSafeStr as _, Sqlite, TypeInfo as _, ValueRef as _,
    sqlite::SqliteColumn,
};

/// Maps a declared column type name to a [`DataType`].
pub(crate) fn data_type(name: &str) -> DataType {
    match name.to_ascii_uppercase().as_str() {
        "INTEGER" | "INT" => DataType::Integer,
        "REAL" => DataType::Float,
        "TEXT" => DataType::Text,
        "BLOB" => DataType::Binary,
        "NUMERIC" => DataType::Decimal,
        "BOOLEAN" => DataType::Boolean,
        "DATE" => DataType::Date,
        "TIME" => DataType::Time,
        "DATETIME" => DataType::Timestamp,
        _ => DataType::Other,
    }
}

/// Builds the core [`Column`] for a prepared-statement column.
pub(crate) fn column(col: &SqliteColumn) -> Column {
    let native = col.type_info().name().to_owned();
    Column::new(col.name(), data_type(&native)).with_native_type(native)
}

/// Decodes one row by the runtime storage class of each cell.
pub(crate) fn decode_row(row: &SqliteRow) -> Result<Row, DriverError> {
    let mut values = Vec::with_capacity(row.len());
    for i in 0..row.len() {
        let raw = row.try_get_raw(i).map_err(query_error)?;
        if raw.is_null() {
            values.push(Value::Null);
            continue;
        }
        let class = raw.type_info().name().to_owned();
        drop(raw);
        let value = match class.as_str() {
            "INTEGER" => Value::Int(row.try_get_unchecked::<i64, _>(i).map_err(query_error)?),
            "REAL" => Value::Float(row.try_get_unchecked::<f64, _>(i).map_err(query_error)?),
            "TEXT" => Value::Text(row.try_get_unchecked::<String, _>(i).map_err(query_error)?),
            "BLOB" => Value::Bytes(
                row.try_get_unchecked::<Vec<u8>, _>(i)
                    .map_err(query_error)?,
            ),
            other => {
                return Err(DriverError::Unsupported(format!(
                    "SQLite storage class {other}"
                )));
            }
        };
        values.push(value);
    }
    Ok(Row(values))
}

/// A `sqlx` query with the parameters bound in order.
pub(crate) type BoundQuery = Query<'static, Sqlite, SqliteArguments>;

/// Builds a query for `sql` and binds `params` in order.
///
/// # Errors
/// Returns [`DriverError::Unsupported`] for a value variant this driver cannot bind.
pub(crate) fn bind_params(sql: &str, params: &[Value]) -> Result<BoundQuery, DriverError> {
    let mut query = sqlx::query(AssertSqlSafe(sql.to_owned()));
    for param in params {
        query = match param {
            Value::Null => query.bind(None::<i64>),
            Value::Bool(b) => query.bind(*b),
            Value::Int(i) => query.bind(*i),
            Value::Float(f) => query.bind(*f),
            Value::Text(s) => query.bind(s.clone()),
            Value::Bytes(b) => query.bind(b.clone()),
            other => {
                return Err(DriverError::Unsupported(format!(
                    "SQLite parameter {other:?}"
                )));
            }
        };
    }
    Ok(query)
}

/// Turns SQL text into the `sqlx` string type.
pub(crate) fn sql_str(sql: &str) -> sqlx::SqlStr {
    AssertSqlSafe(sql.to_owned()).into_sql_str()
}

/// Maps a `sqlx` error raised while preparing, executing or fetching.
pub(crate) fn query_error(err: sqlx::Error) -> DriverError {
    match err {
        sqlx::Error::Database(d) => DriverError::Query(d.message().to_owned()),
        other => DriverError::Backend(Box::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types_map() {
        assert_eq!(data_type("INTEGER"), DataType::Integer);
        assert_eq!(data_type("int"), DataType::Integer);
        assert_eq!(data_type("REAL"), DataType::Float);
        assert_eq!(data_type("text"), DataType::Text);
        assert_eq!(data_type("BLOB"), DataType::Binary);
        assert_eq!(data_type("NUMERIC"), DataType::Decimal);
        assert_eq!(data_type("Boolean"), DataType::Boolean);
        assert_eq!(data_type("DATE"), DataType::Date);
        assert_eq!(data_type("TIME"), DataType::Time);
        assert_eq!(data_type("DATETIME"), DataType::Timestamp);
    }

    #[test]
    fn unknown_types_are_other() {
        assert_eq!(data_type("VARCHAR(10)"), DataType::Other);
        assert_eq!(data_type("NULL"), DataType::Other);
        assert_eq!(data_type(""), DataType::Other);
    }

    #[test]
    fn non_database_errors_are_backend() {
        assert!(matches!(
            query_error(sqlx::Error::PoolClosed),
            DriverError::Backend(_)
        ));
    }
}
