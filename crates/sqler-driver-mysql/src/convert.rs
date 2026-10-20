// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Type mapping, row decoding, parameter binding and error mapping.

use std::str::FromStr as _;

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{Column, DataType, Date, Decimal, Row, Time, Timestamp, Value};
use sqlx::error::UnexpectedNullError;
use sqlx::mysql::{MySqlArguments, MySqlColumn, MySqlDatabaseError, MySqlRow};
use sqlx::query::Query;
use sqlx::{
    AssertSqlSafe, Column as _, MySql, Row as _, SqlSafeStr as _, TypeInfo as _, ValueRef as _,
};

/// MySQL error 1317: the statement was interrupted (`KILL QUERY`).
pub(crate) const ER_QUERY_INTERRUPTED: u16 = 1317;

/// How a cell is read off the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wire {
    Bool,
    Int,
    Uint,
    Float,
    Decimal,
    Text,
    Bytes,
    Date,
    DateTime,
    Time,
    Json,
    Other,
}

/// Maps a `sqlx` type name (`MySqlTypeInfo::name()`) to its wire reading.
pub(crate) fn wire(name: &str) -> Wire {
    match name {
        "BOOLEAN" => Wire::Bool,
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => Wire::Int,
        "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
        | "BIGINT UNSIGNED" => Wire::Uint,
        "FLOAT" | "DOUBLE" => Wire::Float,
        "DECIMAL" => Wire::Decimal,
        "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            Wire::Text
        }
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BIT" => {
            Wire::Bytes
        }
        "DATE" => Wire::Date,
        "DATETIME" | "TIMESTAMP" => Wire::DateTime,
        "TIME" => Wire::Time,
        "JSON" => Wire::Json,
        _ => Wire::Other,
    }
}

/// Maps a `sqlx` type name to a [`DataType`].
pub(crate) fn data_type(name: &str) -> DataType {
    match wire(name) {
        Wire::Bool => DataType::Boolean,
        Wire::Int | Wire::Uint => DataType::Integer,
        Wire::Float => DataType::Float,
        Wire::Decimal => DataType::Decimal,
        Wire::Text => DataType::Text,
        Wire::Bytes => DataType::Binary,
        Wire::Date => DataType::Date,
        Wire::DateTime => DataType::Timestamp,
        Wire::Time => DataType::Time,
        Wire::Json => DataType::Json,
        Wire::Other => DataType::Other,
    }
}

/// Maps a declared column type from `information_schema.COLUMNS` to a [`DataType`].
///
/// `data_type` is `DATA_TYPE`, `column_type` is `COLUMN_TYPE`. `tinyint(1)` is a boolean and
/// `json` is JSON.
pub(crate) fn declared_type(data_type: &str, column_type: &str) -> DataType {
    let base = data_type.to_ascii_lowercase();
    let full = column_type.to_ascii_lowercase();
    match base.as_str() {
        "tinyint" if full.starts_with("tinyint(1)") => DataType::Boolean,
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" => {
            DataType::Integer
        }
        "float" | "double" | "real" => DataType::Float,
        "decimal" | "numeric" => DataType::Decimal,
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" => {
            DataType::Text
        }
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" | "bit" => {
            DataType::Binary
        }
        "date" => DataType::Date,
        "datetime" | "timestamp" => DataType::Timestamp,
        "time" => DataType::Time,
        "json" => DataType::Json,
        _ => DataType::Other,
    }
}

/// Builds the core [`Column`] for a prepared-statement column.
pub(crate) fn column(col: &MySqlColumn) -> Column {
    let native = col.type_info().name().to_owned();
    Column::new(col.name(), data_type(&native)).with_native_type(native)
}

/// Text from bytes: `Text` when valid UTF-8, `Bytes` otherwise.
fn text_or_bytes(bytes: Vec<u8>) -> Value {
    match String::from_utf8(bytes) {
        Ok(text) => Value::Text(text),
        Err(e) => Value::Bytes(e.into_bytes()),
    }
}

/// A temporal cell in the text format (first byte above 12) or `Bytes` when not UTF-8.
fn temporal_text<T>(buf: &[u8], typed: impl FnOnce(T) -> Value) -> Value
where
    T: std::str::FromStr,
{
    match std::str::from_utf8(buf) {
        Ok(text) if !text.is_empty() => text
            .parse::<T>()
            .map_or_else(|_| Value::Text(text.to_owned()), typed),
        _ => Value::Bytes(buf.to_vec()),
    }
}

/// Whether `buf` is a length-prefixed binary value (first byte `0..=12`) of exactly the
/// length its prefix says.
fn binary_len(buf: &[u8]) -> Option<usize> {
    let &len = buf.first()?;
    (len <= 12).then_some(usize::from(len))
}

fn u16_le(buf: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(buf.get(at..at + 2)?.try_into().ok()?))
}

fn u32_le(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(buf.get(at..at + 4)?.try_into().ok()?))
}

/// The date part of a binary buffer at `at`: year, month, day.
fn ymd(buf: &[u8], at: usize) -> Option<(u16, u8, u8)> {
    Some((u16_le(buf, at)?, *buf.get(at + 2)?, *buf.get(at + 3)?))
}

fn date_text(year: u16, month: u8, day: u8) -> String {
    format!("{year:04}-{month:02}-{day:02}")
}

/// Decodes a `DATE` cell: binary layout `len(0|4) year(2) month day`, else text.
pub(crate) fn decode_date(buf: &[u8]) -> Value {
    let Some(len) = binary_len(buf) else {
        return temporal_text::<Date>(buf, Value::Date);
    };
    match (len, buf.len()) {
        (0, 1) => Value::Text(date_text(0, 0, 0)),
        (4, 5) => ymd(buf, 1).map_or_else(
            || Value::Bytes(buf.to_vec()),
            |(y, m, d)| {
                Date::new(i32::from(y), m, d)
                    .map_or_else(|_| Value::Text(date_text(y, m, d)), Value::Date)
            },
        ),
        _ => Value::Bytes(buf.to_vec()),
    }
}

/// The clock part of a binary cell.
#[derive(Clone, Copy)]
struct Clock {
    hour: u8,
    minute: u8,
    second: u8,
    micros: u32,
}

impl Clock {
    /// `.ffffff`, or nothing when there is no fraction.
    fn fraction(self) -> String {
        if self.micros == 0 {
            String::new()
        } else {
            format!(".{:06}", self.micros)
        }
    }

    fn to_time(self) -> Option<Time> {
        Time::new(
            self.hour,
            self.minute,
            self.second,
            self.micros.checked_mul(1000)?,
        )
        .ok()
    }
}

/// Decodes a `DATETIME` or `TIMESTAMP` cell: binary layout
/// `len(0|4|7|11) year(2) month day [hour minute second [microsecond(4)]]`, else text.
pub(crate) fn decode_datetime(buf: &[u8]) -> Value {
    let Some(len) = binary_len(buf) else {
        return temporal_text::<Timestamp>(buf, Value::Timestamp);
    };
    if buf.len() != len + 1 || !matches!(len, 0 | 4 | 7 | 11) {
        return Value::Bytes(buf.to_vec());
    }
    if len == 0 {
        return Value::Text(format!("{} 00:00:00", date_text(0, 0, 0)));
    }
    let parts = (|| {
        let day = ymd(buf, 1)?;
        let mut clock = Clock {
            hour: 0,
            minute: 0,
            second: 0,
            micros: 0,
        };
        if len >= 7 {
            clock.hour = *buf.get(5)?;
            clock.minute = *buf.get(6)?;
            clock.second = *buf.get(7)?;
        }
        if len >= 11 {
            clock.micros = u32_le(buf, 8)?;
        }
        Some((day, clock))
    })();
    let Some(((year, month, day), clock)) = parts else {
        return Value::Bytes(buf.to_vec());
    };
    let typed = Date::new(i32::from(year), month, day)
        .ok()
        .zip(clock.to_time())
        .map(|(date, time)| Value::Timestamp(Timestamp::new(date, time)));
    typed.unwrap_or_else(|| {
        Value::Text(format!(
            "{} {:02}:{:02}:{:02}{}",
            date_text(year, month, day),
            clock.hour,
            clock.minute,
            clock.second,
            clock.fraction()
        ))
    })
}

/// Decodes a `TIME` cell: binary layout
/// `len(0|8|12) negative(1) days(4) hour minute second [microsecond(4)]`, else text.
///
/// Negative values and values of 24 hours or more do not fit a time of day and come back as
/// `Text`, for example `-838:59:59`.
pub(crate) fn decode_time(buf: &[u8]) -> Value {
    let Some(len) = binary_len(buf) else {
        return temporal_text::<Time>(buf, Value::Time);
    };
    if buf.len() != len + 1 {
        return Value::Bytes(buf.to_vec());
    }
    if len == 0 {
        return Time::new(0, 0, 0, 0).map_or_else(|_| Value::Bytes(buf.to_vec()), Value::Time);
    }
    if len != 8 && len != 12 {
        return Value::Bytes(buf.to_vec());
    }
    let parts = (|| {
        let negative = *buf.get(1)? != 0;
        let days = u32_le(buf, 2)?;
        let micros = if len == 12 { u32_le(buf, 9)? } else { 0 };
        let clock = Clock {
            hour: *buf.get(6)?,
            minute: *buf.get(7)?,
            second: *buf.get(8)?,
            micros,
        };
        Some((negative, days, clock))
    })();
    let Some((negative, days, clock)) = parts else {
        return Value::Bytes(buf.to_vec());
    };
    if let Some(time) = clock.to_time().filter(|_| !negative && days == 0) {
        return Value::Time(time);
    }
    let hours = u64::from(days) * 24 + u64::from(clock.hour);
    Value::Text(format!(
        "{}{hours:02}:{:02}:{:02}{}",
        if negative { "-" } else { "" },
        clock.minute,
        clock.second,
        clock.fraction()
    ))
}

fn decode_error(err: sqlx::Error) -> DriverError {
    query_error(err)
}

/// Decodes one row by the type the server sent with it.
///
/// The type comes from the row's own column metadata, which is what the binary encoding of the
/// cells follows. For a plain column it equals the prepared type; for an expression over a
/// parameter (`select ?`) it can be more precise.
pub(crate) fn decode_row(row: &MySqlRow) -> Result<Row, DriverError> {
    let mut values = Vec::with_capacity(row.len());
    for i in 0..row.len() {
        let wire = wire(row.column(i).type_info().name());
        if matches!(wire, Wire::Date | Wire::DateTime) {
            // `sqlx` reports a zero date (a leading 0 byte) as NULL, so ask for the bytes and
            // treat only a missing value as NULL.
            values.push(match row.try_get_unchecked::<Vec<u8>, _>(i) {
                Ok(bytes) if wire == Wire::Date => decode_date(&bytes),
                Ok(bytes) => decode_datetime(&bytes),
                Err(sqlx::Error::ColumnDecode { source, .. })
                    if source.is::<UnexpectedNullError>() =>
                {
                    Value::Null
                }
                Err(e) => return Err(decode_error(e)),
            });
        } else if row.try_get_raw(i).map_err(decode_error)?.is_null() {
            values.push(Value::Null);
        } else {
            values.push(decode_cell(row, i, wire)?);
        }
    }
    Ok(Row(values))
}

fn decode_cell(row: &MySqlRow, i: usize, wire: Wire) -> Result<Value, DriverError> {
    let bytes = |row: &MySqlRow| row.try_get_unchecked::<Vec<u8>, _>(i).map_err(decode_error);
    Ok(match wire {
        Wire::Bool => Value::Bool(row.try_get_unchecked::<i64, _>(i).map_err(decode_error)? != 0),
        Wire::Int => Value::Int(row.try_get_unchecked::<i64, _>(i).map_err(decode_error)?),
        Wire::Uint => {
            let v = row.try_get_unchecked::<u64, _>(i).map_err(decode_error)?;
            i64::try_from(v).map_or_else(
                |_| {
                    let text = v.to_string();
                    Decimal::from_str(&text).map_or(Value::Text(text), Value::Decimal)
                },
                Value::Int,
            )
        }
        Wire::Float => Value::Float(row.try_get_unchecked::<f64, _>(i).map_err(decode_error)?),
        Wire::Decimal => match String::from_utf8(bytes(row)?) {
            Ok(text) => Decimal::from_str(&text).map_or(Value::Text(text), Value::Decimal),
            Err(e) => Value::Bytes(e.into_bytes()),
        },
        Wire::Text => text_or_bytes(bytes(row)?),
        Wire::Json => match String::from_utf8(bytes(row)?) {
            Ok(text) => Value::Json(text),
            Err(e) => Value::Bytes(e.into_bytes()),
        },
        Wire::Bytes | Wire::Other => Value::Bytes(bytes(row)?),
        Wire::Date => decode_date(&bytes(row)?),
        Wire::DateTime => decode_datetime(&bytes(row)?),
        Wire::Time => decode_time(&bytes(row)?),
    })
}

/// A `sqlx` query with the parameters bound in order.
pub(crate) type BoundQuery = Query<'static, MySql, MySqlArguments>;

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
                    "MySQL parameter {other:?}"
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

/// The MySQL error number of a database error, if `err` is one.
pub(crate) fn mysql_code(err: &sqlx::Error) -> Option<u16> {
    err.as_database_error()?
        .try_downcast_ref::<MySqlDatabaseError>()
        .map(MySqlDatabaseError::number)
}

/// Maps a `sqlx` error raised while preparing, executing or fetching.
pub(crate) fn query_error(err: sqlx::Error) -> DriverError {
    match err {
        sqlx::Error::Database(d) => DriverError::Query(d.message().to_owned()),
        other => DriverError::Backend(Box::new(other)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn type_names_map() {
        let cases = [
            ("BOOLEAN", DataType::Boolean, Wire::Bool),
            ("TINYINT", DataType::Integer, Wire::Int),
            ("SMALLINT", DataType::Integer, Wire::Int),
            ("MEDIUMINT", DataType::Integer, Wire::Int),
            ("INT", DataType::Integer, Wire::Int),
            ("BIGINT", DataType::Integer, Wire::Int),
            ("YEAR", DataType::Integer, Wire::Int),
            ("TINYINT UNSIGNED", DataType::Integer, Wire::Uint),
            ("SMALLINT UNSIGNED", DataType::Integer, Wire::Uint),
            ("MEDIUMINT UNSIGNED", DataType::Integer, Wire::Uint),
            ("INT UNSIGNED", DataType::Integer, Wire::Uint),
            ("BIGINT UNSIGNED", DataType::Integer, Wire::Uint),
            ("FLOAT", DataType::Float, Wire::Float),
            ("DOUBLE", DataType::Float, Wire::Float),
            ("DECIMAL", DataType::Decimal, Wire::Decimal),
            ("CHAR", DataType::Text, Wire::Text),
            ("VARCHAR", DataType::Text, Wire::Text),
            ("TINYTEXT", DataType::Text, Wire::Text),
            ("TEXT", DataType::Text, Wire::Text),
            ("MEDIUMTEXT", DataType::Text, Wire::Text),
            ("LONGTEXT", DataType::Text, Wire::Text),
            ("ENUM", DataType::Text, Wire::Text),
            ("SET", DataType::Text, Wire::Text),
            ("BINARY", DataType::Binary, Wire::Bytes),
            ("VARBINARY", DataType::Binary, Wire::Bytes),
            ("TINYBLOB", DataType::Binary, Wire::Bytes),
            ("BLOB", DataType::Binary, Wire::Bytes),
            ("MEDIUMBLOB", DataType::Binary, Wire::Bytes),
            ("LONGBLOB", DataType::Binary, Wire::Bytes),
            ("BIT", DataType::Binary, Wire::Bytes),
            ("DATE", DataType::Date, Wire::Date),
            ("DATETIME", DataType::Timestamp, Wire::DateTime),
            ("TIMESTAMP", DataType::Timestamp, Wire::DateTime),
            ("TIME", DataType::Time, Wire::Time),
            ("JSON", DataType::Json, Wire::Json),
            ("GEOMETRY", DataType::Other, Wire::Other),
            ("NULL", DataType::Other, Wire::Other),
            ("", DataType::Other, Wire::Other),
        ];
        for (name, ty, w) in cases {
            assert_eq!(data_type(name), ty, "{name}");
            assert_eq!(wire(name), w, "{name}");
        }
    }

    #[test]
    fn declared_types_map() {
        let cases = [
            ("tinyint", "tinyint(1)", DataType::Boolean),
            ("tinyint", "tinyint(1) unsigned", DataType::Boolean),
            ("tinyint", "tinyint", DataType::Integer),
            ("tinyint", "tinyint(4)", DataType::Integer),
            ("smallint", "smallint", DataType::Integer),
            ("mediumint", "mediumint", DataType::Integer),
            ("int", "int unsigned", DataType::Integer),
            ("bigint", "bigint", DataType::Integer),
            ("year", "year", DataType::Integer),
            ("float", "float", DataType::Float),
            ("double", "double", DataType::Float),
            ("decimal", "decimal(10,2)", DataType::Decimal),
            ("char", "char(3)", DataType::Text),
            ("varchar", "varchar(20)", DataType::Text),
            ("text", "text", DataType::Text),
            ("longtext", "longtext", DataType::Text),
            ("enum", "enum('a','b')", DataType::Text),
            ("set", "set('a','b')", DataType::Text),
            ("binary", "binary(4)", DataType::Binary),
            ("varbinary", "varbinary(4)", DataType::Binary),
            ("blob", "blob", DataType::Binary),
            ("bit", "bit(8)", DataType::Binary),
            ("date", "date", DataType::Date),
            ("datetime", "datetime(6)", DataType::Timestamp),
            ("timestamp", "timestamp", DataType::Timestamp),
            ("time", "time", DataType::Time),
            ("json", "json", DataType::Json),
            ("geometry", "geometry", DataType::Other),
            ("JSON", "JSON", DataType::Json),
        ];
        for (data, column, expected) in cases {
            assert_eq!(declared_type(data, column), expected, "{data} {column}");
        }
    }

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    #[test]
    fn date_binary_lengths() {
        assert_eq!(
            decode_date(&[0]),
            Value::Text("0000-00-00".to_owned()),
            "zero date"
        );
        // 2024-02-29
        let mut buf = vec![4];
        buf.extend(2024u16.to_le_bytes());
        buf.extend([2, 29]);
        assert_eq!(decode_date(&buf), Value::Date(date(2024, 2, 29)));
        // 2024-00-00 does not fit a calendar date.
        let mut buf = vec![4];
        buf.extend(2024u16.to_le_bytes());
        buf.extend([0, 0]);
        assert_eq!(decode_date(&buf), Value::Text("2024-00-00".to_owned()));
        // Wrong length for the prefix.
        assert!(matches!(decode_date(&[4, 1, 2]), Value::Bytes(_)));
        assert!(matches!(decode_date(&[7]), Value::Bytes(_)));
    }

    #[test]
    fn date_text_fallback() {
        assert_eq!(decode_date(b"2024-01-02"), Value::Date(date(2024, 1, 2)));
        assert_eq!(
            decode_date(b"0000-00-00"),
            Value::Text("0000-00-00".to_owned())
        );
        assert!(matches!(decode_date(&[0xff, 0xfe]), Value::Bytes(_)));
    }

    fn datetime_buf(len: u8, h: u8, mi: u8, s: u8, micros: u32) -> Vec<u8> {
        let mut buf = vec![len];
        buf.extend(2024u16.to_le_bytes());
        buf.extend([3, 4]);
        if len >= 7 {
            buf.extend([h, mi, s]);
        }
        if len >= 11 {
            buf.extend(micros.to_le_bytes());
        }
        buf
    }

    #[test]
    fn datetime_binary_lengths() {
        assert_eq!(
            decode_datetime(&[0]),
            Value::Text("0000-00-00 00:00:00".to_owned())
        );
        let ts = |h, mi, s, nanos| {
            Value::Timestamp(Timestamp::new(
                date(2024, 3, 4),
                Time::new(h, mi, s, nanos).unwrap(),
            ))
        };
        assert_eq!(
            decode_datetime(&datetime_buf(4, 0, 0, 0, 0)),
            ts(0, 0, 0, 0)
        );
        assert_eq!(
            decode_datetime(&datetime_buf(7, 5, 6, 7, 0)),
            ts(5, 6, 7, 0)
        );
        assert_eq!(
            decode_datetime(&datetime_buf(11, 5, 6, 7, 123_456)),
            ts(5, 6, 7, 123_456_000)
        );
    }

    #[test]
    fn datetime_edge_cases() {
        // Zero month and day with a time part.
        let mut buf = vec![7];
        buf.extend(2024u16.to_le_bytes());
        buf.extend([0, 0, 1, 2, 3]);
        assert_eq!(
            decode_datetime(&buf),
            Value::Text("2024-00-00 01:02:03".to_owned())
        );
        // Out of range hour keeps the text, with microseconds.
        let buf = datetime_buf(11, 25, 0, 0, 5);
        assert_eq!(
            decode_datetime(&buf),
            Value::Text("2024-03-04 25:00:00.000005".to_owned())
        );
        assert!(matches!(decode_datetime(&[7, 1, 2]), Value::Bytes(_)));
        assert!(matches!(
            decode_datetime(&[5, 1, 2, 3, 4, 5]),
            Value::Bytes(_)
        ));
        assert_eq!(
            decode_datetime(b"2024-03-04 05:06:07.5"),
            Value::Timestamp(Timestamp::new(
                date(2024, 3, 4),
                Time::new(5, 6, 7, 500_000_000).unwrap()
            ))
        );
        assert_eq!(
            decode_datetime(b"0000-00-00 00:00:00"),
            Value::Text("0000-00-00 00:00:00".to_owned())
        );
    }

    fn time_buf(negative: bool, days: u32, h: u8, m: u8, s: u8, micros: Option<u32>) -> Vec<u8> {
        let mut buf = vec![if micros.is_some() { 12 } else { 8 }, u8::from(negative)];
        buf.extend(days.to_le_bytes());
        buf.extend([h, m, s]);
        if let Some(micros) = micros {
            buf.extend(micros.to_le_bytes());
        }
        buf
    }

    #[test]
    fn time_binary_lengths() {
        assert_eq!(
            decode_time(&[0]),
            Value::Time(Time::new(0, 0, 0, 0).unwrap())
        );
        assert_eq!(
            decode_time(&time_buf(false, 0, 1, 2, 3, None)),
            Value::Time(Time::new(1, 2, 3, 0).unwrap())
        );
        assert_eq!(
            decode_time(&time_buf(false, 0, 23, 59, 59, Some(999_999))),
            Value::Time(Time::new(23, 59, 59, 999_999_000).unwrap())
        );
    }

    #[test]
    fn time_outside_time_of_day_is_text() {
        assert_eq!(
            decode_time(&time_buf(true, 0, 1, 0, 0, None)),
            Value::Text("-01:00:00".to_owned())
        );
        assert_eq!(
            decode_time(&time_buf(true, 34, 22, 59, 59, None)),
            Value::Text("-838:59:59".to_owned())
        );
        assert_eq!(
            decode_time(&time_buf(false, 1, 1, 0, 0, Some(5))),
            Value::Text("25:00:00.000005".to_owned())
        );
        assert!(matches!(decode_time(&[8, 0, 1]), Value::Bytes(_)));
        assert!(matches!(decode_time(&[4, 0, 0, 0, 0]), Value::Bytes(_)));
    }

    #[test]
    fn time_text_fallback() {
        assert_eq!(
            decode_time(b"12:34:56"),
            Value::Time(Time::new(12, 34, 56, 0).unwrap())
        );
        assert_eq!(
            decode_time(b"-838:59:59"),
            Value::Text("-838:59:59".to_owned())
        );
        assert!(matches!(decode_time(&[0xff]), Value::Bytes(_)));
        assert!(matches!(decode_time(&[]), Value::Bytes(_)));
    }

    #[test]
    fn text_or_bytes_checks_utf8() {
        assert_eq!(
            text_or_bytes(b"abc".to_vec()),
            Value::Text("abc".to_owned())
        );
        assert_eq!(
            text_or_bytes(vec![0xff, 0xfe]),
            Value::Bytes(vec![0xff, 0xfe])
        );
    }

    #[test]
    fn binds_every_supported_variant() {
        let params = [
            Value::Null,
            Value::Bool(true),
            Value::Int(1),
            Value::Float(1.5),
            Value::Text("a".into()),
            Value::Bytes(vec![1]),
            Value::Decimal("1.5".parse().unwrap()),
            Value::Date(date(2024, 1, 2)),
            Value::Time(Time::new(1, 2, 3, 0).unwrap()),
            Value::Timestamp(Timestamp::new(
                date(2024, 1, 2),
                Time::new(1, 2, 3, 0).unwrap(),
            )),
            Value::Json("{}".into()),
        ];
        assert!(bind_params("select ?", &params).is_ok());
    }

    #[test]
    fn non_database_errors_are_backend() {
        assert!(matches!(
            query_error(sqlx::Error::PoolClosed),
            DriverError::Backend(_)
        ));
        assert_eq!(mysql_code(&sqlx::Error::PoolClosed), None);
    }
}
