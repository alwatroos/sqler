// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The size caps of a stored entry, then the scrub.
//!
//! Text is scrubbed, cut and scrubbed again: the first pass sees a secret that straddles the cut,
//! the second removes any fragment the cut leaves. The heuristics never run over unbounded text.

use serde_json::{Value, json};

use crate::chat_history::scrub::{scrub_text, scrub_value};
use crate::chat_history::types::{ChatHistoryOptions, StoredEntry, StoredNotice, StoredStatus};

const CUT_SUFFIX: &str = " [cut]";

/// Cuts `text` to at most `max` bytes on a char boundary, with a suffix when it was cut.
pub(crate) fn cut_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(CUT_SUFFIX.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{CUT_SUFFIX}", &text[..end])
}

/// Scrubs before the cut, so a secret straddling the cut is seen whole, and after it, so no
/// fragment the cut leaves behind survives. The first pass runs over at most twice the cap.
fn fit_text(text: &str, o: &ChatHistoryOptions) -> String {
    let max = o.max_text_bytes;
    scrub_text(&cut_text(
        &scrub_text(&cut_text(text, max.saturating_mul(2))),
        max,
    ))
}

/// Cuts every string of `value`, scrubs it and limits every `rows` array.
fn fit_value(value: &mut Value, o: &ChatHistoryOptions) {
    cut_strings(value, o.max_text_bytes.saturating_mul(2));
    scrub_value(value);
    cut_strings(value, o.max_text_bytes);
    scrub_value(value);
    cut_rows(value, o.max_rows);
}

fn cut_strings(value: &mut Value, max: usize) {
    match value {
        Value::String(s) if s.len() > max => *s = cut_text(s, max),
        Value::Array(items) => items.iter_mut().for_each(|v| cut_strings(v, max)),
        Value::Object(map) => map.values_mut().for_each(|v| cut_strings(v, max)),
        _ => {}
    }
}

fn cut_rows(value: &mut Value, max_rows: usize) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(|v| cut_rows(v, max_rows)),
        Value::Object(map) => {
            let mut kept = None;
            if let Some(Value::Array(rows)) = map.get_mut("rows")
                && rows.len() > max_rows
            {
                rows.truncate(max_rows);
                kept = Some(max_rows);
            }
            for v in map.values_mut() {
                cut_rows(v, max_rows);
            }
            if let Some(n) = kept {
                map.insert("rows_kept_in_history".to_owned(), json!(n));
            }
        }
        _ => {}
    }
}

/// Applies the text cut, the scrub, the row cap and the entry cap.
pub(crate) fn fit_entry(entry: StoredEntry, o: &ChatHistoryOptions) -> StoredEntry {
    match entry {
        StoredEntry::User { text } => StoredEntry::User {
            text: fit_text(&text, o),
        },
        StoredEntry::Assistant { text } => StoredEntry::Assistant {
            text: fit_text(&text, o),
        },
        StoredEntry::ToolCall {
            call_id,
            tool,
            mut arguments,
            status,
            mut result,
        } => {
            fit_value(&mut arguments, o);
            fit_value(&mut result, o);
            let status = match status {
                StoredStatus::Error { code, message } => StoredStatus::Error {
                    code: fit_text(&code, o),
                    message: fit_text(&message, o),
                },
                other => other,
            };
            let mut entry = StoredEntry::ToolCall {
                call_id: fit_text(&call_id, o),
                tool: fit_text(&tool, o),
                arguments,
                status,
                result,
            };
            if serialised_len(&entry) > o.max_entry_bytes
                && let StoredEntry::ToolCall {
                    arguments, result, ..
                } = &mut entry
            {
                *result = json!({"history_note": "result too large to keep"});
                *arguments = Value::Null;
            }
            entry
        }
        StoredEntry::Sql {
            call_id,
            connection,
            display_sql,
            class,
            approved,
            rows,
            truncated,
            failed,
        } => StoredEntry::Sql {
            call_id: fit_text(&call_id, o),
            connection: cut_text(&connection, 256),
            display_sql: fit_text(&display_sql, o),
            class: cut_text(&class, 32),
            approved,
            rows,
            truncated: truncated.map(|t| cut_text(&t, 32)),
            failed,
        },
        StoredEntry::Notice { notice } => StoredEntry::Notice {
            notice: fit_notice(notice, o),
        },
    }
}

fn fit_notice(notice: StoredNotice, o: &ChatHistoryOptions) -> StoredNotice {
    match notice {
        StoredNotice::LoopDetected { tool } => StoredNotice::LoopDetected {
            tool: fit_text(&tool, o),
        },
        StoredNotice::ProviderError { message } => StoredNotice::ProviderError {
            message: fit_text(&message, o),
        },
        StoredNotice::LimitReached { limit } => StoredNotice::LimitReached {
            limit: cut_text(&limit, 32),
        },
        other => other,
    }
}

fn serialised_len(entry: &StoredEntry) -> usize {
    serde_json::to_string(entry).map_or(usize::MAX, |s| s.len())
}

/// A map-free helper for tests: the number of rows in the first `rows` array found.
#[cfg(test)]
fn rows_len(value: &Value) -> Option<usize> {
    match value {
        Value::Object(map) => map
            .get("rows")
            .and_then(Value::as_array)
            .map(Vec::len)
            .or_else(|| map.values().find_map(rows_len)),
        Value::Array(items) => items.iter().find_map(rows_len),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]

    use serde_json::Map;

    use super::*;

    fn tool(arguments: Value, result: Value) -> StoredEntry {
        StoredEntry::ToolCall {
            call_id: "c1".into(),
            tool: "run_query".into(),
            arguments,
            status: StoredStatus::Ok,
            result,
        }
    }

    #[test]
    fn a_secret_straddling_the_cut_leaves_no_fragment() {
        let o = ChatHistoryOptions::default();
        let max = o.max_text_bytes;
        for secret in [
            "postgres://u:SECRETPW@h/db",
            "sk-SECRETKEYabcdefghijklmnopqrst",
        ] {
            for back in 0..secret.len() {
                let head = "a ".repeat((max - 6) / 2);
                let mut text = head.clone();
                // Put the cut (at max - 6) `back` bytes into the secret.
                text.truncate(max - 7 - back);
                text.push(' ');
                text.push_str(secret);
                text.push_str(&" b".repeat(100));
                let fitted = fit_text(&text, &o);
                assert!(
                    !fitted.contains("SECRET"),
                    "{secret} at {back}: {}",
                    &fitted[fitted.len().saturating_sub(80)..]
                );
                let mut v = json!({ "k": text });
                fit_value(&mut v, &o);
                assert!(
                    !v.to_string().contains("SECRET"),
                    "{secret} at {back} (value)"
                );
            }
        }
    }

    #[test]
    fn cut_text_keeps_char_boundaries() {
        let text = "é".repeat(100);
        let cut = cut_text(&text, 51);
        assert!(cut.len() <= 51, "{}", cut.len());
        assert!(cut.ends_with(CUT_SUFFIX));
        assert_eq!(cut_text("short", 10), "short");
    }

    #[test]
    fn rows_are_limited_and_marked() {
        let rows: Vec<Value> = (0..80).map(|i| json!([i, "<redacted>"])).collect();
        let o = ChatHistoryOptions::default();
        let StoredEntry::ToolCall { result, .. } = fit_entry(
            tool(Value::Null, json!({"columns": ["a", "b"], "rows": rows})),
            &o,
        ) else {
            panic!()
        };
        assert_eq!(rows_len(&result), Some(o.max_rows));
        assert_eq!(result["rows_kept_in_history"], o.max_rows);
        assert_eq!(result["rows"][0][1], "<redacted>");
    }

    #[test]
    fn short_results_are_not_marked() {
        let o = ChatHistoryOptions::default();
        let StoredEntry::ToolCall { result, .. } =
            fit_entry(tool(Value::Null, json!({"rows": [[1]]})), &o)
        else {
            panic!()
        };
        assert!(result.get("rows_kept_in_history").is_none());
    }

    #[test]
    fn an_oversized_entry_loses_its_result_and_arguments() {
        let mut o = ChatHistoryOptions::default();
        o.max_entry_bytes = 300;
        let big: Map<String, Value> = (0..40)
            .map(|i| (format!("k{i}"), json!("x".repeat(20))))
            .collect();
        let StoredEntry::ToolCall {
            arguments, result, ..
        } = fit_entry(tool(json!({"sql": "select 1"}), Value::Object(big)), &o)
        else {
            panic!()
        };
        assert_eq!(arguments, Value::Null);
        assert_eq!(result, json!({"history_note": "result too large to keep"}));
    }

    #[test]
    fn texts_are_cut_then_scrubbed() {
        let mut o = ChatHistoryOptions::default();
        o.max_text_bytes = 40;
        let StoredEntry::User { text } = fit_entry(
            StoredEntry::User {
                text: format!("token=CANARY {}", "a".repeat(100)),
            },
            &o,
        ) else {
            panic!()
        };
        assert!(text.len() <= 60, "{text}");
        assert!(!text.contains("CANARY"));
    }
}
