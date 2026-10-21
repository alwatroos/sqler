// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `DataType`-to-schema mapping and the conversion of a [`Value`] into a column buffer.

use std::sync::Arc;

use ::parquet::basic::{LogicalType, Repetition, TimeUnit, Type as PhysicalType};
use ::parquet::data_type::ByteArray;
use ::parquet::errors::ParquetError;
use ::parquet::schema::types::{Type, TypePtr};
use sqler_core::{DataType, Value};

use crate::render;
use crate::temporal;

/// How a column is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Boolean,
    Integer,
    Float,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Binary,
    /// Decimal, text, JSON and every other type: the rendered text.
    Text,
}

impl Kind {
    pub(super) fn of(data_type: DataType) -> Self {
        match data_type {
            DataType::Boolean => Self::Boolean,
            DataType::Integer => Self::Integer,
            DataType::Float => Self::Float,
            DataType::Date => Self::Date,
            DataType::Time => Self::Time,
            DataType::Timestamp => Self::Timestamp,
            DataType::TimestampTz => Self::TimestampTz,
            DataType::Binary => Self::Binary,
            _ => Self::Text,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Date => "date",
            Self::Time => "time",
            Self::Timestamp => "timestamp",
            Self::TimestampTz => "timestamptz",
            Self::Binary => "binary",
            Self::Text => "text",
        }
    }

    fn field(self, name: &str) -> Result<TypePtr, ParquetError> {
        let (physical, logical) = match self {
            Self::Boolean => (PhysicalType::BOOLEAN, None),
            Self::Integer => (PhysicalType::INT64, None),
            Self::Float => (PhysicalType::DOUBLE, None),
            Self::Date => (PhysicalType::INT32, Some(LogicalType::Date)),
            Self::Time => (
                PhysicalType::INT64,
                Some(LogicalType::time(false, TimeUnit::NANOS)),
            ),
            Self::Timestamp => (
                PhysicalType::INT64,
                Some(LogicalType::timestamp(false, TimeUnit::MICROS)),
            ),
            Self::TimestampTz => (
                PhysicalType::INT64,
                Some(LogicalType::timestamp(true, TimeUnit::MICROS)),
            ),
            Self::Binary => (PhysicalType::BYTE_ARRAY, None),
            Self::Text => (PhysicalType::BYTE_ARRAY, Some(LogicalType::String)),
        };
        let field = Type::primitive_type_builder(name, physical)
            .with_repetition(Repetition::OPTIONAL)
            .with_logical_type(logical)
            .build()?;
        Ok(Arc::new(field))
    }
}

/// The message type of the file: one optional field per column.
pub(super) fn message_type(names: &[String], kinds: &[Kind]) -> Result<TypePtr, ParquetError> {
    let fields = names
        .iter()
        .zip(kinds)
        .map(|(name, kind)| kind.field(name))
        .collect::<Result<Vec<_>, _>>()?;
    let message = Type::group_type_builder("schema")
        .with_fields(fields)
        .build()?;
    Ok(Arc::new(message))
}

/// The values of one column group, typed as the physical type.
#[derive(Debug)]
pub(super) enum Values {
    Bool(Vec<bool>),
    Int32(Vec<i32>),
    Int64(Vec<i64>),
    Double(Vec<f64>),
    Bytes(Vec<ByteArray>),
}

/// The buffer of one column for the current row group: the non-null values and one definition
/// level per row.
#[derive(Debug)]
pub(super) struct ColumnBuf {
    pub(super) kind: Kind,
    pub(super) values: Values,
    pub(super) defs: Vec<i16>,
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Int(_) => "integer",
        Value::Float(_) => "float",
        Value::Text(_) => "text",
        Value::Bytes(_) => "binary",
        Value::Decimal(_) => "decimal",
        Value::Date(_) => "date",
        Value::Time(_) => "time",
        Value::Timestamp(_) => "timestamp",
        Value::TimestampTz(_) => "timestamptz",
        Value::Json(_) => "json",
        _ => "other",
    }
}

impl ColumnBuf {
    pub(super) fn new(kind: Kind) -> Self {
        let values = match kind {
            Kind::Boolean => Values::Bool(Vec::new()),
            Kind::Date => Values::Int32(Vec::new()),
            Kind::Integer | Kind::Time | Kind::Timestamp | Kind::TimestampTz => {
                Values::Int64(Vec::new())
            }
            Kind::Float => Values::Double(Vec::new()),
            Kind::Binary | Kind::Text => Values::Bytes(Vec::new()),
        };
        Self {
            kind,
            values,
            defs: Vec::new(),
        }
    }

    pub(super) fn clear(&mut self) {
        match &mut self.values {
            Values::Bool(v) => v.clear(),
            Values::Int32(v) => v.clear(),
            Values::Int64(v) => v.clear(),
            Values::Double(v) => v.clear(),
            Values::Bytes(v) => v.clear(),
        }
        self.defs.clear();
    }

    /// Adds one value and returns the bytes it adds to the buffer. `scratch` is reused text.
    ///
    /// # Errors
    ///
    /// The reason when the value does not fit the column; the buffer is unchanged then.
    pub(super) fn push(&mut self, value: &Value, scratch: &mut String) -> Result<usize, String> {
        if matches!(value, Value::Null) {
            self.defs.push(0);
            return Ok(2);
        }
        let mismatch = || {
            format!(
                "a {} value cannot be stored in a {} column",
                kind_of(value),
                self.kind.name()
            )
        };
        let added = match (&mut self.values, value) {
            (Values::Bool(v), Value::Bool(b)) => {
                v.push(*b);
                1
            }
            (Values::Int64(v), Value::Int(i)) if self.kind == Kind::Integer => {
                v.push(*i);
                8
            }
            (Values::Double(v), Value::Float(x)) => {
                v.push(*x);
                8
            }
            (Values::Double(v), Value::Int(i)) => {
                #[allow(clippy::cast_precision_loss, reason = "an integer in a float column")]
                v.push(*i as f64);
                8
            }
            (Values::Int32(v), Value::Date(d)) => {
                let days = i32::try_from(temporal::days_from_civil(*d))
                    .map_err(|_| format!("date {d} is out of the range of a Parquet date"))?;
                v.push(days);
                4
            }
            (Values::Int64(v), Value::Time(t)) if self.kind == Kind::Time => {
                v.push(temporal::nanos_of_day(*t));
                8
            }
            (Values::Int64(v), Value::Timestamp(ts)) if self.kind == Kind::Timestamp => {
                let micros = temporal::local_micros(*ts)
                    .ok_or_else(|| format!("timestamp {ts} is out of range"))?;
                v.push(micros);
                8
            }
            (Values::Int64(v), Value::TimestampTz(ts)) if self.kind == Kind::TimestampTz => {
                let micros = temporal::utc_micros(*ts)
                    .ok_or_else(|| format!("timestamp {ts} is out of range"))?;
                v.push(micros);
                8
            }
            (Values::Bytes(v), Value::Bytes(b)) if self.kind == Kind::Binary => {
                v.push(ByteArray::from(b.as_slice()));
                b.len() + 24
            }
            (Values::Bytes(v), Value::Text(t)) if self.kind == Kind::Binary => {
                v.push(ByteArray::from(t.as_bytes()));
                t.len() + 24
            }
            (Values::Bytes(v), other) if self.kind == Kind::Text => {
                scratch.clear();
                render::plain(scratch, other, "");
                v.push(ByteArray::from(scratch.as_bytes()));
                scratch.len() + 24
            }
            _ => return Err(mismatch()),
        };
        self.defs.push(1);
        Ok(added + 2)
    }
}

#[cfg(test)]
mod tests {
    use sqler_core::{Date, Decimal, Time, Timestamp, TimestampTz};

    use super::*;

    fn buf(data_type: DataType) -> ColumnBuf {
        ColumnBuf::new(Kind::of(data_type))
    }

    fn ts(y: i32) -> Timestamp {
        Timestamp::new(Date::new(y, 1, 1).unwrap(), Time::new(0, 0, 0, 0).unwrap())
    }

    #[test]
    fn schema_has_optional_fields_and_logical_types() {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"].map(String::from);
        let types = [
            DataType::Boolean,
            DataType::Integer,
            DataType::Float,
            DataType::Date,
            DataType::Time,
            DataType::Timestamp,
            DataType::TimestampTz,
            DataType::Binary,
            DataType::Decimal,
        ];
        let kinds: Vec<_> = types.into_iter().map(Kind::of).collect();
        let schema = message_type(&names, &kinds).unwrap();
        let fields = schema.get_fields();
        assert_eq!(fields.len(), 9);
        assert!(fields.iter().all(|f| f.get_basic_info().has_repetition()));
        let phys: Vec<_> = fields.iter().map(|f| f.get_physical_type()).collect();
        assert_eq!(
            phys,
            [
                PhysicalType::BOOLEAN,
                PhysicalType::INT64,
                PhysicalType::DOUBLE,
                PhysicalType::INT32,
                PhysicalType::INT64,
                PhysicalType::INT64,
                PhysicalType::INT64,
                PhysicalType::BYTE_ARRAY,
                PhysicalType::BYTE_ARRAY,
            ]
        );
        let logical: Vec<_> = fields
            .iter()
            .map(|f| f.get_basic_info().logical_type_ref().cloned())
            .collect();
        assert_eq!(logical[3], Some(LogicalType::Date));
        assert_eq!(logical[4], Some(LogicalType::time(false, TimeUnit::NANOS)));
        assert_eq!(
            logical[5],
            Some(LogicalType::timestamp(false, TimeUnit::MICROS))
        );
        assert_eq!(
            logical[6],
            Some(LogicalType::timestamp(true, TimeUnit::MICROS))
        );
        assert_eq!(logical[7], None);
        assert_eq!(logical[8], Some(LogicalType::String));
        assert!(
            fields
                .iter()
                .all(|f| f.get_basic_info().repetition() == Repetition::OPTIONAL)
        );
    }

    #[test]
    fn nulls_are_definition_levels_only() {
        let mut b = buf(DataType::Integer);
        let mut s = String::new();
        b.push(&Value::Int(7), &mut s).unwrap();
        b.push(&Value::Null, &mut s).unwrap();
        assert_eq!(b.defs, [1, 0]);
        assert!(matches!(&b.values, Values::Int64(v) if v == &[7]));
    }

    #[test]
    fn timestamps_are_micros_and_tz_is_utc() {
        let mut s = String::new();
        let mut b = buf(DataType::Timestamp);
        let t = Timestamp::new(
            Date::new(1970, 1, 1).unwrap(),
            Time::new(0, 0, 1, 1_999).unwrap(),
        );
        b.push(&Value::Timestamp(t), &mut s).unwrap();
        assert!(matches!(&b.values, Values::Int64(v) if v == &[1_000_001]));

        let mut b = buf(DataType::TimestampTz);
        let tz = TimestampTz::new(ts(1970), 5 * 3600 + 1800).unwrap();
        b.push(&Value::TimestampTz(tz), &mut s).unwrap();
        assert!(matches!(&b.values, Values::Int64(v) if v == &[-(5 * 3600 + 1800) * 1_000_000]));
    }

    #[test]
    fn decimal_is_exact_text() {
        let mut s = String::new();
        let mut b = buf(DataType::Decimal);
        let d = "123.4500".parse::<Decimal>().unwrap();
        b.push(&Value::Decimal(d), &mut s).unwrap();
        assert!(matches!(&b.values, Values::Bytes(v) if v[0].data() == b"123.4500"));
    }

    #[test]
    fn mismatches_fail_instead_of_becoming_null() {
        let mut s = String::new();
        let mut b = buf(DataType::Timestamp);
        let err = b.push(&Value::Text("infinity".into()), &mut s).unwrap_err();
        assert!(err.contains("text") && err.contains("timestamp"), "{err}");
        assert_eq!(b.defs.len(), 0);
        assert!(
            buf(DataType::Integer)
                .push(&Value::Float(1.0), &mut s)
                .is_err()
        );
        assert!(buf(DataType::Boolean).push(&Value::Int(1), &mut s).is_err());
        assert!(buf(DataType::Date).push(&Value::Int(1), &mut s).is_err());
        // A float column takes integers; a binary column takes text.
        assert!(buf(DataType::Float).push(&Value::Int(1), &mut s).is_ok());
        assert!(
            buf(DataType::Binary)
                .push(&Value::Text("x".into()), &mut s)
                .is_ok()
        );
    }

    #[test]
    fn out_of_range_values_are_errors() {
        let mut s = String::new();
        let far = Value::Date(Date::new(i32::MAX, 1, 1).unwrap());
        assert!(buf(DataType::Date).push(&far, &mut s).is_err());
        let far_ts = Value::Timestamp(ts(i32::MAX));
        assert!(buf(DataType::Timestamp).push(&far_ts, &mut s).is_err());
    }
}
