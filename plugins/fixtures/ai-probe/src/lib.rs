// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! AI probe fixture plugin (`ai-tool-plugin` world). Four tools: `echo` returns its arguments, `query` runs the `sql`
//! setting on the `connection` setting, `trap` traps and `spin` never returns. The host tests
//! (`crates/sqler-plugin-host/tests/guests_ai.rs`) drive them. JSON is written by hand (no serde in the guest).

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items (AGENTS.md section 4 exception),
// so the generated code and the exports live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/sqler-plugin-api/wit",
        world: "ai-tool-plugin",
    });

    use std::fmt::Write as _;

    use self::exports::sqler::plugin::ai_tools::{Guest, PluginError};
    use self::exports::sqler::plugin::lifecycle;
    use self::sqler::plugin::log::{Level, write};
    use self::sqler::plugin::query::{QueryRequest, run};
    use self::sqler::plugin::settings::{self, SettingValue};
    use self::sqler::plugin::types::{ErrorCode, Value};

    struct AiProbe;

    impl lifecycle::Guest for AiProbe {
        fn init() -> Result<(), lifecycle::PluginError> {
            write(Level::Info, "ai-probe: init");
            Ok(())
        }
    }

    impl Guest for AiProbe {
        fn invoke(tool: String, arguments_json: String) -> Result<String, PluginError> {
            match tool.as_str() {
                "echo" => Ok(arguments_json),
                "query" => Ok(query()),
                "trap" => {
                    core::arch::wasm32::unreachable();
                }
                "spin" => loop {
                    std::hint::black_box(());
                },
                other => Err(PluginError {
                    code: ErrorCode::NotFound,
                    message: format!("unknown tool {other}"),
                }),
            }
        }
    }

    fn text_setting(key: &str) -> String {
        match settings::get(key) {
            Some(SettingValue::Text(t)) => t,
            _ => String::new(),
        }
    }

    fn code(code: ErrorCode) -> &'static str {
        match code {
            ErrorCode::CapabilityDenied => "capability-denied",
            ErrorCode::Declined => "declined",
            ErrorCode::InvalidArgument => "invalid-argument",
            ErrorCode::NotFound => "not-found",
            ErrorCode::LimitExceeded => "limit-exceeded",
            ErrorCode::Timeout => "timeout",
            ErrorCode::Failed => "failed",
        }
    }

    /// A JSON string literal with the escapes JSON requires.
    fn quote(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    fn cell(v: &Value) -> String {
        match v {
            Value::Null => "null".to_owned(),
            Value::Boolean(b) => b.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Float(f) if f.is_finite() => f.to_string(),
            Value::Float(_) => "null".to_owned(),
            Value::Text(t)
            | Value::Decimal(t)
            | Value::Date(t)
            | Value::Time(t)
            | Value::Timestamp(t)
            | Value::TimestampTz(t)
            | Value::Json(t) => quote(t),
            Value::Bytes(b) => quote(&format!("<{} bytes>", b.len())),
        }
    }

    /// Runs the `sql` setting on the `connection` setting: `{"columns":[..],"rows":[[..]]}` or `{"error":"<code>"}`.
    fn query() -> String {
        let request = QueryRequest {
            connection: text_setting("connection"),
            sql: text_setting("sql"),
            params: Vec::<Value>::new(),
            max_rows: None,
            timeout_ms: None,
        };
        match run(&request) {
            Ok(result) => {
                let columns: Vec<String> = result.columns.iter().map(|c| quote(&c.name)).collect();
                let rows: Vec<String> = result
                    .rows
                    .iter()
                    .map(|row| {
                        let cells: Vec<String> = row.iter().map(cell).collect();
                        format!("[{}]", cells.join(","))
                    })
                    .collect();
                format!(
                    "{{\"columns\":[{}],\"rows\":[{}]}}",
                    columns.join(","),
                    rows.join(",")
                )
            }
            Err(e) => format!("{{\"error\":{}}}", quote(code(e.code))),
        }
    }

    export!(AiProbe);
}
