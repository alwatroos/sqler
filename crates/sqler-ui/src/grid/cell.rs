// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fmt::Write as _;

use sqler_driver_api::sqler_core::{Column, DataType};

/// Longest cell text in characters; longer text is cut and ends with `…`.
pub const MAX_CELL_CHARS: usize = 256;

/// The display text of a cell: the `Display` of a `Value` or a
/// `CellRef`, with line breaks and tabs turned into
/// spaces and at most [`MAX_CELL_CHARS`] characters.
#[must_use]
pub fn cell_text(v: &(impl std::fmt::Display + ?Sized)) -> String {
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
        DataType::TimestampTz => "timestamptz",
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
    use sqler_driver_api::sqler_core::{Date, Decimal, Time, Timestamp, TimestampTz, Value};

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
    fn typed_values_render_with_display() {
        let date = Date::new(2024, 2, 29).unwrap();
        let time = Time::new(3, 4, 5, 500_000_000).unwrap();
        let ts = Timestamp::new(date, time);
        assert_eq!(cell_text(&Value::Date(date)), "2024-02-29");
        assert_eq!(cell_text(&Value::Time(time)), "03:04:05.5");
        assert_eq!(cell_text(&Value::Timestamp(ts)), "2024-02-29 03:04:05.5");
        let tz = TimestampTz::new(ts, 3600).unwrap();
        assert_eq!(
            cell_text(&Value::TimestampTz(tz)),
            "2024-02-29 03:04:05.5+01:00"
        );
        let dec: Decimal = "-12.50".parse().unwrap();
        assert_eq!(cell_text(&Value::Decimal(dec)), "-12.50");
    }

    #[test]
    fn json_cells_are_single_line_and_cut() {
        assert_eq!(
            cell_text(&Value::Json("{\n\t\"a\": 1\n}".into())),
            "{  \"a\": 1 }"
        );
        let long = cell_text(&Value::Json("x".repeat(MAX_CELL_CHARS + 1)));
        assert_eq!(long.chars().count(), MAX_CELL_CHARS + 1);
        assert!(long.ends_with('…'));
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
            (DataType::TimestampTz, "timestamptz"),
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
