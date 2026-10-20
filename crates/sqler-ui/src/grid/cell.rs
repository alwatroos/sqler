// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fmt::Write as _;

use sqler_driver_api::sqler_core::{Column, DataType, Value};

/// Longest cell text in characters; longer text is cut and ends with `…`.
pub const MAX_CELL_CHARS: usize = 256;

/// The display text of a cell: the value's `Display`, with line breaks and tabs turned into
/// spaces and at most [`MAX_CELL_CHARS`] characters.
#[must_use]
pub fn cell_text(v: &Value) -> String {
    let mut w = Bounded {
        out: String::new(),
        chars: 0,
        truncated: false,
    };
    // The writer stops accepting text past the limit; that error is the expected end.
    let _ = write!(w, "{v}");
    let mut text = w.out;
    if w.truncated {
        text.push('…');
    }
    text
}

/// A `fmt::Write` sink that keeps at most [`MAX_CELL_CHARS`] characters (line breaks and tabs
/// become spaces) and fails once more text arrives, so a huge value is not fully formatted.
struct Bounded {
    out: String,
    chars: usize,
    truncated: bool,
}

impl std::fmt::Write for Bounded {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for c in s.chars() {
            if self.chars == MAX_CELL_CHARS {
                self.truncated = true;
                return Err(std::fmt::Error);
            }
            self.out.push(if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            });
            self.chars += 1;
        }
        Ok(())
    }
}

/// A short lower-case label for a column type.
#[must_use]
pub fn type_label(t: DataType) -> &'static str {
    match t {
        DataType::Boolean => "boolean",
        DataType::Integer => "integer",
        DataType::Float => "float",
        DataType::Decimal => "decimal",
        DataType::Text => "text",
        DataType::Binary => "binary",
        DataType::Date => "date",
        DataType::Time => "time",
        DataType::Timestamp => "timestamp",
        DataType::Json => "json",
        _ => "other",
    }
}

/// What a column header shows: the name and the type label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnHeader {
    /// The column name.
    pub name: String,
    /// The type label, see [`type_label`].
    pub type_label: &'static str,
}

impl From<&Column> for ColumnHeader {
    fn from(c: &Column) -> Self {
        Self {
            name: c.name.clone(),
            type_label: type_label(c.data_type),
        }
    }
}

/// Formats a count with `,` thousands separators: `1000000` becomes `1,000,000`.
#[must_use]
pub fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_text_is_cut() {
        let t = cell_text(&Value::Text("é".repeat(5_000_000)));
        assert_eq!(t.chars().count(), MAX_CELL_CHARS + 1);
        assert!(t.ends_with('…'));
        let exact = cell_text(&Value::Text("a".repeat(MAX_CELL_CHARS)));
        assert_eq!(exact.chars().count(), MAX_CELL_CHARS);
        let bytes = cell_text(&Value::Bytes(vec![0xab; 1_000_000]));
        assert_eq!(bytes.chars().count(), MAX_CELL_CHARS + 1);
    }

    #[test]
    fn values_render_with_display() {
        assert_eq!(cell_text(&Value::Null), "NULL");
        assert_eq!(cell_text(&Value::Int(-4)), "-4");
        assert_eq!(cell_text(&Value::Bool(true)), "true");
        assert_eq!(cell_text(&Value::Float(1.5)), "1.5");
        assert_eq!(cell_text(&Value::Bytes(vec![0xde, 0xad])), "0xdead");
    }

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(cell_text(&Value::Text("a\nb\r\nc\td".into())), "a b  c d");
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        let exact = "é".repeat(MAX_CELL_CHARS);
        assert_eq!(cell_text(&Value::Text(exact.clone())), exact);
        let long = "é".repeat(MAX_CELL_CHARS + 1);
        let cut = cell_text(&Value::Text(long));
        assert_eq!(cut.chars().count(), MAX_CELL_CHARS + 1);
        assert!(cut.ends_with('…'));
        let big = cell_text(&Value::Bytes(vec![0xab; 1_000]));
        assert_eq!(big.chars().count(), MAX_CELL_CHARS + 1);
    }

    #[test]
    fn every_type_has_a_label() {
        let cases = [
            (DataType::Boolean, "boolean"),
            (DataType::Integer, "integer"),
            (DataType::Float, "float"),
            (DataType::Decimal, "decimal"),
            (DataType::Text, "text"),
            (DataType::Binary, "binary"),
            (DataType::Date, "date"),
            (DataType::Time, "time"),
            (DataType::Timestamp, "timestamp"),
            (DataType::Json, "json"),
            (DataType::Other, "other"),
        ];
        for (t, label) in cases {
            assert_eq!(type_label(t), label);
        }
    }

    #[test]
    fn header_from_column() {
        let h = ColumnHeader::from(&Column::new("id", DataType::Integer));
        assert_eq!((h.name.as_str(), h.type_label), ("id", "integer"));
    }

    #[test]
    fn thousands() {
        for (n, s) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1,000"),
            (12_345, "12,345"),
            (100_000, "100,000"),
            (1_000_000, "1,000,000"),
        ] {
            assert_eq!(group_thousands(n), s);
        }
    }
}
