// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The JSON walk behind [`super::Redaction::apply`].

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::{MAX_SUMMARY_COLUMNS, REDACTED, RedactScope, Redaction, RedactionSummary};

const WITHHELD_NO_ROWS: &str =
    "The error text was withheld because row data is not sent for this connection.";
const WITHHELD_MENTION: &str = "The error text mentions a redacted column and was withheld.";

struct Acc {
    columns: BTreeSet<String>,
    tainted: u32,
    rows_withheld: bool,
    errors: u32,
}

pub(super) fn run(red: &Redaction, value: &mut Value, scope: RedactScope) -> RedactionSummary {
    let mut acc = Acc {
        columns: BTreeSet::new(),
        tainted: 0,
        rows_withheld: false,
        errors: 0,
    };
    walk(red, value, scope, &mut acc);
    RedactionSummary {
        columns: acc.columns.into_iter().take(MAX_SUMMARY_COLUMNS).collect(),
        tainted_statements: acc.tainted,
        rows_withheld: acc.rows_withheld,
        errors_withheld: acc.errors,
    }
}

fn walk(red: &Redaction, value: &mut Value, scope: RedactScope, acc: &mut Acc) {
    match value {
        Value::Object(map) => {
            object(red, map, scope, acc);
            for (key, child) in map.iter_mut() {
                // Cells are data, not structure.
                if key != "rows" {
                    walk(red, child, scope, acc);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(red, item, scope, acc);
            }
        }
        _ => {}
    }
}

/// A word is a maximal run of alphanumerics or `_`.
fn mentions_listed_word(red: &Redaction, text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| !w.is_empty() && red.column_redacted(w))
}

/// The replacement for an error text, if it must be withheld.
///
/// `tainted` marks a statement that names a flagged identifier: its driver error
/// may echo the value (a Postgres cast error quotes it), so the text is withheld.
fn withheld_text(
    red: &Redaction,
    text: &str,
    scope: RedactScope,
    tainted: bool,
) -> Option<&'static str> {
    if text == WITHHELD_NO_ROWS || text == WITHHELD_MENTION {
        return None;
    }
    if !scope.send_rows {
        Some(WITHHELD_NO_ROWS)
    } else if tainted || mentions_listed_word(red, text) {
        Some(WITHHELD_MENTION)
    } else {
        None
    }
}

fn object(red: &Redaction, map: &mut Map<String, Value>, scope: RedactScope, acc: &mut Acc) {
    // R4, driver error object: {"error": {"code": "driver", "message": ".."}}.
    if let Some(Value::Object(err)) = map.get_mut("error")
        && err.get("code").and_then(Value::as_str) == Some("driver")
        && let Some(Value::String(msg)) = err.get_mut("message")
        && let Some(new) = withheld_text(red, msg, scope, false)
    {
        *msg = new.to_owned();
        acc.errors += 1;
    }

    let has_rows = matches!(map.get("rows"), Some(Value::Array(_)));
    let mut touched = false;

    // R1 / R2.
    let names: Vec<Option<String>> = match map.get("columns") {
        Some(Value::Array(cols)) => cols
            .iter()
            .map(|c| c.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    let sql_tainted = map
        .get("sql")
        .and_then(Value::as_str)
        .is_some_and(|sql| red.statement_tainted(scope.dialect, sql));
    let tainted = has_rows && sql_tainted;
    let hidden: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            n.as_deref()
                .is_some_and(|n| tainted || red.column_redacted(n))
        })
        .map(|(i, _)| i)
        .collect();
    let hidden_names: BTreeSet<String> = hidden
        .iter()
        .filter_map(|&i| names.get(i).cloned().flatten())
        .collect();

    if let Some(Value::Array(cols)) = map.get_mut("columns") {
        for &i in &hidden {
            if let Some(Value::Object(col)) = cols.get_mut(i) {
                col.insert("redacted".into(), Value::Bool(true));
                if col.get("default").is_some_and(|d| !d.is_null()) {
                    col.insert("default".into(), Value::String(REDACTED.into()));
                }
            }
        }
    }
    if !hidden.is_empty() || tainted {
        touched = true;
        acc.columns.extend(hidden_names.iter().cloned());
    }
    if tainted {
        acc.tainted += 1;
    }

    // Rows: R1/R2 cells, then R3.
    let mut rows_withheld = false;
    if let Some(Value::Array(rows)) = map.get_mut("rows") {
        for row in rows.iter_mut() {
            if let Value::Array(cells) = row {
                for &i in &hidden {
                    if let Some(cell) = cells.get_mut(i) {
                        *cell = Value::String(REDACTED.into());
                    }
                }
                if tainted {
                    for cell in cells.iter_mut() {
                        *cell = Value::String(REDACTED.into());
                    }
                }
            }
        }
        if !scope.send_rows {
            rows.clear();
            rows_withheld = true;
            acc.rows_withheld = true;
            touched = true;
        }
    }

    // R4, statement error text.
    if has_rows
        && let Some(Value::String(msg)) = map.get_mut("error")
        && let Some(new) = withheld_text(red, msg, scope, sql_tainted)
    {
        *msg = new.to_owned();
        acc.errors += 1;
        touched = true;
    }

    if touched && has_rows {
        map.insert(
            "redaction".into(),
            json!({
                "columns": hidden_names.into_iter().collect::<Vec<_>>(),
                "all_values": tainted,
                "rows_withheld": rows_withheld,
            }),
        );
    }
}
