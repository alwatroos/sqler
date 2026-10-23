// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Example plugin `exporter-markdown` (`exporter-plugin` world): the rows of a result as a Markdown table whose
//! integer, float and decimal columns are right-aligned. The host owns the file and writes the bytes this plugin
//! returns, so the plugin needs no capability. It is the target of the "write your first plugin" tutorial, so it stays
//! small and carries no behaviour that exists only for tests.

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items (AGENTS.md section 4 exception),
// so the generated code and the exports live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "../../crates/sqler-plugin-api/wit",
        world: "exporter-plugin",
    });

    use std::cell::Cell;
    use std::fmt::Write as _;

    use self::exports::sqler::plugin::exporter::{Guest, GuestWriter, Writer};
    use self::exports::sqler::plugin::lifecycle;
    use self::sqler::plugin::log::{Level, write};
    use self::sqler::plugin::types::{Column, DataType, ErrorCode, PluginError, Value};

    struct Markdown;

    fn invalid(message: impl Into<String>) -> PluginError {
        PluginError {
            code: ErrorCode::InvalidArgument,
            message: message.into(),
        }
    }

    impl lifecycle::Guest for Markdown {
        fn init() -> Result<(), lifecycle::PluginError> {
            write(Level::Info, "exporter-markdown: init");
            Ok(())
        }
    }

    impl Guest for Markdown {
        type Writer = Table;

        fn start(format: String, columns: Vec<Column>) -> Result<Writer, PluginError> {
            if format != "markdown" {
                return Err(invalid(format!("unknown format {format}")));
            }
            if columns.is_empty() {
                return Err(invalid("a Markdown table needs at least one column"));
            }
            Ok(Writer::new(Table {
                header: header(&columns),
                header_written: Cell::new(false),
            }))
        }
    }

    /// An open export. The interface has no way to return bytes from `start`, so the header waits for the first
    /// `push` or for `finish`.
    struct Table {
        header: String,
        header_written: Cell<bool>,
    }

    /// Appends `text` with the characters that break a table cell escaped.
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

    /// The two header lines: the names, then the alignment row.
    fn header(columns: &[Column]) -> String {
        let mut out = String::from("|");
        for column in columns {
            out.push(' ');
            push_escaped(&mut out, &column.name);
            out.push_str(" |");
        }
        out.push_str("\n|");
        for column in columns {
            let aligned = matches!(
                column.data_type,
                DataType::Integer | DataType::Float | DataType::Decimal
            );
            out.push_str(if aligned { " ---: |" } else { " --- |" });
        }
        out.push('\n');
        out
    }

    fn cell(out: &mut String, value: &Value) {
        match value {
            Value::Null => {}
            Value::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Integer(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Float(f) => {
                let _ = write!(out, "{f}");
            }
            Value::Bytes(bytes) => {
                out.push_str("0x");
                for byte in bytes {
                    let _ = write!(out, "{byte:02x}");
                }
            }
            Value::Text(s)
            | Value::Decimal(s)
            | Value::Date(s)
            | Value::Time(s)
            | Value::Timestamp(s)
            | Value::TimestampTz(s)
            | Value::Json(s) => push_escaped(out, s),
        }
    }

    impl Table {
        /// The header the first time it is asked for, nothing afterwards.
        fn take_header(&self, out: &mut String) {
            if !self.header_written.replace(true) {
                out.push_str(&self.header);
            }
        }
    }

    impl GuestWriter for Table {
        fn push(&self, rows: Vec<Vec<Value>>) -> Result<Vec<u8>, PluginError> {
            let mut out = String::new();
            self.take_header(&mut out);
            for row in &rows {
                out.push('|');
                for value in row {
                    out.push(' ');
                    cell(&mut out, value);
                    out.push_str(" |");
                }
                out.push('\n');
            }
            Ok(out.into_bytes())
        }

        fn finish(&self) -> Result<Vec<u8>, PluginError> {
            let mut out = String::new();
            self.take_header(&mut out);
            Ok(out.into_bytes())
        }
    }

    export!(Markdown);
}
