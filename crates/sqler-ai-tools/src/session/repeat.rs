// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Loop detection: the same call over and over.

use std::collections::HashMap;

use serde_json::Value;
use sqler_ai_api::ToolCall;

/// Counts calls by tool name and canonical arguments within one user turn.
#[derive(Debug)]
pub(crate) struct RepeatGuard {
    max: u32,
    seen: HashMap<(String, String), u32>,
}

impl RepeatGuard {
    pub(crate) fn new(max_identical_calls: u32) -> Self {
        Self {
            max: max_identical_calls,
            seen: HashMap::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.seen.clear();
    }

    /// Whether the call may run. False once the same call was admitted `max` times.
    pub(crate) fn admit(&mut self, call: &ToolCall) -> bool {
        let key = (call.name.clone(), canonical(&call.arguments));
        let count = self.seen.entry(key).or_insert(0);
        if *count >= self.max {
            return false;
        }
        *count += 1;
        true
    }
}

/// The arguments with object keys sorted, or the raw text when they do not parse.
fn canonical(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        // `serde_json` keeps maps sorted (no `preserve_order`), so re-serializing sorts keys;
        // sort explicitly anyway so the result does not depend on that feature.
        Ok(value) => sorted(value).to_string(),
        Err(_) => arguments.to_owned(),
    }
}

fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> =
                map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall::new("x", name, args)
    }

    #[test]
    fn third_identical_call_is_refused() {
        let mut g = RepeatGuard::new(2);
        assert!(g.admit(&call("t", r#"{"a":1}"#)));
        assert!(g.admit(&call("t", r#"{"a":1}"#)));
        assert!(!g.admit(&call("t", r#"{"a":1}"#)));
        assert!(g.admit(&call("t", r#"{"a":2}"#)));
        assert!(g.admit(&call("u", r#"{"a":1}"#)));
        g.reset();
        assert!(g.admit(&call("t", r#"{"a":1}"#)));
    }

    #[test]
    fn key_order_and_spacing_do_not_matter() {
        let mut g = RepeatGuard::new(1);
        assert!(g.admit(&call("t", r#"{"a":1,"b":{"x":1,"y":2}}"#)));
        assert!(!g.admit(&call("t", r#"{ "b": {"y":2, "x":1}, "a": 1 }"#)));
    }

    #[test]
    fn malformed_arguments_compare_as_raw_text() {
        let mut g = RepeatGuard::new(1);
        assert!(g.admit(&call("t", "{oops")));
        assert!(!g.admit(&call("t", "{oops")));
        assert!(g.admit(&call("t", "{oops ")));
    }
}
