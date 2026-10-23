// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Exporter fixture plugin (`exporter-plugin` world): a tab-separated format. Text cells are written as they are and
//! null as an empty field; values are separated by a tab and rows end with a line feed. `finish` returns `# end`. A
//! text cell `!trap` makes the guest trap, and `!fail` makes it return a `plugin-error` whose message holds a line
//! break and a right-to-left override, so the host tests
//! (`crates/sqler-plugin-host/tests/guests/exporter.rs`) can drive every path of the adapter.

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items (AGENTS.md section 4 exception),
// so the generated code and the exports live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/sqler-plugin-api/wit",
        world: "exporter-plugin",
    });

    use self::exports::sqler::plugin::exporter::{Guest, GuestWriter, Writer};
    use self::exports::sqler::plugin::lifecycle;
    use self::sqler::plugin::log::{Level, write};
    use self::sqler::plugin::types::{Column, ErrorCode, PluginError, Value};

    struct Fixture;

    fn fail(message: impl Into<String>) -> PluginError {
        PluginError {
            code: ErrorCode::Failed,
            message: message.into(),
        }
    }

    impl lifecycle::Guest for Fixture {
        fn init() -> Result<(), lifecycle::PluginError> {
            write(Level::Info, "exporter-fixture: init");
            Ok(())
        }
    }

    impl Guest for Fixture {
        type Writer = Tsv;

        fn start(format: String, _columns: Vec<Column>) -> Result<Writer, PluginError> {
            if format != "tsv" {
                return Err(fail(format!("unknown format {format}")));
            }
            Ok(Writer::new(Tsv))
        }
    }

    /// An open export.
    struct Tsv;

    fn field(value: &Value) -> String {
        match value {
            Value::Null => String::new(),
            Value::Boolean(b) => b.to_string(),
            Value::Integer(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bytes(b) => b.iter().map(|byte| format!("{byte:02x}")).collect(),
            Value::Text(s)
            | Value::Decimal(s)
            | Value::Date(s)
            | Value::Time(s)
            | Value::Timestamp(s)
            | Value::TimestampTz(s)
            | Value::Json(s) => s.clone(),
        }
    }

    impl GuestWriter for Tsv {
        fn push(&self, rows: Vec<Vec<Value>>) -> Result<Vec<u8>, PluginError> {
            let mut out = String::new();
            for row in &rows {
                for (i, value) in row.iter().enumerate() {
                    if let Value::Text(text) = value {
                        match text.as_str() {
                            "!trap" => core::arch::wasm32::unreachable(),
                            "!fail" => return Err(fail("bad\nrow\u{202E}here")),
                            _ => {}
                        }
                    }
                    if i > 0 {
                        out.push('\t');
                    }
                    out.push_str(&field(value));
                }
                out.push('\n');
            }
            Ok(out.into_bytes())
        }

        fn finish(&self) -> Result<Vec<u8>, PluginError> {
            Ok(b"# end\n".to_vec())
        }
    }

    export!(Fixture);
}
