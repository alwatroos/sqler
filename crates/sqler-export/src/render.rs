// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plain-text rendering shared by `CSV` and Markdown.

use std::collections::HashSet;
use std::fmt::Write as _;

use sqler_core::{Column, Value};

/// Appends the plain text of `value` to `out`.
///
/// `NULL` gives `null_text`; booleans are `true`/`false`; finite floats use `Display`; NaN
/// and the infinities are `NaN`, `Infinity` and `-Infinity`; bytes are `0x` and lowercase hex;
/// `JSON` is its raw text; every other value uses its `Display`, as the grid does.
pub(crate) fn plain(out: &mut String, value: &Value, null_text: &str) {
    match value {
        Value::Null => out.push_str(null_text),
        Value::Float(x) => push_float(out, *x),
        Value::Text(t) | Value::Json(t) => out.push_str(t),
        other => {
            let _ = write!(out, "{other}");
        }
    }
}

/// Column names made unique: a repeat gets `_2`, `_3`, ... appended.
pub(crate) fn unique_names(columns: &[Column]) -> Vec<String> {
    let mut used: HashSet<String> = HashSet::new();
    let mut names = Vec::with_capacity(columns.len());
    for column in columns {
        let mut name = column.name.clone();
        let mut n = 2_u32;
        while used.contains(&name) {
            name = format!("{}_{n}", column.name);
            n += 1;
        }
        used.insert(name.clone());
        names.push(name);
    }
    names
}

/// Whether the value is text that a spreadsheet might read as a formula.
pub(crate) fn is_textual(value: &Value) -> bool {
    matches!(value, Value::Text(_) | Value::Json(_))
}

fn push_float(out: &mut String, x: f64) {
    if x.is_nan() {
        out.push_str("NaN");
    } else if x.is_infinite() {
        out.push_str(if x < 0.0 { "-Infinity" } else { "Infinity" });
    } else {
        let _ = write!(out, "{x}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: &Value) -> String {
        let mut s = String::new();
        plain(&mut s, v, "~");
        s
    }

    #[test]
    fn plain_text_of_values() {
        assert_eq!(r(&Value::Null), "~");
        assert_eq!(r(&Value::Bool(true)), "true");
        assert_eq!(r(&Value::Int(-3)), "-3");
        assert_eq!(r(&Value::Float(1.5)), "1.5");
        assert_eq!(r(&Value::Float(f64::NAN)), "NaN");
        assert_eq!(r(&Value::Float(f64::INFINITY)), "Infinity");
        assert_eq!(r(&Value::Float(f64::NEG_INFINITY)), "-Infinity");
        assert_eq!(r(&Value::Bytes(vec![0, 255])), "0x00ff");
        assert_eq!(r(&Value::Bytes(vec![])), "0x");
        assert_eq!(r(&Value::Json("{ \"a\": 1 }".into())), "{ \"a\": 1 }");
        assert_eq!(r(&Value::Text("a\\\"b".into())), "a\\\"b");
    }
}
