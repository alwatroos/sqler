// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `JSON` writer: an array of objects, one object per line.

use std::fmt::Write as _;
use std::io::Write;

use sqler_core::{Column, Row, Value};

use crate::render::unique_names;
use crate::{ExportError, ExportOptions, Format, JSON, RowWriter, Sink};

/// The `JSON` format.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonFormat;

impl Format for JsonFormat {
    fn id(&self) -> &'static str {
        JSON
    }

    fn label(&self) -> &'static str {
        "JSON"
    }

    fn extension(&self) -> &'static str {
        "json"
    }

    fn create<'a>(
        &self,
        _options: &ExportOptions,
        sink: Sink<'a>,
    ) -> Result<Box<dyn RowWriter + 'a>, ExportError> {
        Ok(Box::new(JsonWriter {
            sink,
            keys: Vec::new(),
            wrote_any: false,
            line: Vec::new(),
            scratch: String::new(),
        }))
    }
}

struct JsonWriter<'a> {
    sink: Sink<'a>,
    /// For every column the encoded key and the colon: `"name":`.
    keys: Vec<Vec<u8>>,
    wrote_any: bool,
    /// The row being built; reused.
    line: Vec<u8>,
    scratch: String,
}

fn push_string(line: &mut Vec<u8>, text: &str) -> Result<(), ExportError> {
    serde_json::to_writer(&mut *line, text).map_err(std::io::Error::from)?;
    Ok(())
}

fn push_value(line: &mut Vec<u8>, scratch: &mut String, value: &Value) -> Result<(), ExportError> {
    match value {
        Value::Null => line.extend_from_slice(b"null"),
        Value::Bool(b) => line.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Int(i) => {
            scratch.clear();
            let _ = write!(scratch, "{i}");
            line.extend_from_slice(scratch.as_bytes());
        }
        Value::Float(x) if x.is_finite() => {
            serde_json::to_writer(&mut *line, x).map_err(std::io::Error::from)?;
        }
        Value::Float(x) => push_string(
            line,
            if x.is_nan() {
                "NaN"
            } else if *x < 0.0 {
                "-Infinity"
            } else {
                "Infinity"
            },
        )?,
        Value::Text(t) => push_string(line, t)?,
        Value::Json(t) => {
            let trimmed = t.trim();
            if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
                line.extend_from_slice(trimmed.as_bytes());
            } else {
                push_string(line, t)?;
            }
        }
        other => {
            scratch.clear();
            let _ = write!(scratch, "{other}");
            push_string(line, scratch)?;
        }
    }
    Ok(())
}

impl RowWriter for JsonWriter<'_> {
    fn begin(&mut self, columns: &[Column]) -> Result<(), ExportError> {
        self.keys = Vec::with_capacity(columns.len());
        for name in unique_names(columns) {
            let mut key = Vec::new();
            push_string(&mut key, &name)?;
            key.push(b':');
            self.keys.push(key);
        }
        Ok(())
    }

    fn write_batch(&mut self, rows: &[Row]) -> Result<(), ExportError> {
        for row in rows {
            self.line.clear();
            self.line
                .extend_from_slice(if self.wrote_any { b",\n" } else { b"[\n" });
            self.line.push(b'{');
            for (i, (key, value)) in self.keys.iter().zip(&row.0).enumerate() {
                if i > 0 {
                    self.line.push(b',');
                }
                self.line.extend_from_slice(key);
                push_value(&mut self.line, &mut self.scratch, value)?;
            }
            self.line.push(b'}');
            self.sink.write_all(&self.line)?;
            self.wrote_any = true;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), ExportError> {
        self.sink
            .write_all(if self.wrote_any { b"\n]\n" } else { b"[]\n" })?;
        self.sink.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqler_core::{DataType, Date};

    use super::*;
    use crate::export_to_vec;

    fn run(names: &[&str], rows: Vec<Vec<Value>>) -> String {
        let columns: Vec<_> = names
            .iter()
            .map(|n| Column::new(*n, DataType::Text))
            .collect();
        let rows: Vec<_> = rows.into_iter().map(Row).collect();
        String::from_utf8(
            export_to_vec(&JsonFormat, &ExportOptions::default(), &columns, &rows).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn layout() {
        assert_eq!(run(&["a"], vec![]), "[]\n");
        assert_eq!(
            run(
                &["a", "b"],
                vec![
                    vec![Value::Int(1), Value::Null],
                    vec![Value::Bool(true), Value::Text("x\"y\n".into())]
                ]
            ),
            "[\n{\"a\":1,\"b\":null},\n{\"a\":true,\"b\":\"x\\\"y\\n\"}\n]\n"
        );
    }

    #[test]
    fn duplicate_names_are_deduplicated_once() {
        let out = run(&["a", "a", "a", "a_2"], vec![vec![Value::Int(1); 4]]);
        assert_eq!(out, "[\n{\"a\":1,\"a_2\":1,\"a_3\":1,\"a_2_2\":1}\n]\n");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v[0].as_object().unwrap().len(), 4);
    }

    #[test]
    fn json_values_are_embedded_or_fall_back_to_strings() {
        let out = run(
            &["ok", "bad", "scalar"],
            vec![vec![
                Value::Json(" {\"a\": [1, 2]} ".into()),
                Value::Json("{oops".into()),
                Value::Json("5".into()),
            ]],
        );
        assert_eq!(
            out,
            "[\n{\"ok\":{\"a\": [1, 2]},\"bad\":\"{oops\",\"scalar\":5}\n]\n"
        );
    }

    #[test]
    fn floats_and_non_finite() {
        let out = run(
            &["a", "b", "c", "d", "e"],
            vec![vec![
                Value::Float(1.5),
                Value::Float(-0.0),
                Value::Float(f64::NAN),
                Value::Float(f64::INFINITY),
                Value::Float(f64::NEG_INFINITY),
            ]],
        );
        assert_eq!(
            out,
            "[\n{\"a\":1.5,\"b\":-0.0,\"c\":\"NaN\",\"d\":\"Infinity\",\"e\":\"-Infinity\"}\n]\n"
        );
    }

    #[test]
    fn strings_for_decimal_temporal_and_bytes() {
        let out = run(
            &["a", "b", "c"],
            vec![vec![
                Value::Decimal("1.50".parse().unwrap()),
                Value::Date(Date::new(2024, 1, 2).unwrap()),
                Value::Bytes(vec![0xde, 0xad]),
            ]],
        );
        assert_eq!(
            out,
            "[\n{\"a\":\"1.50\",\"b\":\"2024-01-02\",\"c\":\"0xdead\"}\n]\n"
        );
    }
}
