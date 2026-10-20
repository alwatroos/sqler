// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `CSV` writer (RFC 4180).

use std::io::Write;

use sqler_core::{Column, Row, Value};

use crate::options::CsvOptions;
use crate::render::{is_textual, plain};
use crate::{CSV, ExportError, ExportOptions, Format, RowWriter, Sink};

/// The `CSV` format.
#[derive(Debug, Clone, Copy, Default)]
pub struct CsvFormat;

impl Format for CsvFormat {
    fn id(&self) -> &'static str {
        CSV
    }

    fn label(&self) -> &'static str {
        "CSV"
    }

    fn extension(&self) -> &'static str {
        "csv"
    }

    fn create<'a>(
        &self,
        options: &ExportOptions,
        sink: Sink<'a>,
    ) -> Result<Box<dyn RowWriter + 'a>, ExportError> {
        let o = &options.csv;
        let d = o.delimiter;
        if !d.is_ascii() || matches!(d, b'"' | b'\r' | b'\n') {
            return Err(ExportError::InvalidOptions(format!(
                "CSV delimiter {:?} must be ASCII and not a quote or line break",
                char::from(d)
            )));
        }
        if o.null_text.contains([char::from(d), '"', '\r', '\n']) {
            return Err(ExportError::InvalidOptions(
                "CSV null text must not contain the delimiter, a quote or a line break".to_owned(),
            ));
        }
        Ok(Box::new(CsvWriter {
            sink,
            options: o.clone(),
            record: String::new(),
            cell: String::new(),
        }))
    }
}

struct CsvWriter<'a> {
    sink: Sink<'a>,
    options: CsvOptions,
    /// The record being built; reused.
    record: String,
    /// The cell being rendered; reused.
    cell: String,
}

impl CsvWriter<'_> {
    /// Appends the field in `self.cell` to the record, quoted when needed.
    fn push_field(&mut self, first: bool, is_null: bool, textual: bool) {
        if !first {
            self.record.push(char::from(self.options.delimiter));
        }
        if self.options.neutralize_formulas
            && textual
            && matches!(
                self.cell.as_bytes().first(),
                Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r')
            )
        {
            self.cell.insert(0, '\'');
        }
        let delimiter = char::from(self.options.delimiter);
        let quote = self.cell.contains([delimiter, '"', '\r', '\n'])
            || (!is_null && self.cell == self.options.null_text);
        if quote {
            self.record.push('"');
            for c in self.cell.chars() {
                if c == '"' {
                    self.record.push('"');
                }
                self.record.push(c);
            }
            self.record.push('"');
        } else {
            self.record.push_str(&self.cell);
        }
    }

    fn end_record(&mut self) -> Result<(), ExportError> {
        self.record.push_str(self.options.line_ending.as_str());
        self.sink.write_all(self.record.as_bytes())?;
        self.record.clear();
        Ok(())
    }
}

impl RowWriter for CsvWriter<'_> {
    fn begin(&mut self, columns: &[Column]) -> Result<(), ExportError> {
        if columns.is_empty() {
            return Err(ExportError::InvalidOptions(
                "CSV export needs at least one column".to_owned(),
            ));
        }
        if self.options.header {
            for (i, column) in columns.iter().enumerate() {
                self.cell.clear();
                self.cell.push_str(&column.name);
                self.push_field(i == 0, false, true);
            }
            self.end_record()?;
        }
        Ok(())
    }

    fn write_batch(&mut self, rows: &[Row]) -> Result<(), ExportError> {
        for row in rows {
            for (i, value) in row.0.iter().enumerate() {
                self.cell.clear();
                plain(&mut self.cell, value, &self.options.null_text);
                self.push_field(i == 0, matches!(value, Value::Null), is_textual(value));
            }
            self.end_record()?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), ExportError> {
        self.sink.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqler_core::DataType;

    use super::*;
    use crate::{LineEnding, export_to_vec};

    fn run(options: &ExportOptions, names: &[&str], rows: Vec<Vec<Value>>) -> String {
        let columns: Vec<_> = names
            .iter()
            .map(|n| Column::new(*n, DataType::Text))
            .collect();
        let rows: Vec<_> = rows.into_iter().map(Row).collect();
        String::from_utf8(export_to_vec(&CsvFormat, options, &columns, &rows).unwrap()).unwrap()
    }

    fn t(s: &str) -> Value {
        Value::Text(s.into())
    }

    #[test]
    fn quoting_rules() {
        let out = run(
            &ExportOptions::default(),
            &["a", "b,c", "d\"e"],
            vec![
                vec![t("x,y"), t("q\"r"), t("l1\r\nl2")],
                vec![t("plain"), t("l\nf"), t("")],
            ],
        );
        assert_eq!(
            out,
            "a,\"b,c\",\"d\"\"e\"\r\n\"x,y\",\"q\"\"r\",\"l1\r\nl2\"\r\n\
             plain,\"l\nf\",\"\"\r\n"
        );
    }

    #[test]
    fn null_versus_empty_text() {
        let mut o = ExportOptions::default();
        let rows = || vec![vec![Value::Null, t("")]];
        // Default null text is empty: an empty string is quoted to tell them apart.
        assert_eq!(run(&o, &["a", "b"], rows()), "a,b\r\n,\"\"\r\n");
        o.csv.null_text = "NULL".into();
        assert_eq!(
            run(
                &o,
                &["a", "b"],
                vec![vec![Value::Null, t("NULL")], vec![t(""), t("x")]]
            ),
            "a,b\r\nNULL,\"NULL\"\r\n,x\r\n"
        );
    }

    #[test]
    fn options_delimiter_header_line_ending() {
        let mut o = ExportOptions::default();
        o.csv.delimiter = b';';
        o.csv.header = false;
        o.csv.line_ending = LineEnding::Lf;
        assert_eq!(
            run(
                &o,
                &["a", "b"],
                vec![
                    vec![t("x;y"), t("a,b")],
                    vec![Value::Int(1), Value::Float(f64::NAN)]
                ]
            ),
            "\"x;y\";a,b\n1;NaN\n"
        );
        o.csv.delimiter = b'\t';
        assert_eq!(run(&o, &["a"], vec![vec![t("a\tb")]]), "\"a\tb\"\n");
    }

    #[test]
    fn invalid_options() {
        for (delimiter, null_text) in [
            (b'"', ""),
            (b'\n', ""),
            (b'\r', ""),
            (0xC3, ""),
            (b',', "a,b"),
            (b',', "\"x"),
            (b',', "a\nb"),
            (b';', "a;b"),
        ] {
            let mut o = ExportOptions::default();
            o.csv.delimiter = delimiter;
            o.csv.null_text = null_text.into();
            let err = CsvFormat.create(&o, Box::new(Vec::new())).err();
            assert!(
                matches!(err, Some(ExportError::InvalidOptions(_))),
                "{delimiter} {null_text:?}"
            );
        }
        let mut o = ExportOptions::default();
        o.csv.null_text = "a;b".into();
        assert!(CsvFormat.create(&o, Box::new(Vec::new())).is_ok());
    }

    #[test]
    fn formula_neutralization_is_opt_in() {
        let cells = [
            "=1+1", "+1", "-1", "@SUM(A1)", "\tx", "\rx", "safe", "a=b", "",
        ];
        let rows = || vec![cells.iter().map(|c| t(c)).collect::<Vec<_>>()];
        let names = ["=h", "n", "n", "n", "n", "n", "n", "n", "n"];
        let mut o = ExportOptions::default();
        let off = run(&o, &names, rows());
        assert_eq!(
            off,
            "=h,n,n,n,n,n,n,n,n\r\n=1+1,+1,-1,@SUM(A1),\tx,\"\rx\",safe,a=b,\"\"\r\n"
        );
        o.csv.neutralize_formulas = true;
        let on = run(&o, &names, rows());
        assert_eq!(
            on,
            "'=h,n,n,n,n,n,n,n,n\r\n'=1+1,'+1,'-1,'@SUM(A1),'\tx,\"'\rx\",safe,a=b,\"\"\r\n"
        );
    }

    #[test]
    fn neutralization_leaves_numbers_and_nulls_alone() {
        let mut o = ExportOptions::default();
        o.csv.neutralize_formulas = true;
        let out = run(
            &o,
            &["a", "b", "c", "d"],
            vec![vec![
                Value::Int(-5),
                Value::Float(-1.5),
                Value::Null,
                Value::Json("-1".into()),
            ]],
        );
        assert_eq!(out, "a,b,c,d\r\n-5,-1.5,,'-1\r\n");
    }

    #[test]
    fn zero_columns_rejected() {
        assert!(matches!(
            export_to_vec(&CsvFormat, &ExportOptions::default(), &[], &[]),
            Err(ExportError::InvalidOptions(_))
        ));
    }
}
