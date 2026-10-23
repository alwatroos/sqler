// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The texts of the status bar items plugins contribute (ADR-0060).
//!
//! A model of one text per (plugin, item). A text is asked for once, when the plugin turns `Live`, and then replaced
//! by the events the plugin pushes. What the bar shows is capped: at most [`MAX_ITEMS`] plugin items and
//! [`MAX_CHARS`] characters each; a missing or empty text hides the item.

use std::collections::{HashMap, HashSet};

use sqler_plugin_api::{Alignment, ContributionId, PluginId};

use super::contrib::StatusReg;

/// The most plugin items the bar shows, over both sides.
pub const MAX_ITEMS: usize = 8;
/// The most characters of one item; a longer text ends in an ellipsis.
pub const MAX_CHARS: usize = 64;

/// An item of the bar as the view draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// A stable key for element ids.
    pub key: String,
    /// The text, one line, at most [`MAX_CHARS`] characters.
    pub text: String,
    /// The tooltip: the manifest tooltip if any, always followed by the plugin name, which the plugin cannot change.
    pub tooltip: String,
}

/// A character plugin text must not carry: a control character, a line or paragraph separator, or a bidirectional
/// override or isolate (it could reorder the text around it).
#[must_use]
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// One line without control characters, at most [`MAX_CHARS`] characters; empty when nothing visible is left.
#[must_use]
pub fn display_text(raw: &str) -> String {
    let flat: String = raw
        .chars()
        .map(|c| if is_unsafe_char(c) { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() <= MAX_CHARS {
        return flat.to_owned();
    }
    let mut cut: String = flat.chars().take(MAX_CHARS - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

type Key = (PluginId, ContributionId);

/// The items and their texts.
#[derive(Debug, Default)]
pub struct StatusModel {
    items: Vec<StatusReg>,
    texts: HashMap<Key, String>,
    polled: HashSet<PluginId>,
}

impl StatusModel {
    /// Takes in the current items. A text of an item that is still declared stays; every other goes. A plugin that is
    /// not `Live` is forgotten, so it is asked again when it comes back. Returns the items to ask for now (their
    /// plugin is `Live` and was not asked yet).
    pub fn sync(&mut self, items: &[StatusReg], live: &HashSet<PluginId>) -> Vec<Key> {
        self.items = items.to_vec();
        let declared: HashSet<Key> = items
            .iter()
            .map(|i| (i.plugin.clone(), i.item.clone()))
            .collect();
        self.texts.retain(|k, _| declared.contains(k));
        self.polled.retain(|p| live.contains(p));
        let mut polls = Vec::new();
        for item in items {
            if live.contains(&item.plugin) && !self.polled.contains(&item.plugin) {
                polls.push((item.plugin.clone(), item.item.clone()));
            }
        }
        self.polled
            .extend(polls.iter().map(|(plugin, _)| plugin.clone()));
        polls
    }

    /// Sets the text of an item. `false` when the item is not declared. An empty text hides the item.
    pub fn set(&mut self, plugin: &PluginId, item: &ContributionId, text: &str) -> bool {
        let key = (plugin.clone(), item.clone());
        if !self
            .items
            .iter()
            .any(|i| i.plugin == key.0 && i.item == key.1)
        {
            return false;
        }
        let shown = display_text(text);
        if shown.is_empty() {
            self.texts.remove(&key);
        } else {
            self.texts.insert(key, shown);
        }
        true
    }

    /// Takes the answer of a `status` call: a text sets it, `None` hides the item, a failure leaves it hidden.
    pub fn answered(
        &mut self,
        plugin: &PluginId,
        item: &ContributionId,
        result: &Result<Option<String>, String>,
    ) {
        match result {
            Ok(Some(text)) => {
                self.set(plugin, item, text);
            }
            Ok(None) | Err(_) => {
                self.texts.remove(&(plugin.clone(), item.clone()));
            }
        }
    }

    /// The items to draw on one side, in registration order. The cap of [`MAX_ITEMS`] counts the items that have a
    /// text, over both sides.
    #[must_use]
    pub fn shown(&self, side: Alignment) -> Vec<Shown> {
        self.items
            .iter()
            .filter_map(|i| {
                let text = self.texts.get(&(i.plugin.clone(), i.item.clone()))?;
                Some((i, text))
            })
            .take(MAX_ITEMS)
            .filter(|(i, _)| i.alignment == side)
            .map(|(i, text)| Shown {
                key: format!("{}-{}", i.plugin, i.item),
                text: text.clone(),
                tooltip: match &i.tooltip {
                    Some(tip) => format!("{tip} - {}", i.plugin_name),
                    None => i.plugin_name.clone(),
                },
            })
            .collect()
    }

    /// How many items are declared.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether no item is declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(plugin: &str, item: &str, side: Alignment, tooltip: Option<&str>) -> StatusReg {
        StatusReg {
            plugin: plugin.parse().unwrap(),
            plugin_name: format!("{plugin} name"),
            item: item.parse().unwrap(),
            tooltip: tooltip.map(str::to_owned),
            alignment: side,
        }
    }

    fn live(ids: &[&str]) -> HashSet<PluginId> {
        ids.iter().map(|i| i.parse().unwrap()).collect()
    }

    #[test]
    fn display_text_flattens_and_caps() {
        assert_eq!(display_text("  hello\nworld\t! "), "hello world !");
        assert_eq!(display_text("\u{7}\u{1b}"), "");
        let long = "x".repeat(100);
        let cut = display_text(&long);
        assert_eq!(cut.chars().count(), MAX_CHARS);
        assert!(cut.ends_with('…'));
        assert_eq!(display_text(&"y".repeat(64)).chars().count(), 64);
        assert!(!display_text(&"y".repeat(64)).ends_with('…'));
        // Multi-byte characters are cut on a character boundary.
        assert_eq!(display_text(&"é".repeat(70)).chars().count(), MAX_CHARS);
    }

    #[test]
    fn a_plugin_is_asked_once_when_it_turns_live() {
        let mut m = StatusModel::default();
        let items = [
            reg("acme-sync", "a", Alignment::Left, None),
            reg("acme-sync", "b", Alignment::Right, None),
        ];
        assert_eq!(m.sync(&items, &live(&[])).len(), 0);
        let polls = m.sync(&items, &live(&["acme-sync"]));
        assert_eq!(polls.len(), 2);
        assert!(m.sync(&items, &live(&["acme-sync"])).is_empty(), "once");
        assert_eq!(m.sync(&items, &live(&[])).len(), 0);
        assert_eq!(m.sync(&items, &live(&["acme-sync"])).len(), 2, "again");
    }

    #[test]
    fn answers_and_events_set_and_hide_texts() {
        let mut m = StatusModel::default();
        let items = [reg("acme-sync", "env", Alignment::Left, Some("Where"))];
        m.sync(&items, &live(&["acme-sync"]));
        let (p, i): (PluginId, ContributionId) =
            ("acme-sync".parse().unwrap(), "env".parse().unwrap());
        assert_eq!(m.shown(Alignment::Left).len(), 0);
        m.answered(&p, &i, &Ok(Some("prod".into())));
        let shown = m.shown(Alignment::Left);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].text, "prod");
        assert_eq!(shown[0].tooltip, "Where - acme-sync name");
        assert_eq!(m.shown(Alignment::Right).len(), 0);
        assert!(m.set(&p, &i, "dev"));
        assert_eq!(m.shown(Alignment::Left)[0].text, "dev");
        assert!(m.set(&p, &i, "   "));
        assert!(m.shown(Alignment::Left).is_empty(), "empty hides");
        m.set(&p, &i, "x");
        m.answered(&p, &i, &Ok(None));
        assert!(m.shown(Alignment::Left).is_empty(), "None hides");
        m.set(&p, &i, "x");
        m.answered(&p, &i, &Err("boom".into()));
        assert!(m.shown(Alignment::Left).is_empty(), "a failure hides");
        let other: ContributionId = "other".parse().unwrap();
        assert!(!m.set(&p, &other, "x"), "undeclared");
    }

    #[test]
    fn the_tooltip_is_just_the_plugin_name_without_a_manifest_one() {
        let mut m = StatusModel::default();
        m.sync(&[reg("acme-sync", "a", Alignment::Right, None)], &live(&[]));
        m.set(&"acme-sync".parse().unwrap(), &"a".parse().unwrap(), "t");
        assert_eq!(m.shown(Alignment::Right)[0].tooltip, "acme-sync name");
    }

    #[test]
    fn bidi_overrides_and_separators_are_dropped() {
        assert_eq!(display_text("a\u{202e}b\u{2028}c"), "a b c");
    }

    #[test]
    fn at_most_eight_items_in_order() {
        let mut m = StatusModel::default();
        let items: Vec<StatusReg> = (0..12)
            .map(|n| {
                let side = if n % 2 == 0 {
                    Alignment::Left
                } else {
                    Alignment::Right
                };
                reg("acme-sync", &format!("item{n}"), side, None)
            })
            .collect();
        m.sync(&items, &live(&[]));
        for n in 0..12 {
            m.set(
                &"acme-sync".parse().unwrap(),
                &format!("item{n}").parse().unwrap(),
                "t",
            );
        }
        let total = m.shown(Alignment::Left).len() + m.shown(Alignment::Right).len();
        assert_eq!(total, MAX_ITEMS);
        assert_eq!(m.shown(Alignment::Left)[0].key, "acme-sync-item0");
        assert_eq!(m.shown(Alignment::Right)[0].key, "acme-sync-item1");
    }

    #[test]
    fn a_removed_item_loses_its_text() {
        let mut m = StatusModel::default();
        let a = reg("acme-sync", "a", Alignment::Left, None);
        m.sync(std::slice::from_ref(&a), &live(&[]));
        m.set(&a.plugin, &a.item, "x");
        m.sync(&[], &live(&[]));
        m.sync(std::slice::from_ref(&a), &live(&[]));
        assert_eq!(m.shown(Alignment::Left).len(), 0);
    }
}
