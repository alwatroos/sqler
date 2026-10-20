// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Type mapping, row decoding, parameter binding and error mapping.

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{
    Column, DataType, Date, Row, Time, Timestamp, TimestampTz, Value,
};
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
        "DATETIME" | "TIMESTAMP" => DataType::Timestamp,
        _ => DataType::Other,
    }
}

/// Maps a declared column type from the schema to a [`DataType`].
///
/// Exact names first (after lowercasing and dropping a `(..)` suffix), then the `SQLite`
/// affinity rules.
pub(crate) fn declared_type(name: &str) -> DataType {
    let lower = name.to_ascii_lowercase();
    let base = lower.split('(').next().unwrap_or("").trim();
    match base {
        "bool" | "boolean" => return DataType::Boolean,
        "date" => return DataType::Date,
        "time" => return DataType::Time,
        "datetime" | "timestamp" => return DataType::Timestamp,
        "json" | "jsonb" => return DataType::Json,
        "decimal" | "numeric" => return DataType::Decimal,
        _ => {}
    }
    if base.contains("int") {
        DataType::Integer
    } else if base.contains("char") || base.contains("clob") || base.contains("text") {
        DataType::Text
    } else if base.contains("blob") {
        DataType::Binary
    } else if base.contains("real") || base.contains("floa") || base.contains("doub") {
        DataType::Float
    } else {
        DataType::Other
    }
}

/// Parses a TEXT cell by the declared column type, or `None` to keep it as text.
fn typed_text(text: &str, ty: DataType) -> Option<Value> {
    match ty {
        DataType::Date => text.parse::<Date>().ok().map(Value::Date),
        DataType::Time => text.parse::<Time>().ok().map(Value::Time),
        DataType::Timestamp => text
            .parse::<Timestamp>()
            .map(Value::Timestamp)
            .or_else(|_| text.parse::<TimestampTz>().map(Value::TimestampTz))
            .ok(),
        _ => None,
    }
}

/// Builds the core [`Column`] for a prepared-statement column.
pub(crate) fn column(col: &SqliteColumn) -> Column {
    let native = col.type_info().name().to_owned();
    Column::new(col.name(), data_type(&native)).with_native_type(native)
}

/// Decodes one row by the runtime storage class of each cell.
///
/// A TEXT cell in a DATE, TIME or DATETIME column becomes the typed value when it parses and
/// stays `Text` otherwise.
pub(crate) fn decode_row(row: &SqliteRow, types: &[DataType]) -> Result<Row, DriverError> {
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
            "TEXT" => {
                let text = row.try_get_unchecked::<String, _>(i).map_err(query_error)?;
                let ty = types.get(i).copied().unwrap_or(DataType::Other);
                typed_text(&text, ty).unwrap_or(Value::Text(text))
            }
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
            Value::Decimal(v) => query.bind(v.to_string()),
            Value::Date(v) => query.bind(v.to_string()),
            Value::Time(v) => query.bind(v.to_string()),
            Value::Timestamp(v) => query.bind(v.to_string()),
            Value::TimestampTz(v) => query.bind(v.to_string()),
            Value::Json(v) => query.bind(v.clone()),
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
    fn timestamp_decltype_maps() {
        assert_eq!(data_type("TIMESTAMP"), DataType::Timestamp);
    }

    #[test]
    fn declared_type_table() {
        let cases = [
            ("INTEGER", DataType::Integer),
            ("BIGINT", DataType::Integer),
            ("varchar(20)", DataType::Text),
            ("NVARCHAR(5)", DataType::Text),
            ("CLOB", DataType::Text),
            ("TEXT", DataType::Text),
            ("BLOB", DataType::Binary),
            ("REAL", DataType::Float),
            ("FLOAT", DataType::Float),
            ("DOUBLE PRECISION", DataType::Float),
            ("bool", DataType::Boolean),
            ("BOOLEAN", DataType::Boolean),
            ("DATE", DataType::Date),
            ("time", DataType::Time),
            ("DATETIME", DataType::Timestamp),
            ("timestamp", DataType::Timestamp),
            ("JSON", DataType::Json),
            ("jsonb", DataType::Json),
            ("DECIMAL(10,2)", DataType::Decimal),
            ("numeric", DataType::Decimal),
            ("", DataType::Other),
            ("whatever", DataType::Other),
        ];
        for (name, expected) in cases {
            assert_eq!(declared_type(name), expected, "{name}");
        }
    }

    #[test]
    fn typed_text_parses_or_falls_back() {
        assert!(matches!(
            typed_text("2024-01-02", DataType::Date),
            Some(Value::Date(_))
        ));
        assert!(typed_text("garbage", DataType::Date).is_none());
        assert!(matches!(
            typed_text("2024-01-02T03:04:05Z", DataType::Timestamp),
            Some(Value::TimestampTz(_))
        ));
        assert!(typed_text("2024-01-02", DataType::Text).is_none());
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
