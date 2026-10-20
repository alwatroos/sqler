// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Markdown table writer.

use std::io::Write;

use sqler_core::{Column, Row};

use crate::render::plain;
use crate::{ExportError, ExportOptions, Format, MARKDOWN, RowWriter, Sink};

/// The Markdown table format.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownFormat;

impl Format for MarkdownFormat {
    fn id(&self) -> &'static str {
        MARKDOWN
    }

    fn label(&self) -> &'static str {
        "Markdown table"
    }

    fn extension(&self) -> &'static str {
        "md"
    }

    fn create<'a>(
        &self,
        options: &ExportOptions,
        sink: Sink<'a>,
    ) -> Result<Box<dyn RowWriter + 'a>, ExportError> {
        Ok(Box::new(MarkdownWriter {
            sink,
            null_text: options.markdown.null_text.clone(),
            line: String::new(),
            cell: String::new(),
        }))
    }
}

struct MarkdownWriter<'a> {
    sink: Sink<'a>,
    null_text: String,
    line: String,
    cell: String,
}

/// Appends `text` as a table cell: `\` and `|` are escaped, line breaks become `<br>`.
fn push_escaped(out: &mut String, text: &str) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("<br>");
            }
            '\n' => out.push_str("<br>"),
            c => out.push(c),
        }
    }
}

impl RowWriter for MarkdownWriter<'_> {
    fn begin(&mut self, columns: &[Column]) -> Result<(), ExportError> {
        if columns.is_empty() {
            return Err(ExportError::InvalidOptions(
                "Markdown export needs at least one column".to_owned(),
            ));
        }
        self.line.clear();
        self.line.push('|');
        for column in columns {
            self.line.push(' ');
            push_escaped(&mut self.line, &column.name);
            self.line.push_str(" |");
        }
        self.line.push_str("\n|");
        for _ in columns {
            self.line.push_str(" --- |");
        }
        self.line.push('\n');
        self.sink.write_all(self.line.as_bytes())?;
        Ok(())
    }

    fn write_batch(&mut self, rows: &[Row]) -> Result<(), ExportError> {
        for row in rows {
            self.line.clear();
            self.line.push('|');
            for value in &row.0 {
                self.cell.clear();
                plain(&mut self.cell, value, &self.null_text);
                self.line.push(' ');
                push_escaped(&mut self.line, &self.cell);
                self.line.push_str(" |");
            }
            self.line.push('\n');
            self.sink.write_all(self.line.as_bytes())?;
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
    use sqler_core::{DataType, Value};

    use super::*;
    use crate::export_to_vec;

    fn run(options: &ExportOptions, names: &[&str], rows: Vec<Vec<Value>>) -> String {
        let columns: Vec<_> = names
            .iter()
            .map(|n| Column::new(*n, DataType::Text))
            .collect();
        let rows: Vec<_> = rows.into_iter().map(Row).collect();
        String::from_utf8(export_to_vec(&MarkdownFormat, options, &columns, &rows).unwrap())
            .unwrap()
    }

    #[test]
    fn table_layout_and_null_text() {
        let mut o = ExportOptions::default();
        let rows = || vec![vec![Value::Int(1), Value::Null]];
        assert_eq!(
            run(&o, &["a", "b"], rows()),
            "| a | b |\n| --- | --- |\n| 1 | NULL |\n"
        );
        o.markdown.null_text = String::new();
        assert_eq!(
            run(&o, &["a", "b"], rows()),
            "| a | b |\n| --- | --- |\n| 1 |  |\n"
        );
        assert_eq!(run(&o, &["a"], vec![]), "| a |\n| --- |\n");
    }

    #[test]
    fn cell_escaping() {
        let out = run(
            &ExportOptions::default(),
            &["a|b"],
            vec![
                vec![Value::Text("x|y\\z".into())],
                vec![Value::Text("l1\r\nl2\nl3\rl4".into())],
            ],
        );
        assert_eq!(
            out,
            "| a\\|b |\n| --- |\n| x\\|y\\\\z |\n| l1<br>l2<br>l3<br>l4 |\n"
        );
    }

    #[test]
    fn zero_columns_rejected() {
        assert!(matches!(
            export_to_vec(&MarkdownFormat, &ExportOptions::default(), &[], &[]),
            Err(ExportError::InvalidOptions(_))
        ));
    }
}
