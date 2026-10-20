// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Tab-separated output of query results.

use std::io::{self, Write};

use sqler_driver_api::sqler_core::{Column, Row};

/// Writes columns and rows as TSV, one line each, with `\n` line ends.
pub(crate) struct TsvWriter<W: Write> {
    out: W,
}

impl<W: Write> TsvWriter<W> {
    pub(crate) fn new(out: W) -> Self {
        Self { out }
    }

    /// Writes the header line. Writes nothing for an empty column list.
    pub(crate) fn columns(&mut self, columns: &[Column]) -> io::Result<()> {
        if columns.is_empty() {
            return Ok(());
        }
        let line = join(columns.iter().map(|c| c.name.clone()));
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\n")
    }

    /// Writes one line per row.
    pub(crate) fn rows(&mut self, rows: &[Row]) -> io::Result<()> {
        for row in rows {
            let line = join(row.0.iter().map(ToString::to_string));
            self.out.write_all(line.as_bytes())?;
            self.out.write_all(b"\n")?;
        }
        Ok(())
    }

    /// Flushes the underlying writer.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

fn join(fields: impl Iterator<Item = String>) -> String {
    let mut line = String::new();
    for (i, field) in fields.enumerate() {
        if i > 0 {
            line.push('\t');
        }
        escape_into(&field, &mut line);
    }
    line
}

fn escape_into(field: &str, out: &mut String) {
    for ch in field.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{
        DataType, Date, Decimal, Time, Timestamp, TimestampTz, Value,
    };

    use super::*;

    fn cols(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .map(|n| Column::new(*n, DataType::Text))
            .collect()
    }

    fn render(columns: &[Column], rows: &[Row]) -> String {
        let mut buf = Vec::new();
        let mut w = TsvWriter::new(&mut buf);
        w.columns(columns).unwrap();
        w.rows(rows).unwrap();
        w.flush().unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn header_null_and_bytes() {
        let rows = [Row(vec![
            Value::Int(1),
            Value::Null,
            Value::Bytes(vec![0xde, 0xad]),
        ])];
        assert_eq!(
            render(&cols(&["a", "b", "c"]), &rows),
            "a\tb\tc\n1\tNULL\t0xdead\n"
        );
    }

    #[test]
    fn typed_values_print_in_normalised_form() {
        let date = Date::new(2024, 2, 29).unwrap();
        let ts = Timestamp::new(date, Time::new(3, 4, 5, 120_000_000).unwrap());
        let rows = [Row(vec![
            Value::Decimal("-12.50".parse::<Decimal>().unwrap()),
            Value::Date(date),
            Value::Time(Time::new(23, 59, 1, 0).unwrap()),
            Value::Timestamp(ts),
            Value::TimestampTz(TimestampTz::new(ts, -(5 * 3600 + 1800)).unwrap()),
            Value::Json("{\"a\":[1,2]}".to_owned()),
        ])];
        assert_eq!(
            render(&cols(&["a", "b", "c", "d", "e", "f"]), &rows),
            "a\tb\tc\td\te\tf\n\
             -12.50\t2024-02-29\t23:59:01\t2024-02-29 03:04:05.12\t2024-02-29 03:04:05.12-05:30\t{\"a\":[1,2]}\n"
        );
    }

    #[test]
    fn json_with_control_characters_is_escaped() {
        let rows = [Row(vec![Value::Json("{\n\t\"a\": 1}".to_owned())])];
        assert_eq!(render(&cols(&["j"]), &rows), "j\n{\\n\\t\"a\": 1}\n");
    }

    #[test]
    fn escaping() {
        let rows = [Row(vec![Value::Text("a\tb\nc\rd\\e".to_owned())])];
        assert_eq!(
            render(&cols(&["x\ty"]), &rows),
            "x\\ty\na\\tb\\nc\\rd\\\\e\n"
        );
    }

    #[test]
    fn empty_columns_write_nothing() {
        assert_eq!(render(&[], &[]), "");
    }
}
