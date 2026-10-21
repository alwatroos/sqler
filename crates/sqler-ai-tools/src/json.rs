// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Domain values as JSON.

use serde_json::{Value as Json, json};
use sqler_driver_api::sqler_core::{Column, DataType, Value};

/// The lowercase name of a data type (`integer`, `text`, `timestamptz`, ...).
pub(crate) fn type_name(data_type: DataType) -> String {
    format!("{data_type:?}").to_lowercase()
}

/// A cell as JSON: null, bool, number for ints and finite floats, `"NaN"`/`"Infinity"`/
/// `"-Infinity"` for the other floats, `"0x..."` for bytes, the `Display` text for the rest.
pub(crate) fn cell(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(b) => json!(b),
        Value::Int(i) => json!(i),
        Value::Float(f) => match serde_json::Number::from_f64(*f) {
            Some(n) => Json::Number(n),
            None if f.is_nan() => json!("NaN"),
            None if f.is_sign_negative() => json!("-Infinity"),
            None => json!("Infinity"),
        },
        other => Json::String(other.to_string()),
    }
}

/// A result column as `{name, type}`.
pub(crate) fn column(col: &Column) -> Json {
    json!({"name": col.name, "type": type_name(col.data_type)})
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::{Date, Decimal};

    #[test]
    fn cells() {
        assert_eq!(cell(&Value::Null), Json::Null);
        assert_eq!(cell(&Value::Bool(true)), json!(true));
        assert_eq!(cell(&Value::Int(-3)), json!(-3));
        assert_eq!(cell(&Value::Float(1.5)), json!(1.5));
        assert_eq!(cell(&Value::Float(f64::NAN)), json!("NaN"));
        assert_eq!(cell(&Value::Float(f64::INFINITY)), json!("Infinity"));
        assert_eq!(cell(&Value::Float(f64::NEG_INFINITY)), json!("-Infinity"));
        assert_eq!(cell(&Value::Bytes(vec![0, 255, 16])), json!("0x00ff10"));
        assert_eq!(cell(&Value::Text("hi".into())), json!("hi"));
        assert_eq!(cell(&Value::Json("{\"a\":1}".into())), json!("{\"a\":1}"));
        let date = Date::new(2024, 2, 29).unwrap();
        assert_eq!(cell(&Value::Date(date)), json!("2024-02-29"));
        let dec: Decimal = "12.50".parse().unwrap();
        assert_eq!(cell(&Value::Decimal(dec)), json!("12.50"));
    }

    #[test]
    fn column_types() {
        let c = Column::new("a", DataType::TimestampTz);
        assert_eq!(column(&c), json!({"name": "a", "type": "timestamptz"}));
        assert_eq!(type_name(DataType::Integer), "integer");
    }
}
