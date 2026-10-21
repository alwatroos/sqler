// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The sections of the palette list: "Recently used" above "All commands". Headless, so the
//! order is tested without `GPUI`.

use super::PaletteItem;

/// The label of the block of recently used commands.
pub(crate) const RECENT_LABEL: &str = "Recently used";
/// The label of the block of all other commands.
pub(crate) const ALL_LABEL: &str = "All commands";

/// One line of the palette list.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PaletteLine {
    /// A section label; not selectable.
    Section(&'static str),
    /// The item at this index of the palette's items.
    Item(usize),
}

/// The lines for `items`. When the first item is a recent one the list has two sections;
/// otherwise (a typed query, or no recents) it is only items.
pub(crate) fn palette_lines(items: &[PaletteItem]) -> Vec<PaletteLine> {
    if !items.first().is_some_and(|i| i.recent) {
        return (0..items.len()).map(PaletteLine::Item).collect();
    }
    let recent = items.iter().take_while(|i| i.recent).count();
    let mut lines = vec![PaletteLine::Section(RECENT_LABEL)];
    lines.extend((0..recent).map(PaletteLine::Item));
    if recent < items.len() {
        lines.push(PaletteLine::Section(ALL_LABEL));
        lines.extend((recent..items.len()).map(PaletteLine::Item));
    }
    lines
}

#[cfg(test)]
mod tests {
    use sqler_config::{KeybindingOverrides, Platform};

    use super::*;
    use crate::command::{CommandId, CommandRegistry};
    use crate::keymap::KeyDispatcher;
    use crate::palette::{PaletteState, register_commands};
    use crate::workspace;

    fn setup() -> (CommandRegistry, KeyDispatcher) {
        let mut r = CommandRegistry::new();
        workspace::register_commands(&mut r).unwrap();
        register_commands(&mut r).unwrap();
        let k = KeyDispatcher::new(Platform::Other, Ok(KeybindingOverrides::new()), &r);
        (r, k)
    }

    fn cid(s: &str) -> CommandId {
        CommandId::new(s).unwrap()
    }

    #[test]
    fn no_recents_gives_no_sections() {
        let (r, k) = setup();
        let items = PaletteState::default().items(&r, &k);
        let lines = palette_lines(&items);
        assert_eq!(lines.len(), items.len());
        assert!(lines.iter().all(|l| matches!(l, PaletteLine::Item(_))));
    }

    #[test]
    fn three_recents_give_two_sections() {
        let (r, k) = setup();
        let mut s = PaletteState::default();
        for id in ["tab:next", "app:quit", "tab:close"] {
            s.record(&cid(id));
        }
        let items = s.items(&r, &k);
        let lines = palette_lines(&items);
        assert_eq!(lines[0], PaletteLine::Section(RECENT_LABEL));
        assert_eq!(
            lines[1..4],
            [
                PaletteLine::Item(0),
                PaletteLine::Item(1),
                PaletteLine::Item(2)
            ]
        );
        assert_eq!(lines[4], PaletteLine::Section(ALL_LABEL));
        assert_eq!(lines[5], PaletteLine::Item(3));
        assert_eq!(lines.len(), items.len() + 2);
    }

    #[test]
    fn only_recents_has_no_all_section() {
        let item = |recent| PaletteItem {
            id: cid("tab:next"),
            title: String::new(),
            shortcut: None,
            field: crate::command::MatchField::Title,
            positions: Vec::new(),
            recent,
        };
        let items = [item(true), item(true)];
        assert_eq!(
            palette_lines(&items),
            [
                PaletteLine::Section(RECENT_LABEL),
                PaletteLine::Item(0),
                PaletteLine::Item(1)
            ]
        );
    }

    #[test]
    fn twelve_recorded_give_ten_recent() {
        let (r, k) = setup();
        let mut s = PaletteState::default();
        // Twelve distinct, runnable commands.
        let ids: Vec<String> = r
            .query("")
            .iter()
            .map(|m| m.command.id().to_string())
            .filter(|i| !crate::palette::NEEDS_ARGUMENT.contains(&i.as_str()))
            .take(12)
            .collect();
        assert_eq!(ids.len(), 12);
        for id in &ids {
            s.record(&cid(id));
        }
        let items = s.items(&r, &k);
        assert_eq!(items.iter().filter(|i| i.recent).count(), 10);
        let lines = palette_lines(&items);
        let recent_lines = lines
            .iter()
            .skip(1)
            .take_while(|l| matches!(l, PaletteLine::Item(_)))
            .count();
        assert_eq!(recent_lines, 10);
    }

    #[test]
    fn a_typed_query_gives_no_sections_in_matcher_order() {
        let (r, k) = setup();
        let mut s = PaletteState::default();
        s.record(&cid("tab:next"));
        s.set_query("tab");
        let items = s.items(&r, &k);
        let lines = palette_lines(&items);
        assert!(lines.iter().all(|l| matches!(l, PaletteLine::Item(_))));
        let expected: Vec<String> = r
            .query("tab")
            .iter()
            .map(|m| m.command.id().to_string())
            .filter(|i| !crate::palette::NEEDS_ARGUMENT.contains(&i.as_str()))
            .collect();
        let got: Vec<String> = items.iter().map(|i| i.id.to_string()).collect();
        assert_eq!(got, expected);
    }
}
