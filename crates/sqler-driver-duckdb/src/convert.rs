// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Arrow to `Value` conversion, parameter mapping and column metadata.
//!
//! `DuckDB` hands results over as Arrow record batches. `DECIMAL(38)` and `HUGEINT` arrive as
//! `Decimal128` and are rendered from the integer and scale, so they stay exact.

use duckdb::arrow::array::{Array, ArrayRef, AsArray, RecordBatch};
use duckdb::arrow::datatypes::{
    DataType as ArrowType, Date32Type, Date64Type, Decimal128Type, Decimal256Type, Float16Type,
    Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type, Schema, TimeUnit,
    UInt8Type, UInt16Type, UInt32Type, UInt64Type,
};
use duckdb::arrow::util::display::{ArrayFormatter, FormatOptions};
use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{
    Column, DataType, Date, Row, Time, Timestamp, TimestampTz, Value,
};

/// Maps an Arrow type to a [`DataType`].
pub(crate) fn data_type(ty: &ArrowType) -> DataType {
    match ty {
        ArrowType::Boolean => DataType::Boolean,
        ArrowType::Int8
        | ArrowType::Int16
        | ArrowType::Int32
        | ArrowType::Int64
        | ArrowType::UInt8
        | ArrowType::UInt16
        | ArrowType::UInt32
        | ArrowType::UInt64 => DataType::Integer,
        ArrowType::Float16 | ArrowType::Float32 | ArrowType::Float64 => DataType::Float,
        ArrowType::Decimal128(..) | ArrowType::Decimal256(..) => DataType::Decimal,
        ArrowType::Utf8 | ArrowType::LargeUtf8 | ArrowType::Utf8View => DataType::Text,
        ArrowType::Binary
        | ArrowType::LargeBinary
        | ArrowType::FixedSizeBinary(_)
        | ArrowType::BinaryView => DataType::Binary,
        ArrowType::Date32 | ArrowType::Date64 => DataType::Date,
        ArrowType::Time32(_) | ArrowType::Time64(_) => DataType::Time,
        ArrowType::Timestamp(_, None) => DataType::Timestamp,
        ArrowType::Timestamp(_, Some(_)) => DataType::TimestampTz,
        _ => DataType::Other,
    }
}

/// The result columns of an executed statement. `DuckDB` reports no native type name here.
pub(crate) fn columns(schema: &Schema) -> Vec<Column> {
    schema
        .fields()
        .iter()
        .map(|f| Column::new(f.name().clone(), data_type(f.data_type())))
        .collect()
}

/// Maps a bound sqler value to a `DuckDB` value. Temporal and decimal values go as ISO text.
pub(crate) fn param(value: &Value) -> duckdb::types::Value {
    use duckdb::types::Value as D;
    match value {
        Value::Null => D::Null,
        Value::Bool(b) => D::Boolean(*b),
        Value::Int(i) => D::BigInt(*i),
        Value::Float(f) => D::Double(*f),
        Value::Text(s) | Value::Json(s) => D::Text(s.clone()),
        Value::Bytes(b) => D::Blob(b.clone()),
        Value::Decimal(d) => D::Text(d.to_string()),
        Value::Date(d) => D::Text(d.to_string()),
        Value::Time(t) => D::Text(t.to_string()),
        Value::Timestamp(t) => D::Text(t.to_string()),
        Value::TimestampTz(t) => D::Text(t.to_string()),
        // `Value` is non-exhaustive: an unknown future variant goes as its display text.
        other => D::Text(other.to_string()),
    }
}

/// Converts a batch to rows.
pub(crate) fn rows(batch: &RecordBatch) -> Result<Vec<Row>, DriverError> {
    let mut cols = Vec::with_capacity(batch.num_columns());
    for array in batch.columns() {
        cols.push(column_values(array)?);
    }
    let mut out = Vec::with_capacity(batch.num_rows());
    let mut iters: Vec<_> = cols.into_iter().map(Vec::into_iter).collect();
    for _ in 0..batch.num_rows() {
        let cells = iters.iter_mut().map(|it| it.next().unwrap_or(Value::Null));
        out.push(Row(cells.collect()));
    }
    Ok(out)
}

fn display_error(err: impl std::fmt::Display) -> DriverError {
    DriverError::Query(format!("cannot render a DuckDB value: {err}"))
}

/// Renders a cell through Arrow's display formatter.
fn text_column(array: &ArrayRef) -> Result<Vec<Value>, DriverError> {
    let formatter = ArrayFormatter::try_new(array.as_ref(), &FormatOptions::default())
        .map_err(display_error)?;
    let mut out = Vec::with_capacity(array.len());
    for i in 0..array.len() {
        out.push(if array.is_null(i) {
            Value::Null
        } else {
            Value::Text(formatter.value(i).to_string())
        });
    }
    Ok(out)
}

fn float(x: f64) -> Value {
    if x.is_finite() {
        Value::Float(x)
    } else {
        Value::Text(x.to_string())
    }
}

/// An integer and a scale as exact decimal text.
fn decimal_text(unscaled: &str, scale: i8) -> String {
    let (neg, digits) = match unscaled.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, unscaled),
    };
    let mut body = digits.to_owned();
    if scale > 0 {
        let scale = usize::from(scale.unsigned_abs());
        if body.len() <= scale {
            body = format!("{}{body}", "0".repeat(scale + 1 - body.len()));
        }
        body.insert(body.len() - scale, '.');
    } else if scale < 0 {
        body.push_str(&"0".repeat(usize::from(scale.unsigned_abs())));
    }
    if neg { format!("-{body}") } else { body }
}

fn decimal_value(unscaled: &str, scale: i8) -> Option<Value> {
    decimal_text(unscaled, scale)
        .parse()
        .ok()
        .map(Value::Decimal)
}

/// Days since 1970-01-01 as a calendar date (proleptic Gregorian), years 1..=9999 only.
fn civil(days: i64) -> Option<Date> {
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    if !(1..=9999).contains(&year) {
        return None;
    }
    Date::new(
        i32::try_from(year).ok()?,
        u8::try_from(month).ok()?,
        u8::try_from(day).ok()?,
    )
    .ok()
}

fn time_of_day(seconds: i64, nanos: u32) -> Option<Time> {
    if !(0..86_400).contains(&seconds) {
        return None;
    }
    Time::new(
        u8::try_from(seconds / 3600).ok()?,
        u8::try_from(seconds % 3600 / 60).ok()?,
        u8::try_from(seconds % 60).ok()?,
        nanos,
    )
    .ok()
}

/// Splits `value` units since the epoch into whole seconds and nanoseconds.
fn split(value: i64, unit: TimeUnit) -> (i64, u32) {
    let per_sec: i64 = match unit {
        TimeUnit::Second => 1,
        TimeUnit::Millisecond => 1_000,
        TimeUnit::Microsecond => 1_000_000,
        TimeUnit::Nanosecond => 1_000_000_000,
    };
    let secs = value.div_euclid(per_sec);
    let rest = value.rem_euclid(per_sec);
    let nanos = rest * (1_000_000_000 / per_sec);
    (secs, u32::try_from(nanos).unwrap_or(0))
}

fn timestamp(value: i64, unit: TimeUnit) -> Option<Timestamp> {
    let (secs, nanos) = split(value, unit);
    let date = civil(secs.div_euclid(86_400))?;
    let time = time_of_day(secs.rem_euclid(86_400), nanos)?;
    Some(Timestamp::new(date, time))
}

/// Converts one column to values. A cell without a typed form becomes text.
fn column_values(array: &ArrayRef) -> Result<Vec<Value>, DriverError> {
    let mut out = typed_column(array);
    let fallback = if out.iter().any(Option::is_none) {
        Some(text_column(array)?)
    } else {
        None
    };
    Ok(out
        .drain(..)
        .enumerate()
        .map(|(i, v)| {
            v.or_else(|| fallback.as_ref().and_then(|f| f.get(i).cloned()))
                .unwrap_or(Value::Null)
        })
        .collect())
}

/// `Some(value)` per cell, or `None` where the text fallback applies.
#[allow(clippy::too_many_lines, reason = "one flat arm per Arrow type")]
fn typed_column(array: &ArrayRef) -> Vec<Option<Value>> {
    let n = array.len();
    let all_text = || vec![None; n];
    let cells = |f: &dyn Fn(usize) -> Option<Value>| -> Vec<Option<Value>> {
        (0..n)
            .map(|i| {
                if array.is_null(i) {
                    Some(Value::Null)
                } else {
                    f(i)
                }
            })
            .collect()
    };
    match array.data_type() {
        ArrowType::Null => vec![Some(Value::Null); n],
        ArrowType::Boolean => {
            let a = array.as_boolean();
            cells(&|i| Some(Value::Bool(a.value(i))))
        }
        ArrowType::Int8 => {
            let a = array.as_primitive::<Int8Type>();
            cells(&|i| Some(Value::Int(i64::from(a.value(i)))))
        }
        ArrowType::Int16 => {
            let a = array.as_primitive::<Int16Type>();
            cells(&|i| Some(Value::Int(i64::from(a.value(i)))))
        }
        ArrowType::Int32 => {
            let a = array.as_primitive::<Int32Type>();
            cells(&|i| Some(Value::Int(i64::from(a.value(i)))))
        }
        ArrowType::Int64 => {
            let a = array.as_primitive::<Int64Type>();
            cells(&|i| Some(Value::Int(a.value(i))))
        }
        ArrowType::UInt8 => {
            let a = array.as_primitive::<UInt8Type>();
            cells(&|i| Some(Value::Int(i64::from(a.value(i)))))
        }
        ArrowType::UInt16 => {
            let a = array.as_primitive::<UInt16Type>();
            cells(&|i| Some(Value::Int(i64::from(a.value(i)))))
        }
        ArrowType::UInt32 => {
            let a = array.as_primitive::<UInt32Type>();
            cells(&|i| Some(Value::Int(i64::from(a.value(i)))))
        }
        ArrowType::UInt64 => {
            let a = array.as_primitive::<UInt64Type>();
            cells(&|i| {
                let v = a.value(i);
                Some(match i64::try_from(v) {
                    Ok(v) => Value::Int(v),
                    Err(_) => return decimal_value(&v.to_string(), 0),
                })
            })
        }
        ArrowType::Float16 => {
            let a = array.as_primitive::<Float16Type>();
            cells(&|i| Some(float(a.value(i).to_f64())))
        }
        ArrowType::Float32 => {
            let a = array.as_primitive::<Float32Type>();
            cells(&|i| Some(float(f64::from(a.value(i)))))
        }
        ArrowType::Float64 => {
            let a = array.as_primitive::<Float64Type>();
            cells(&|i| Some(float(a.value(i))))
        }
        ArrowType::Decimal128(_, scale) => {
            let a = array.as_primitive::<Decimal128Type>();
            cells(&|i| decimal_value(&a.value(i).to_string(), *scale))
        }
        ArrowType::Decimal256(_, scale) => {
            let a = array.as_primitive::<Decimal256Type>();
            cells(&|i| decimal_value(&a.value(i).to_string(), *scale))
        }
        ArrowType::Utf8 => {
            let a = array.as_string::<i32>();
            cells(&|i| Some(Value::Text(a.value(i).to_owned())))
        }
        ArrowType::LargeUtf8 => {
            let a = array.as_string::<i64>();
            cells(&|i| Some(Value::Text(a.value(i).to_owned())))
        }
        ArrowType::Utf8View => {
            let a = array.as_string_view();
            cells(&|i| Some(Value::Text(a.value(i).to_owned())))
        }
        ArrowType::Binary => {
            let a = array.as_binary::<i32>();
            cells(&|i| Some(Value::Bytes(a.value(i).to_vec())))
        }
        ArrowType::LargeBinary => {
            let a = array.as_binary::<i64>();
            cells(&|i| Some(Value::Bytes(a.value(i).to_vec())))
        }
        ArrowType::BinaryView => {
            let a = array.as_binary_view();
            cells(&|i| Some(Value::Bytes(a.value(i).to_vec())))
        }
        ArrowType::FixedSizeBinary(_) => {
            let a = array.as_fixed_size_binary();
            cells(&|i| Some(Value::Bytes(a.value(i).to_vec())))
        }
        ArrowType::Date32 => {
            let a = array.as_primitive::<Date32Type>();
            cells(&|i| civil(i64::from(a.value(i))).map(Value::Date))
        }
        ArrowType::Date64 => {
            let a = array.as_primitive::<Date64Type>();
            cells(&|i| civil(a.value(i).div_euclid(86_400_000)).map(Value::Date))
        }
        ArrowType::Time64(unit) => match unit {
            TimeUnit::Microsecond => {
                let a = array.as_primitive::<duckdb::arrow::datatypes::Time64MicrosecondType>();
                cells(&|i| {
                    let (s, n) = split(a.value(i), TimeUnit::Microsecond);
                    time_of_day(s, n).map(Value::Time)
                })
            }
            TimeUnit::Nanosecond => {
                let a = array.as_primitive::<duckdb::arrow::datatypes::Time64NanosecondType>();
                cells(&|i| {
                    let (s, n) = split(a.value(i), TimeUnit::Nanosecond);
                    time_of_day(s, n).map(Value::Time)
                })
            }
            _ => all_text(),
        },
        ArrowType::Timestamp(unit, tz) => {
            let values: Vec<i64> = match unit {
                TimeUnit::Second => {
                    ts_values::<duckdb::arrow::datatypes::TimestampSecondType>(array)
                }
                TimeUnit::Millisecond => {
                    ts_values::<duckdb::arrow::datatypes::TimestampMillisecondType>(array)
                }
                TimeUnit::Microsecond => {
                    ts_values::<duckdb::arrow::datatypes::TimestampMicrosecondType>(array)
                }
                TimeUnit::Nanosecond => {
                    ts_values::<duckdb::arrow::datatypes::TimestampNanosecondType>(array)
                }
            };
            let zoned = tz.is_some();
            cells(&|i| {
                let local = timestamp(values[i], *unit)?;
                if zoned {
                    TimestampTz::new(local, 0).ok().map(Value::TimestampTz)
                } else {
                    Some(Value::Timestamp(local))
                }
            })
        }
        _ => all_text(),
    }
}

fn ts_values<T: duckdb::arrow::datatypes::ArrowPrimitiveType<Native = i64>>(
    array: &ArrayRef,
) -> Vec<i64> {
    array.as_primitive::<T>().values().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_rendering() {
        assert_eq!(decimal_text("12345", 4), "1.2345");
        assert_eq!(decimal_text("-5", 3), "-0.005");
        assert_eq!(decimal_text("0", 2), "0.00");
        assert_eq!(decimal_text("7", 0), "7");
        assert_eq!(decimal_text("7", -2), "700");
    }

    #[test]
    fn civil_dates() {
        assert_eq!(
            civil(0).map(|d| d.to_string()).as_deref(),
            Some("1970-01-01")
        );
        assert_eq!(
            civil(-1).map(|d| d.to_string()).as_deref(),
            Some("1969-12-31")
        );
        assert_eq!(
            civil(19_782).map(|d| d.to_string()).as_deref(),
            Some("2024-02-29")
        );
        assert!(civil(i64::from(i32::MAX)).is_none());
    }

    #[test]
    fn split_negative_micros() {
        assert_eq!(split(-1, TimeUnit::Microsecond), (-1, 999_999_000));
    }
}
