// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Schema grounding: pick the tables a request is about and describe them under a byte budget.
//!
//! Every byte that reaches the prompt is the enveloped, redacted, audited content of a tool
//! result; this module writes no database text of its own.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use sqler_ai_api::ToolCall;
use sqler_driver_api::sqler_core::ObjectKind;

use super::{AssistError, GroundedTable, Grounding};
use crate::schema::names;
use crate::source::ConnectionInfo;
use crate::toolset::ToolSet;
use crate::untrusted::unwrap_untrusted;

/// The most terms searched.
const MAX_TERMS: usize = 8;
/// The hits asked from one `search_schema` call.
const SEARCH_LIMIT: u32 = 20;

/// Words that never name a table or column: English fillers and SQL keywords.
const STOP: &[&str] = &[
    "the", "and", "for", "are", "was", "were", "with", "from", "that", "this", "these", "those",
    "have", "has", "had", "all", "any", "each", "every", "per", "whom", "whose", "which", "what",
    "when", "where", "why", "how", "show", "list", "give", "find", "get", "tell", "please", "can",
    "could", "would", "should", "not", "but", "its", "their", "there", "than", "then", "into",
    "onto", "over", "under", "about", "after", "before", "between", "more", "most", "less",
    "least", "some", "only", "also", "just", "ever", "our", "your", "you", "me", "who", "select",
    "insert", "update", "delete", "where", "group", "order", "having", "join", "left", "right",
    "inner", "outer", "full", "cross", "union", "limit", "offset", "distinct", "null", "like",
    "case", "else", "end", "set", "values", "create", "alter", "drop", "table",
];

/// The search terms of `text`: words of three or more characters, lowercased, without
/// stop words, each with a singular form, deduplicated, at most eight.
pub(crate) fn terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        let word = word.to_ascii_lowercase();
        if word.len() < 3 || STOP.contains(&word.as_str()) {
            continue;
        }
        let singular =
            (word.len() > 4 && word.ends_with('s')).then(|| word[..word.len() - 1].to_owned());
        for candidate in std::iter::once(word).chain(singular) {
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out.truncate(MAX_TERMS);
    out
}

pub(crate) type Key = (Option<String>, Option<String>, String);

pub(crate) fn call(connection: &str, name: &str, args: &Value) -> ToolCall {
    let mut args = args.clone();
    if let Some(map) = args.as_object_mut() {
        map.insert("connection".to_owned(), json!(connection));
    }
    ToolCall::new(format!("assist-{name}"), name, args.to_string())
}

/// The JSON inside a tool result, or `None` for an error result or a malformed one.
pub(crate) fn body_of(result: &sqler_ai_api::ToolResult) -> Option<Value> {
    if result.is_error {
        return None;
    }
    serde_json::from_str(unwrap_untrusted(&result.content)?).ok()
}

fn text_of(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

pub(crate) async fn score(
    tools: &ToolSet,
    connection: &str,
    terms: &[String],
) -> Result<BTreeMap<Key, u32>, AssistError> {
    let mut scores: BTreeMap<Key, u32> = BTreeMap::new();
    for term in terms {
        let result = tools
            .call(&call(
                connection,
                names::SEARCH_SCHEMA,
                &json!({"query": term, "limit": SEARCH_LIMIT}),
            ))
            .await;
        let Some(body) = body_of(&result) else {
            continue;
        };
        for hit in body["hits"].as_array().map_or(&[][..], Vec::as_slice) {
            let kind = hit["kind"].as_str().unwrap_or_default();
            let (name, points) = if kind == "column" {
                (text_of(&hit["table"]), 2)
            } else if ObjectKind::from_id(kind).is_some_and(|k| {
                matches!(
                    k,
                    ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
                )
            }) {
                (text_of(&hit["name"]), 3)
            } else {
                continue;
            };
            let Some(name) = name else { continue };
            let bonus = if hit["quality"] == "exact" { 2 } else { 0 };
            let key = (text_of(&hit["database"]), text_of(&hit["schema"]), name);
            *scores.entry(key).or_insert(0) += points + bonus;
        }
    }
    Ok(scores)
}

/// Grounds `text` on `connection`: ranks tables by `search_schema` hits for its terms and
/// describes the top `max_tables` while the total stays within `budget` bytes.
pub(crate) async fn ground(
    tools: &ToolSet,
    info: &ConnectionInfo,
    text: &str,
    max_tables: usize,
    budget: usize,
) -> Result<Grounding, AssistError> {
    let terms = terms(text);
    let mut scores = score(tools, &info.id, &terms).await?;
    if scores.is_empty() && !terms.is_empty() {
        // A cold catalog finds nothing: listing loads it. The listing itself is dropped.
        let _ = tools
            .call(&call(&info.id, names::LIST_TABLES, &json!({})))
            .await;
        scores = score(tools, &info.id, &terms).await?;
    }
    let mut ranked: Vec<(Key, u32)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.2.cmp(&b.0.2)));
    let mut grounding = Grounding::empty(terms);
    for ((database, schema, name), points) in ranked.into_iter().take(max_tables) {
        let result = tools
            .call(&call(
                &info.id,
                names::DESCRIBE_TABLE,
                &json!({"database": database, "schema": schema, "name": name}),
            ))
            .await;
        if result.is_error {
            continue;
        }
        let extra = result.content.len() + usize::from(!grounding.text.is_empty());
        if grounding.bytes + extra > budget {
            grounding.truncated = true;
            continue;
        }
        if !grounding.text.is_empty() {
            grounding.text.push('\n');
        }
        grounding.text.push_str(&result.content);
        grounding.bytes += extra;
        grounding
            .tables
            .push(GroundedTable::new(database, schema, name, points));
    }
    Ok(grounding)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_drop_stop_words_and_short_words() {
        assert_eq!(
            terms("Show me the total of all orders by customer"),
            ["total", "orders", "order", "customer"]
        );
        assert_eq!(terms("a b is of"), Vec::<String>::new());
    }

    #[test]
    fn terms_keep_underscores_and_dedupe() {
        assert_eq!(terms("user_id, USER_ID"), ["user_id"]);
        assert_eq!(terms("orders ORDERS"), ["orders", "order"]);
    }

    #[test]
    fn terms_are_capped_at_eight() {
        let text = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        assert_eq!(terms(text).len(), MAX_TERMS);
    }

    #[test]
    fn short_plurals_are_not_cut() {
        assert_eq!(terms("bus"), ["bus"]);
        assert_eq!(terms("dogs"), ["dogs"]);
    }
}
