// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin completion items next to the built-in popup, as plain data.
//!
//! The built-in popup never waits for a plugin. The GPUI side asks the plugins on the side, keeps
//! their answers in [`PluginAnswers`], and the next popup at the same place appends
//! [`merge_plugin_items`] after the built-in rows.

use std::collections::HashSet;

use sqler_plugin_api::{CompletionKind, CompletionRequest, MAX_COMPLETION_STATEMENT_BYTES};
use sqler_sql::{Dialect, split, statement_at};

use super::COMPLETION_LIMIT;
use super::popup::PopupItem;
use crate::plugins::backend::PluginCompletions;

/// The most plugin rows one popup shows, all plugins together.
pub const PLUGIN_POPUP_LIMIT: usize = 100;

/// The request for the statement under `offset`, or `None` when that statement is over the cap.
///
/// The statement is the one [`statement_at`] finds, stretched to include the cursor; with no
/// statement it is empty. The cursor is relative to the statement start. Nothing but the
/// statement text and the engine id goes out: no connection, no secrets, no rows.
#[must_use]
pub fn plugin_request(
    text: &str,
    offset: usize,
    dialect: Dialect,
    driver: Option<&str>,
) -> Option<CompletionRequest> {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let segments = split(dialect, text);
    let range = match statement_at(&segments, offset) {
        Some(seg) => {
            let end = seg.terminator.as_ref().map_or(seg.body.end, |t| t.end);
            seg.body.start.min(offset)..end.max(offset).min(text.len())
        }
        None => offset..offset,
    };
    if range.len() > MAX_COMPLETION_STATEMENT_BYTES {
        return None;
    }
    Some(CompletionRequest {
        driver: driver.map(str::to_owned),
        statement: text.get(range.clone())?.to_owned(),
        cursor: offset - range.start,
    })
}

/// One plugin row of the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPopupItem {
    /// The text shown and filtered on.
    pub label: String,
    /// What it is.
    pub kind: CompletionKind,
    /// The text inserted in place of the typed word.
    pub insert_text: String,
    /// The plugin's name, then its own note when it has one.
    pub detail: String,
    /// Sorts after every built-in row.
    pub sort_text: String,
}

/// The plugin rows to append after `builtin`.
///
/// Plugins go in the order of their ids and each keeps its own item order. An item whose label and
/// insert text equal a built-in one, or an earlier plugin one, is dropped. At most
/// [`PLUGIN_POPUP_LIMIT`] rows come out. Every row names its plugin in `detail`, so it cannot pass
/// as a built-in one.
#[must_use]
pub fn merge_plugin_items(
    builtin: &[PopupItem],
    answers: &[PluginCompletions],
) -> Vec<PluginPopupItem> {
    let mut seen: HashSet<(&str, &str)> = builtin
        .iter()
        .map(|b| (b.label.as_str(), b.insert_text.as_str()))
        .collect();
    let mut order: Vec<&PluginCompletions> = answers.iter().collect();
    order.sort_by(|a, b| a.plugin.cmp(&b.plugin));
    let mut out = Vec::new();
    for answer in order {
        for item in &answer.items {
            if out.len() >= PLUGIN_POPUP_LIMIT {
                return out;
            }
            if !seen.insert((item.label.as_str(), item.insert_text.as_str())) {
                continue;
            }
            let detail = match item.detail.as_deref() {
                Some(d) if !d.is_empty() => format!("{} · {d}", answer.source),
                _ => answer.source.clone(),
            };
            out.push(PluginPopupItem {
                label: item.label.clone(),
                kind: item.kind,
                insert_text: item.insert_text.clone(),
                detail,
                sort_text: format!("{:05}", COMPLETION_LIMIT + out.len()),
            });
        }
    }
    out
}

/// The answers to one ask. An answer to an older ask is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAnswers {
    ask: u64,
    answers: Vec<PluginCompletions>,
}

impl PluginAnswers {
    /// An empty set for ask number `ask`.
    #[must_use]
    pub fn new(ask: u64) -> Self {
        Self {
            ask,
            answers: Vec::new(),
        }
    }

    /// The ask number these answers belong to.
    #[must_use]
    pub fn ask(&self) -> u64 {
        self.ask
    }

    /// Keeps `answer` when it belongs to the current ask (a plugin's later answer replaces its
    /// earlier one). Whether it was kept.
    pub fn accept(&mut self, ask: u64, answer: PluginCompletions) -> bool {
        if ask != self.ask {
            return false;
        }
        match self.answers.iter_mut().find(|a| a.plugin == answer.plugin) {
            Some(slot) => *slot = answer,
            None => self.answers.push(answer),
        }
        true
    }

    /// The answers kept so far.
    #[must_use]
    pub fn answers(&self) -> &[PluginCompletions] {
        &self.answers
    }
}

/// Builds a plugin id for tests of this module and the provider.
#[cfg(test)]
pub(crate) fn test_id(s: &str) -> sqler_plugin_api::PluginId {
    s.parse().expect("a valid plugin id")
}

#[cfg(test)]
mod tests {
    use sqler_completion::ItemKind;
    use sqler_plugin_api::CompletionItem;

    use super::*;

    fn builtin(label: &str, i: usize) -> PopupItem {
        PopupItem {
            label: label.to_owned(),
            kind: ItemKind::Keyword,
            insert_text: label.to_owned(),
            detail: None,
            sort_text: format!("{i:05}"),
        }
    }

    fn item(label: &str, detail: Option<&str>) -> CompletionItem {
        CompletionItem {
            label: label.to_owned(),
            kind: CompletionKind::Table,
            insert_text: label.to_owned(),
            detail: detail.map(str::to_owned),
        }
    }

    fn answer(id: &str, source: &str, items: Vec<CompletionItem>) -> PluginCompletions {
        PluginCompletions {
            plugin: test_id(id),
            source: source.to_owned(),
            items,
        }
    }

    #[test]
    fn the_request_is_the_statement_under_the_cursor() {
        let text = "SELECT 1; SELECT * FROM us; SELECT 3";
        let offset = text.find("us;").expect("us") + 2;
        let r = plugin_request(text, offset, Dialect::Postgres, Some("postgres")).expect("request");
        assert_eq!(r.statement, "SELECT * FROM us;");
        assert_eq!(&r.statement[..r.cursor], "SELECT * FROM us");
        assert_eq!(r.driver.as_deref(), Some("postgres"));
    }

    #[test]
    fn an_empty_buffer_asks_with_an_empty_statement() {
        let r = plugin_request("", 0, Dialect::Postgres, None).expect("request");
        assert_eq!((r.statement.as_str(), r.cursor, r.driver), ("", 0, None));
    }

    #[test]
    fn a_cursor_past_the_end_or_inside_a_character_is_clamped() {
        let text = "SELECT 'é'";
        let r = plugin_request(text, 99, Dialect::Postgres, None).expect("request");
        assert_eq!((r.statement.as_str(), r.cursor), (text, text.len()));
        let inside = text.find('é').expect("é") + 1;
        let r = plugin_request(text, inside, Dialect::Postgres, None).expect("request");
        assert!(text.is_char_boundary(r.cursor));
        assert_eq!(&r.statement[..r.cursor], "SELECT '");
    }

    #[test]
    fn a_multibyte_statement_keeps_a_byte_cursor() {
        let text = "SELECT 1; SELECT ü_ FROM t";
        let offset = text.find("_ ").expect("_");
        let r = plugin_request(text, offset, Dialect::Postgres, None).expect("request");
        assert_eq!(r.statement, "SELECT ü_ FROM t");
        assert_eq!(&r.statement[..r.cursor], "SELECT ü");
    }

    #[test]
    fn a_statement_over_the_cap_is_not_sent() {
        let text = format!("SELECT '{}'", "x".repeat(MAX_COMPLETION_STATEMENT_BYTES));
        assert!(plugin_request(&text, 5, Dialect::Postgres, None).is_none());
    }

    #[test]
    fn plugin_rows_follow_the_builtin_ones_and_name_their_source() {
        let built = [builtin("SELECT", 0), builtin("FROM", 1)];
        let rows = merge_plugin_items(
            &built,
            &[answer(
                "alpha",
                "Alpha",
                vec![item("users", Some("from alpha")), item("orders", None)],
            )],
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].detail, "Alpha · from alpha");
        assert_eq!(rows[1].detail, "Alpha");
        assert_eq!(rows[0].sort_text, format!("{COMPLETION_LIMIT:05}"));
        assert!(rows.iter().all(|r| r.sort_text > built[1].sort_text));
        assert!(rows[0].sort_text < rows[1].sort_text);
    }

    #[test]
    fn plugins_merge_in_id_order_with_their_own_order_kept() {
        let rows = merge_plugin_items(
            &[],
            &[
                answer("zeta", "Z", vec![item("z1", None), item("z2", None)]),
                answer("alpha", "A", vec![item("a2", None), item("a1", None)]),
            ],
        );
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["a2", "a1", "z1", "z2"]);
    }

    #[test]
    fn a_duplicate_of_a_builtin_or_an_earlier_plugin_item_is_dropped() {
        let rows = merge_plugin_items(
            &[builtin("users", 0)],
            &[
                answer("alpha", "A", vec![item("users", None), item("t", None)]),
                answer("beta", "B", vec![item("t", None), item("u", None)]),
            ],
        );
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["t", "u"]);
    }

    #[test]
    fn the_plugin_rows_are_capped() {
        let many: Vec<_> = (0..PLUGIN_POPUP_LIMIT * 2)
            .map(|i| item(&format!("i{i}"), None))
            .collect();
        let rows = merge_plugin_items(&[], &[answer("alpha", "A", many)]);
        assert_eq!(rows.len(), PLUGIN_POPUP_LIMIT);
    }

    #[test]
    fn a_stale_ask_is_refused() {
        let mut a = PluginAnswers::new(2);
        assert!(!a.accept(1, answer("alpha", "A", vec![item("x", None)])));
        assert_eq!(a.answers(), []);
        assert!(a.accept(2, answer("alpha", "A", vec![item("x", None)])));
        assert!(a.accept(2, answer("alpha", "A", vec![item("y", None)])));
        assert_eq!(a.answers().len(), 1);
        assert_eq!(a.answers()[0].items[0].label, "y");
        assert_eq!(a.ask(), 2);
    }
}
