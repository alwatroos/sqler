// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Type-aware cell input: which editor a column gets, the text it starts with, and how the typed
//! text becomes a [`Value`].
//!
//! Parsing is strict and local: numbers by `i64` and finite `f64`, exact decimals, dates and
//! times by the `FromStr` of the core types. JSON is not checked on the client (the server
//! decides). The error texts never repeat the input.

use std::str::FromStr;

use sqler_driver_api::sqler_core::{DataType, Date, Decimal, Time, Timestamp, TimestampTz, Value};

/// Which editor a column type gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditorKind {
    /// Free text (also the editor of types without a closer match).
    Text,
    /// A whole number.
    Integer,
    /// A finite floating point number.
    Float,
    /// An exact decimal number.
    Decimal,
    /// True or false.
    Boolean,
    /// `YYYY-MM-DD`.
    Date,
    /// `HH:MM:SS[.fraction]`.
    Time,
    /// Date and time without a zone.
    Timestamp,
    /// Date and time with a UTC offset.
    TimestampTz,
    /// A JSON document, as text.
    Json,
    /// Binary data; shown, never edited (hex editing is deferred).
    ReadOnlyBinary,
}

impl EditorKind {
    /// A short hint for the user about the accepted format, if the kind has one.
    #[must_use]
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::Integer => Some("whole number"),
            Self::Float => Some("number"),
            Self::Decimal => Some("exact number"),
            Self::Date => Some("YYYY-MM-DD"),
            Self::Time => Some("HH:MM:SS"),
            Self::Timestamp => Some("YYYY-MM-DD HH:MM:SS"),
            Self::TimestampTz => Some("YYYY-MM-DD HH:MM:SS+00:00"),
            Self::Json => Some("JSON"),
            Self::Text | Self::Boolean | Self::ReadOnlyBinary => None,
        }
    }
}

/// The editor a column of type `t` gets.
#[must_use]
pub fn editor_kind(t: DataType) -> EditorKind {
    match t {
        DataType::Boolean => EditorKind::Boolean,
        DataType::Integer => EditorKind::Integer,
        DataType::Float => EditorKind::Float,
        DataType::Decimal => EditorKind::Decimal,
        DataType::Binary => EditorKind::ReadOnlyBinary,
        DataType::Date => EditorKind::Date,
        DataType::Time => EditorKind::Time,
        DataType::Timestamp => EditorKind::Timestamp,
        DataType::TimestampTz => EditorKind::TimestampTz,
        DataType::Json => EditorKind::Json,
        _ => EditorKind::Text,
    }
}

/// The text an editor starts with for a cell holding `v`: the value as the server would print it,
/// unshortened (`NULL` starts empty; the editor offers "Set NULL" separately).
#[must_use]
pub fn edit_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

const EMPTY: &str = "Enter a value or use Set NULL.";

/// Turns typed text into a value of the column type `t`.
///
/// Empty input is the empty string for text and an error for every other type. Surrounding
/// whitespace is ignored for everything except text and JSON.
///
/// # Errors
///
/// A short sentence for the user; it never contains the input.
pub fn parse_cell(t: DataType, input: &str) -> Result<Value, String> {
    let kind = editor_kind(t);
    match kind {
        EditorKind::Text => return Ok(Value::Text(input.to_owned())),
        EditorKind::ReadOnlyBinary => return Err("Binary cells are read-only.".to_owned()),
        EditorKind::Json => {
            return if input.trim().is_empty() {
                Err(EMPTY.to_owned())
            } else {
                Ok(Value::Json(input.to_owned()))
            };
        }
        _ => {}
    }
    let s = input.trim();
    if s.is_empty() {
        return Err(EMPTY.to_owned());
    }
    match kind {
        EditorKind::Integer => s
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| "Not a whole number that fits 64 bits.".to_owned()),
        EditorKind::Float => match s.parse::<f64>() {
            Ok(x) if x.is_finite() => Ok(Value::Float(x)),
            _ => Err("Not a finite number.".to_owned()),
        },
        EditorKind::Decimal => Decimal::from_str(s)
            .map(Value::Decimal)
            .map_err(|_| "Not a valid decimal number.".to_owned()),
        EditorKind::Boolean => match s.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" | "y" => Ok(Value::Bool(true)),
            "false" | "f" | "0" | "no" | "n" => Ok(Value::Bool(false)),
            _ => Err("Not a boolean: use true or false.".to_owned()),
        },
        EditorKind::Date => Date::from_str(s)
            .map(Value::Date)
            .map_err(|_| "Not a valid date: use YYYY-MM-DD.".to_owned()),
        EditorKind::Time => Time::from_str(s)
            .map(Value::Time)
            .map_err(|_| "Not a valid time: use HH:MM:SS.".to_owned()),
        EditorKind::Timestamp => Timestamp::from_str(s)
            .map(Value::Timestamp)
            .map_err(|_| "Not a valid timestamp: use YYYY-MM-DD HH:MM:SS.".to_owned()),
        EditorKind::TimestampTz => TimestampTz::from_str(s)
            .map(Value::TimestampTz)
            .map_err(|_| {
                "Not a valid timestamp with offset: use YYYY-MM-DD HH:MM:SS+00:00.".to_owned()
            }),
        _ => Err("This type cannot be edited.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [DataType; 13] = [
        DataType::Boolean,
        DataType::Integer,
        DataType::Float,
        DataType::Decimal,
        DataType::Text,
        DataType::Binary,
        DataType::Date,
        DataType::Time,
        DataType::Timestamp,
        DataType::TimestampTz,
        DataType::Json,
        DataType::Other,
        DataType::Text,
    ];

    #[test]
    fn every_type_has_an_editor() {
        let kinds: Vec<_> = ALL.iter().map(|t| editor_kind(*t)).collect();
        assert_eq!(
            kinds,
            [
                EditorKind::Boolean,
                EditorKind::Integer,
                EditorKind::Float,
                EditorKind::Decimal,
                EditorKind::Text,
                EditorKind::ReadOnlyBinary,
                EditorKind::Date,
                EditorKind::Time,
                EditorKind::Timestamp,
                EditorKind::TimestampTz,
                EditorKind::Json,
                EditorKind::Text,
                EditorKind::Text,
            ]
        );
        assert_eq!(EditorKind::Date.hint(), Some("YYYY-MM-DD"));
        assert_eq!(EditorKind::Text.hint(), None);
    }

    #[test]
    fn text_and_other_keep_the_input_verbatim() {
        for t in [DataType::Text, DataType::Other] {
            assert_eq!(
                parse_cell(t, "  a b\n"),
                Ok(Value::Text("  a b\n".to_owned()))
            );
            assert_eq!(parse_cell(t, ""), Ok(Value::Text(String::new())));
        }
    }

    #[test]
    fn integers() {
        assert_eq!(parse_cell(DataType::Integer, " -42 "), Ok(Value::Int(-42)));
        assert_eq!(
            parse_cell(DataType::Integer, "9223372036854775807"),
            Ok(Value::Int(i64::MAX))
        );
        for bad in ["9223372036854775808", "1.5", "abc", "1e3", "+-1"] {
            assert!(parse_cell(DataType::Integer, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn floats_must_be_finite() {
        assert_eq!(parse_cell(DataType::Float, "1.5"), Ok(Value::Float(1.5)));
        assert_eq!(
            parse_cell(DataType::Float, "-2e3"),
            Ok(Value::Float(-2000.0))
        );
        for bad in ["NaN", "inf", "-inf", "infinity", "1e999", "x"] {
            assert!(parse_cell(DataType::Float, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn decimals_keep_their_text() {
        let v = parse_cell(DataType::Decimal, " 12.50 ").unwrap();
        assert_eq!(v.to_string(), "12.50");
        assert!(parse_cell(DataType::Decimal, "1,5").is_err());
        assert!(parse_cell(DataType::Decimal, "abc").is_err());
    }

    #[test]
    fn booleans() {
        for yes in ["true", "TRUE", " t ", "1", "yes"] {
            assert_eq!(parse_cell(DataType::Boolean, yes), Ok(Value::Bool(true)));
        }
        for no in ["false", "False", "f", "0", "no"] {
            assert_eq!(parse_cell(DataType::Boolean, no), Ok(Value::Bool(false)));
        }
        assert!(parse_cell(DataType::Boolean, "maybe").is_err());
        assert!(parse_cell(DataType::Boolean, "2").is_err());
    }

    #[test]
    fn dates_and_times() {
        assert!(matches!(
            parse_cell(DataType::Date, "2024-02-29"),
            Ok(Value::Date(_))
        ));
        assert!(parse_cell(DataType::Date, "2023-02-29").is_err());
        assert!(parse_cell(DataType::Date, "29/02/2024").is_err());
        assert!(matches!(
            parse_cell(DataType::Time, "13:45:10.5"),
            Ok(Value::Time(_))
        ));
        assert!(parse_cell(DataType::Time, "25:00:00").is_err());
        assert!(matches!(
            parse_cell(DataType::Timestamp, "2024-05-06 07:08:09"),
            Ok(Value::Timestamp(_))
        ));
        assert!(parse_cell(DataType::Timestamp, "2024-05-06").is_err());
        assert!(matches!(
            parse_cell(DataType::TimestampTz, "2024-05-06 07:08:09+02:00"),
            Ok(Value::TimestampTz(_))
        ));
        assert!(parse_cell(DataType::TimestampTz, "2024-05-06 07:08:09").is_err());
    }

    #[test]
    fn json_is_not_checked_on_the_client() {
        assert_eq!(
            parse_cell(DataType::Json, "{not json"),
            Ok(Value::Json("{not json".to_owned()))
        );
        assert!(parse_cell(DataType::Json, "  ").is_err());
    }

    #[test]
    fn binary_is_read_only() {
        assert_eq!(
            parse_cell(DataType::Binary, "00ff"),
            Err("Binary cells are read-only.".to_owned())
        );
    }

    #[test]
    fn empty_input_is_an_error_except_for_text() {
        for t in ALL {
            let r = parse_cell(t, "");
            match editor_kind(t) {
                EditorKind::Text => assert_eq!(r, Ok(Value::Text(String::new()))),
                EditorKind::ReadOnlyBinary => assert!(r.is_err()),
                _ => assert_eq!(r, Err("Enter a value or use Set NULL.".to_owned()), "{t:?}"),
            }
        }
        assert!(parse_cell(DataType::Integer, "   ").is_err());
    }

    #[test]
    fn errors_never_repeat_the_input() {
        for t in ALL {
            if let Err(e) = parse_cell(t, "sekrit-input") {
                assert!(!e.contains("sekrit"), "{t:?}: {e}");
            }
        }
    }

    #[test]
    fn edit_text_round_trips_through_parse_cell() {
        let values = [
            (DataType::Boolean, Value::Bool(true)),
            (DataType::Boolean, Value::Bool(false)),
            (DataType::Integer, Value::Int(i64::MIN)),
            (DataType::Integer, Value::Int(0)),
            (DataType::Float, Value::Float(1.5)),
            (DataType::Float, Value::Float(-0.000_001)),
            (DataType::Float, Value::Float(1e300)),
            (DataType::Text, Value::Text("a b\nc".to_owned())),
            (DataType::Json, Value::Json("{\"a\": 1}".to_owned())),
            (
                DataType::Decimal,
                parse_cell(DataType::Decimal, "-0.10").unwrap(),
            ),
            (
                DataType::Date,
                parse_cell(DataType::Date, "2024-01-31").unwrap(),
            ),
            (
                DataType::Time,
                parse_cell(DataType::Time, "01:02:03.25").unwrap(),
            ),
            (
                DataType::Timestamp,
                parse_cell(DataType::Timestamp, "2024-01-31 01:02:03").unwrap(),
            ),
            (
                DataType::TimestampTz,
                parse_cell(DataType::TimestampTz, "2024-01-31 01:02:03-05:30").unwrap(),
            ),
        ];
        for (t, v) in values {
            let text = edit_text(&v);
            assert_eq!(parse_cell(t, &text), Ok(v.clone()), "{t:?} {text}");
        }
    }

    #[test]
    fn null_starts_empty() {
        assert_eq!(edit_text(&Value::Null), "");
    }
}
