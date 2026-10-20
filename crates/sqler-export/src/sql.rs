// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SQL` writer: multi-row `INSERT` statements.

use std::io::Write;

use sqler_core::{Column, Row};
use sqler_sql::{Dialect, push_literal, quote_identifier};

use crate::{ExportError, ExportOptions, Format, RowWriter, SQL, Sink};

/// The `SQL` `INSERT` format.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqlFormat;

impl Format for SqlFormat {
    fn id(&self) -> &'static str {
        SQL
    }

    fn label(&self) -> &'static str {
        "SQL INSERT"
    }

    fn extension(&self) -> &'static str {
        "sql"
    }

    fn create<'a>(
        &self,
        options: &ExportOptions,
        sink: Sink<'a>,
    ) -> Result<Box<dyn RowWriter + 'a>, ExportError> {
        let o = &options.sql;
        if o.table.is_empty() {
            return Err(ExportError::InvalidOptions(
                "SQL export needs a table name".to_owned(),
            ));
        }
        Ok(Box::new(SqlWriter {
            sink,
            dialect: o.dialect,
            schema: o.schema.clone(),
            table: o.table.clone(),
            per_statement: o.rows_per_statement.get(),
            head: String::new(),
            in_statement: 0,
            line: String::new(),
        }))
    }
}

struct SqlWriter<'a> {
    sink: Sink<'a>,
    dialect: Dialect,
    schema: Option<String>,
    table: String,
    per_statement: usize,
    /// `INSERT INTO <table> (<columns>) VALUES\n`, built in `begin`.
    head: String,
    /// Rows in the open statement; 0 when none is open.
    in_statement: usize,
    /// The text of the row being built; reused.
    line: String,
}

impl RowWriter for SqlWriter<'_> {
    fn begin(&mut self, columns: &[Column]) -> Result<(), ExportError> {
        if columns.is_empty() {
            return Err(ExportError::InvalidOptions(
                "SQL export needs at least one column".to_owned(),
            ));
        }
        let d = self.dialect;
        let mut head = String::from("INSERT INTO ");
        if let Some(schema) = &self.schema {
            head.push_str(&quote_identifier(d, schema)?);
            head.push('.');
        }
        head.push_str(&quote_identifier(d, &self.table)?);
        head.push_str(" (");
        for (i, column) in columns.iter().enumerate() {
            if i > 0 {
                head.push_str(", ");
            }
            head.push_str(&quote_identifier(d, &column.name)?);
        }
        head.push_str(") VALUES\n");
        self.head = head;
        Ok(())
    }

    fn write_batch(&mut self, rows: &[Row]) -> Result<(), ExportError> {
        for row in rows {
            self.line.clear();
            if self.in_statement == 0 {
                self.line.push_str(&self.head);
            } else {
                self.line.push_str(",\n");
            }
            self.line.push('(');
            for (i, value) in row.0.iter().enumerate() {
                if i > 0 {
                    self.line.push_str(", ");
                }
                push_literal(&mut self.line, self.dialect, value);
            }
            self.line.push(')');
            self.in_statement += 1;
            if self.in_statement == self.per_statement {
                self.line.push_str(";\n");
                self.in_statement = 0;
            }
            self.sink.write_all(self.line.as_bytes())?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), ExportError> {
        if self.in_statement > 0 {
            self.sink.write_all(b";\n")?;
        }
        self.sink.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use sqler_core::{DataType, Value};

    use super::*;
    use crate::export_to_vec;

    fn options(dialect: Dialect, per: usize) -> ExportOptions {
        let mut o = ExportOptions::default();
        o.sql.dialect = dialect;
        o.sql.table = "t\"x".into();
        o.sql.rows_per_statement = NonZeroUsize::new(per).unwrap();
        o
    }

    fn cols() -> Vec<Column> {
        vec![
            Column::new("id", DataType::Integer),
            Column::new("na`me", DataType::Text),
        ]
    }

    fn rows(n: i64) -> Vec<Row> {
        (0..n)
            .map(|i| Row(vec![Value::Int(i), Value::Text(format!("it's {i}"))]))
            .collect()
    }

    fn run(o: &ExportOptions, n: i64) -> String {
        String::from_utf8(export_to_vec(&SqlFormat, o, &cols(), &rows(n)).unwrap()).unwrap()
    }

    #[test]
    fn statements_split_every_n_rows() {
        let out = run(&options(Dialect::Postgres, 2), 3);
        assert_eq!(
            out,
            "INSERT INTO \"t\"\"x\" (\"id\", \"na`me\") VALUES\n(0, 'it''s 0'),\n(1, 'it''s 1');\n\
             INSERT INTO \"t\"\"x\" (\"id\", \"na`me\") VALUES\n(2, 'it''s 2');\n"
        );
        // An exact multiple closes the last statement once.
        let out = run(&options(Dialect::Postgres, 3), 3);
        assert_eq!(out.matches(';').count(), 1);
        assert!(out.ends_with("2');\n"));
    }

    #[test]
    fn zero_rows_write_nothing() {
        assert_eq!(run(&options(Dialect::Sqlite, 5), 0), "");
    }

    #[test]
    fn schema_and_mysql_quotes() {
        let mut o = options(Dialect::MySql, 10);
        o.sql.schema = Some("s`1".into());
        let out = run(&o, 1);
        assert!(
            out.starts_with("INSERT INTO `s``1`.`t\"x` (`id`, `na``me`) VALUES\n"),
            "{out}"
        );
    }

    #[test]
    fn invalid_input() {
        let mut o = ExportOptions::default();
        assert!(matches!(
            SqlFormat.create(&o, Box::new(Vec::new())).err(),
            Some(ExportError::InvalidOptions(_))
        ));
        o.sql.table = "t".into();
        assert!(matches!(
            export_to_vec(&SqlFormat, &o, &[], &[]),
            Err(ExportError::InvalidOptions(_))
        ));
        let bad = [Column::new("a\0", DataType::Text)];
        assert!(matches!(
            export_to_vec(&SqlFormat, &o, &bad, &[]),
            Err(ExportError::Sql(_))
        ));
        o.sql.schema = Some(String::new());
        assert!(matches!(
            export_to_vec(&SqlFormat, &o, &cols(), &[]),
            Err(ExportError::Sql(_))
        ));
    }
}
