// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Core values, column types and environments to and from the WIT types.

use sqler_driver_api::sqler_core::{
    DataType, Date, Decimal, Environment, ObjectKind, ObjectPath, Time, Timestamp, TimestampTz,
    Value,
};

use crate::runtime::wit::types as wit;

/// Extra bytes charged per cell on top of its payload, for the list and variant overhead.
pub(crate) const CELL_OVERHEAD: usize = 16;

/// A WIT parameter as a core value. `Err` carries what did not parse: temporal and decimal values arrive as text.
pub(crate) fn value_from_wit(v: wit::Value) -> Result<Value, String> {
    fn parse<T: std::str::FromStr>(
        kind: &str,
        text: &str,
        wrap: impl FnOnce(T) -> Value,
    ) -> Result<Value, String> {
        text.parse::<T>()
            .map(wrap)
            .map_err(|_| format!("`{}` is not a valid {kind}", clip(text)))
    }
    Ok(match v {
        wit::Value::Null => Value::Null,
        wit::Value::Boolean(b) => Value::Bool(b),
        wit::Value::Integer(i) => Value::Int(i),
        wit::Value::Float(f) => Value::Float(f),
        wit::Value::Text(t) => Value::Text(t),
        wit::Value::Bytes(b) => Value::Bytes(b),
        wit::Value::Json(j) => Value::Json(j),
        wit::Value::Decimal(t) => parse::<Decimal>("decimal", &t, Value::Decimal)?,
        wit::Value::Date(t) => parse::<Date>("date", &t, Value::Date)?,
        wit::Value::Time(t) => parse::<Time>("time", &t, Value::Time)?,
        wit::Value::Timestamp(t) => parse::<Timestamp>("timestamp", &t, Value::Timestamp)?,
        wit::Value::TimestampTz(t) => {
            parse::<TimestampTz>("timestamp with time zone", &t, Value::TimestampTz)?
        }
    })
}

/// A core value as WIT. Temporal and decimal values become their `Display` text; a variant this host does not know
/// becomes `text`.
pub(crate) fn value_to_wit(v: &Value) -> wit::Value {
    match v {
        Value::Null => wit::Value::Null,
        Value::Bool(b) => wit::Value::Boolean(*b),
        Value::Int(i) => wit::Value::Integer(*i),
        Value::Float(f) => wit::Value::Float(*f),
        Value::Text(t) => wit::Value::Text(t.clone()),
        Value::Bytes(b) => wit::Value::Bytes(b.clone()),
        Value::Json(j) => wit::Value::Json(j.clone()),
        Value::Decimal(d) => wit::Value::Decimal(d.to_string()),
        Value::Date(d) => wit::Value::Date(d.to_string()),
        Value::Time(t) => wit::Value::Time(t.to_string()),
        Value::Timestamp(t) => wit::Value::Timestamp(t.to_string()),
        Value::TimestampTz(t) => wit::Value::TimestampTz(t.to_string()),
        other => wit::Value::Text(other.to_string()),
    }
}

/// What a cell costs against [`MAX_RESULT_BYTES`](super::MAX_RESULT_BYTES): its payload plus [`CELL_OVERHEAD`].
pub(crate) fn cell_cost(v: &wit::Value) -> usize {
    let payload = match v {
        wit::Value::Null => 0,
        wit::Value::Boolean(_) => 1,
        wit::Value::Integer(_) | wit::Value::Float(_) => 8,
        wit::Value::Text(s)
        | wit::Value::Decimal(s)
        | wit::Value::Date(s)
        | wit::Value::Time(s)
        | wit::Value::Timestamp(s)
        | wit::Value::TimestampTz(s)
        | wit::Value::Json(s) => s.len(),
        wit::Value::Bytes(b) => b.len(),
    };
    payload + CELL_OVERHEAD
}

/// The WIT column type. A type this host does not know is `other`.
pub(crate) fn data_type_to_wit(t: DataType) -> wit::DataType {
    match t {
        DataType::Boolean => wit::DataType::Boolean,
        DataType::Integer => wit::DataType::Integer,
        DataType::Float => wit::DataType::Float,
        DataType::Decimal => wit::DataType::Decimal,
        DataType::Text => wit::DataType::Text,
        DataType::Binary => wit::DataType::Binary,
        DataType::Date => wit::DataType::Date,
        DataType::Time => wit::DataType::Time,
        DataType::Timestamp => wit::DataType::Timestamp,
        DataType::TimestampTz => wit::DataType::TimestampTz,
        DataType::Json => wit::DataType::Json,
        _ => wit::DataType::Other,
    }
}

/// The lowercase name of a column type, as shown when the driver has no native type name.
pub(crate) fn data_type_name(t: wit::DataType) -> &'static str {
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
}

/// The type name of a column: the driver's own, else the lowercase type name.
pub(crate) fn type_name(native: Option<&str>, t: DataType) -> String {
    native.map_or_else(
        || data_type_name(data_type_to_wit(t)).to_owned(),
        str::to_owned,
    )
}

/// The WIT environment; an untagged connection is `dev`.
pub(crate) fn environment_to_wit(e: Option<Environment>) -> wit::Environment {
    match e {
        Some(Environment::Staging) => wit::Environment::Staging,
        Some(Environment::Prod) => wit::Environment::Prod,
        _ => wit::Environment::Dev,
    }
}

/// The WIT object kind, or `None` for a kind this host does not know.
pub(crate) fn kind_to_wit(k: ObjectKind) -> Option<wit::ObjectKind> {
    Some(match k {
        ObjectKind::Database => wit::ObjectKind::Database,
        ObjectKind::Schema => wit::ObjectKind::Schema,
        ObjectKind::Table => wit::ObjectKind::Table,
        ObjectKind::View => wit::ObjectKind::View,
        ObjectKind::MaterializedView => wit::ObjectKind::MaterializedView,
        ObjectKind::Function => wit::ObjectKind::Function,
        ObjectKind::Procedure => wit::ObjectKind::Procedure,
        ObjectKind::Trigger => wit::ObjectKind::Trigger,
        ObjectKind::Sequence => wit::ObjectKind::Sequence,
        ObjectKind::Type => wit::ObjectKind::Type,
        ObjectKind::Index => wit::ObjectKind::Index,
        ObjectKind::Extension => wit::ObjectKind::Extension,
        ObjectKind::Role => wit::ObjectKind::Role,
        _ => return None,
    })
}

/// The core path of a WIT path: an empty `database` or `schema` string means `None`.
pub(crate) fn path_from_wit(p: wit::ObjectPath) -> ObjectPath {
    let mut path = ObjectPath::root();
    if !p.database.is_empty() {
        path = path.in_database(p.database);
    }
    if !p.schema.is_empty() {
        path = path.in_schema(p.schema);
    }
    if let Some(table) = p.table {
        path = path.in_table(table);
    }
    path
}

/// The WIT path of a core path: a missing `database` or `schema` is the empty string.
pub(crate) fn path_to_wit(p: ObjectPath) -> wit::ObjectPath {
    wit::ObjectPath {
        database: p.database.unwrap_or_default(),
        schema: p.schema.unwrap_or_default(),
        table: p.table,
    }
}

/// At most 512 characters of `text`, for error messages that carry driver or user text.
pub(crate) fn clip(text: &str) -> String {
    text.chars().take(512).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_through_wit_text() {
        for v in [
            wit::Value::Date("2024-02-29".into()),
            wit::Value::Time("10:20:30".into()),
            wit::Value::Timestamp("2024-02-29T10:20:30".into()),
            wit::Value::TimestampTz("2024-02-29T10:20:30+02:00".into()),
            wit::Value::Decimal("12.50".into()),
            wit::Value::Integer(-3),
            wit::Value::Json("{}".into()),
            wit::Value::Null,
        ] {
            let core = value_from_wit(v).unwrap();
            let back = value_to_wit(&core);
            assert_eq!(value_from_wit(back).unwrap(), core);
        }
    }

    #[test]
    fn unparsable_text_is_an_error() {
        for v in [
            wit::Value::Date("2024-13-40".into()),
            wit::Value::Time("25:00".into()),
            wit::Value::Decimal("abc".into()),
            wit::Value::Timestamp("yesterday".into()),
            wit::Value::TimestampTz("2024-01-01".into()),
        ] {
            assert!(value_from_wit(v).is_err());
        }
    }

    #[test]
    fn cells_cost_payload_plus_overhead() {
        assert_eq!(cell_cost(&wit::Value::Text("abcd".into())), 4 + 16);
        assert_eq!(cell_cost(&wit::Value::Null), 16);
        assert_eq!(cell_cost(&wit::Value::Bytes(vec![0; 10])), 26);
    }

    #[test]
    fn paths_map_empty_to_none() {
        let p = path_from_wit(wit::ObjectPath {
            database: String::new(),
            schema: "main".into(),
            table: None,
        });
        assert_eq!(p.database, None);
        assert_eq!(p.schema.as_deref(), Some("main"));
        let back = path_to_wit(p);
        assert_eq!(back.database, "");
    }

    #[test]
    fn type_names_prefer_the_native_one() {
        assert_eq!(type_name(Some("VARCHAR(5)"), DataType::Text), "VARCHAR(5)");
        assert_eq!(type_name(None, DataType::TimestampTz), "timestamp-tz");
    }

    #[test]
    fn clip_counts_characters() {
        assert_eq!(clip(&"é".repeat(600)).chars().count(), 512);
    }
}
