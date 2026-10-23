// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Containment of the result of an external (plugin) tool that used a connection.
//!
//! The walk behind [`super::Redaction::apply`] knows one shape: `columns` next to `rows`. A
//! plugin with `db.read` may return anything else (`{"records": [..]}`, an array of row
//! objects), so before the walk the value is held to a rule of its own:
//! - on a connection that withholds rows, a value that is not made of recognised parts is
//!   replaced as a whole;
//! - everywhere, a value under an object key that the list names is replaced, including keys of
//!   object rows; next to a `columns` array, array rows (under any key) are masked by column;
//! - a result nested deeper than a fixed cap is withheld as a whole.

use serde_json::{Value, json};

use super::{REDACTED, Redaction};

/// What replaces a result of an unrecognised shape on a connection that withholds rows.
pub(crate) const WITHHELD_SHAPE: &str = "The tool result was withheld: its shape is not recognised and row data is not sent for this connection.";

/// Keys whose string values are messages, not data.
const TEXT_KEYS: &[&str] = &[
    "error",
    "message",
    "status",
    "sql",
    "code",
    "note",
    "warning",
    "reason",
    "redaction",
];

impl Redaction {
    /// Contains `value`, the result of an external tool that used a connection.
    ///
    /// With `send_rows` off, a value that is not scalars (strings only under message-like keys),
    /// objects of those, and arrays of `columns`+`rows` tables is replaced by a note. Then every
    /// value under a listed object key is replaced by [`REDACTED`]. Idempotent.
    #[must_use]
    pub(crate) fn contain_external(&self, mut value: Value, send_rows: bool) -> Value {
        if (!send_rows && !recognised(&value, None, 0)) || !self.mask_keys(&mut value, 0) {
            return json!({"withheld": WITHHELD_SHAPE});
        }
        value
    }

    /// Masks listed keys and listed columns; `false` when the value nests deeper than
    /// [`MAX_DEPTH`] (the caller then withholds the whole result).
    fn mask_keys(&self, value: &mut Value, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        match value {
            Value::Object(map) => {
                self.mask_parallel_cells(map);
                for (key, child) in map.iter_mut() {
                    if !child.is_null() && self.column_redacted(key) {
                        *child = Value::String(REDACTED.into());
                    } else if !self.mask_keys(child, depth + 1) {
                        return false;
                    }
                }
                true
            }
            Value::Array(items) => items.iter_mut().all(|i| self.mask_keys(i, depth + 1)),
            _ => true,
        }
    }

    /// Next to a `columns` array, every array of arrays (whatever its key) is rows: the cells
    /// of a listed column are replaced. Object rows are not touched here; the walk descends
    /// into them and their keys are masked.
    fn mask_parallel_cells(&self, map: &mut serde_json::Map<String, Value>) {
        let Some(Value::Array(cols)) = map.get("columns") else {
            return;
        };
        let hidden: Vec<usize> = cols
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let name = match c {
                    Value::String(s) => Some(s.as_str()),
                    Value::Object(o) => o.get("name").and_then(Value::as_str),
                    _ => None,
                };
                name.is_some_and(|n| self.column_redacted(n))
            })
            .map(|(i, _)| i)
            .collect();
        if hidden.is_empty() {
            return;
        }
        for (key, child) in map.iter_mut() {
            if key == "columns" {
                continue;
            }
            let Value::Array(rows) = child else { continue };
            for row in rows.iter_mut() {
                if let Value::Array(cells) = row {
                    for &i in &hidden {
                        if let Some(cell) = cells.get_mut(i) {
                            *cell = Value::String(REDACTED.into());
                        }
                    }
                }
            }
        }
    }
}

/// Deepest nesting a plugin result may have; a deeper one is withheld.
const MAX_DEPTH: usize = 64;

/// A table: `columns` (an array) next to `rows` (an array of arrays).
fn is_table(map: &serde_json::Map<String, Value>) -> bool {
    matches!(map.get("columns"), Some(Value::Array(_)))
        && matches!(map.get("rows"), Some(Value::Array(rows)) if rows.iter().all(Value::is_array))
}

fn recognised(value: &Value, key: Option<&str>, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(_) => key.is_some_and(|k| TEXT_KEYS.contains(&k)),
        Value::Object(map) => {
            let table = is_table(map);
            map.iter().all(|(k, v)| {
                if table && (k == "columns" || k == "rows") {
                    true
                } else {
                    recognised(v, Some(k), depth + 1)
                }
            })
        }
        // Arrays hold tables only: an array of scalars or of plain objects may be cells or rows.
        Value::Array(items) => items
            .iter()
            .all(|i| matches!(i, Value::Object(m) if is_table(m)) && recognised(i, key, depth + 1)),
    }
}
