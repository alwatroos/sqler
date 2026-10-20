// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fmt;

/// The logical type of a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DataType {
    /// Boolean.
    Boolean,
    /// Integer of any width.
    Integer,
    /// Floating point number.
    Float,
    /// Exact decimal number.
    Decimal,
    /// Text.
    Text,
    /// Binary data.
    Binary,
    /// Calendar date.
    Date,
    /// Time of day.
    Time,
    /// Date and time.
    Timestamp,
    /// JSON document.
    Json,
    /// Any type without a closer match.
    Other,
}

/// A single cell value.
///
/// Temporal and decimal values are returned as [`Value::Text`] until M2.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit integer.
    Int(i64),
    /// 64-bit float.
    Float(f64),
    /// Text.
    Text(String),
    /// Binary data.
    Bytes(Vec<u8>),
}

impl Value {
    /// The data type of this value, or `None` for [`Value::Null`].
    #[must_use]
    pub fn data_type(&self) -> Option<DataType> {
        match self {
            Self::Null => None,
            Self::Bool(_) => Some(DataType::Boolean),
            Self::Int(_) => Some(DataType::Integer),
            Self::Float(_) => Some(DataType::Float),
            Self::Text(_) => Some(DataType::Text),
            Self::Bytes(_) => Some(DataType::Binary),
        }
    }

    /// Whether this value is [`Value::Null`].
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(x) => write!(f, "{x}"),
            Self::Text(s) => f.write_str(s),
            Self::Bytes(bytes) => {
                f.write_str("0x")?;
                for b in bytes {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_types() {
        assert_eq!(Value::Null.data_type(), None);
        assert_eq!(Value::Bool(true).data_type(), Some(DataType::Boolean));
        assert_eq!(Value::Int(1).data_type(), Some(DataType::Integer));
        assert_eq!(Value::Float(1.5).data_type(), Some(DataType::Float));
        assert_eq!(Value::Text(String::new()).data_type(), Some(DataType::Text));
        assert_eq!(Value::Bytes(vec![]).data_type(), Some(DataType::Binary));
    }

    #[test]
    fn null_check() {
        assert!(Value::Null.is_null());
        assert!(!Value::Int(0).is_null());
    }

    #[test]
    fn display() {
        assert_eq!(Value::Null.to_string(), "NULL");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Bool(false).to_string(), "false");
        assert_eq!(Value::Int(-7).to_string(), "-7");
        assert_eq!(Value::Float(1.5).to_string(), "1.5");
        assert_eq!(Value::Text("a b".into()).to_string(), "a b");
        assert_eq!(Value::Bytes(vec![0, 0xab, 0xFF]).to_string(), "0x00abff");
        assert_eq!(Value::Bytes(vec![]).to_string(), "0x");
    }
}
