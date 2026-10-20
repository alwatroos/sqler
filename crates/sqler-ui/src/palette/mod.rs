// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The command palette: state (always built) and the `GPUI` overlay (feature `gui`).
//!
//! The palette lists the commands of the [`CommandRegistry`], filtered and ranked by its matcher,
//! with each command's current shortcut. Key handling is in [`crate::keymap`].

#[cfg(feature = "gui")]
pub(crate) mod view;

use crate::command::{Command, CommandError, CommandId, CommandRegistry, MatchField};
use crate::keymap::KeyDispatcher;

/// The ID of the command that opens the palette (pinned by ADR-0005).
pub const OPEN_PALETTE: &str = "workspace:open-command-palette";

/// Commands that need an argument and so cannot run from the palette.
const NEEDS_ARGUMENT: &[&str] = &[
    "connection:select",
    "tab:activate",
    "settings:set-theme",
    "settings:set-ui-font-size",
];

/// Registers the palette's own commands.
///
/// # Errors
///
/// Returns [`CommandError::Duplicate`] if `OPEN_PALETTE` is already registered.
pub fn register_commands(r: &mut CommandRegistry) -> Result<(), CommandError> {
    r.register(Command::new(OPEN_PALETTE, "Show Command Palette")?)
}

/// One row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    /// The command.
    pub id: CommandId,
    /// The command title.
    pub title: String,
    /// The command's shortcut, if it has one.
    pub shortcut: Option<String>,
    /// The text the query matched.
    pub field: MatchField,
    /// Matched char indices into the title or ID named by `field`.
    pub positions: Vec<usize>,
}

/// The palette state: open or closed, the query and the selected row.
#[derive(Debug, Default)]
pub struct PaletteState {
    open: bool,
    query: String,
    selected: usize,
}

impl PaletteState {
    /// Whether the palette is showing.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the palette with an empty query.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
    }

    /// Closes the palette.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Opens a closed palette and closes an open one.
    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// The current query.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Sets the query and selects the first row.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
    }

    /// The selected row index.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The rows for the current query, best match first.
    #[must_use]
    pub fn items(&self, registry: &CommandRegistry, keys: &KeyDispatcher) -> Vec<PaletteItem> {
        registry
            .query(&self.query)
            .into_iter()
            .filter(|m| !NEEDS_ARGUMENT.contains(&m.command.id().as_str()))
            .map(|m| PaletteItem {
                id: m.command.id().clone(),
                title: m.command.title().to_owned(),
                shortcut: keys.shortcut_label(m.command.id()),
                field: m.field,
                positions: m.positions,
            })
            .collect()
    }

    /// Moves the selection by `delta` rows, wrapping around a list of `len` rows.
    pub fn move_selection(&mut self, delta: isize, len: usize) {
        let Ok(n) = isize::try_from(len) else {
            return;
        };
        if n == 0 {
            self.selected = 0;
            return;
        }
        let current = isize::try_from(self.selected).unwrap_or(0);
        self.selected = usize::try_from((current + delta).rem_euclid(n)).unwrap_or(0);
    }

    /// Closes the palette and returns the selected command, or `None` when there are no rows.
    pub fn confirm(
        &mut self,
        registry: &CommandRegistry,
        keys: &KeyDispatcher,
    ) -> Option<CommandId> {
        let id = self
            .items(registry, keys)
            .into_iter()
            .nth(self.selected)
            .map(|item| item.id);
        self.close();
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace;
    use sqler_config::{KeybindingOverrides, Platform};

    fn setup() -> (CommandRegistry, KeyDispatcher) {
        let mut r = CommandRegistry::new();
        workspace::register_commands(&mut r).unwrap();
        register_commands(&mut r).unwrap();
        let k = KeyDispatcher::new(Platform::Other, Ok(KeybindingOverrides::new()), &r);
        (r, k)
    }

    fn ids(s: &PaletteState, r: &CommandRegistry, k: &KeyDispatcher) -> Vec<String> {
        s.items(r, k).iter().map(|i| i.id.to_string()).collect()
    }

    #[test]
    fn open_close_toggle() {
        let mut s = PaletteState::default();
        assert!(!s.is_open());
        s.open();
        s.set_query("abc");
        s.move_selection(1, 3);
        assert!(s.is_open());
        s.open();
        assert_eq!((s.query(), s.selected()), ("", 0));
        s.toggle();
        assert!(!s.is_open());
        s.toggle();
        assert!(s.is_open());
        s.close();
        assert!(!s.is_open());
    }

    #[test]
    fn empty_query_lists_commands_without_argument_ones() {
        let (r, k) = setup();
        let s = PaletteState::default();
        let list = ids(&s, &r, &k);
        assert_eq!(
            list,
            [
                "app:quit",
                "connection:open",
                "settings:open",
                "tab:close",
                "tab:next",
                "tab:previous",
                "theme:toggle-mode",
                "workspace:open-command-palette"
            ]
        );
    }

    #[test]
    fn items_carry_shortcuts() {
        let (r, k) = setup();
        let s = PaletteState::default();
        let items = s.items(&r, &k);
        let open = items
            .iter()
            .find(|i| i.id.as_str() == OPEN_PALETTE)
            .unwrap();
        assert_eq!(open.title, "Show Command Palette");
        assert_eq!(open.shortcut.as_deref(), Some("ctrl+shift+p"));
        let conn = items
            .iter()
            .find(|i| i.id.as_str() == "connection:open")
            .unwrap();
        assert_eq!(conn.shortcut, None);
    }

    #[test]
    fn query_filters_and_ranks_with_the_matcher() {
        let (r, k) = setup();
        let mut s = PaletteState::default();
        s.set_query("tab");
        let list = ids(&s, &r, &k);
        assert!(list.iter().all(|i| i != "app:quit"));
        assert!(list.contains(&"tab:close".to_owned()));
        let expected: Vec<String> = r
            .query("tab")
            .iter()
            .map(|m| m.command.id().to_string())
            .filter(|i| i != "tab:activate")
            .collect();
        assert_eq!(list, expected);
        s.set_query("zzzz");
        assert_eq!(s.items(&r, &k).len(), 0);
    }

    #[test]
    fn selection_wraps() {
        let mut s = PaletteState::default();
        s.move_selection(-1, 3);
        assert_eq!(s.selected(), 2);
        s.move_selection(1, 3);
        assert_eq!(s.selected(), 0);
        s.move_selection(1, 3);
        s.move_selection(1, 3);
        s.move_selection(1, 3);
        assert_eq!(s.selected(), 0);
        s.move_selection(1, 3);
        s.move_selection(1, 0);
        assert_eq!(s.selected(), 0);
    }

    #[test]
    fn set_query_resets_selection() {
        let mut s = PaletteState::default();
        s.move_selection(1, 3);
        s.set_query("x");
        assert_eq!(s.selected(), 0);
    }

    #[test]
    fn confirm_returns_selected_and_closes() {
        let (r, k) = setup();
        let mut s = PaletteState::default();
        s.open();
        s.move_selection(1, 6);
        let id = s.confirm(&r, &k).unwrap();
        assert_eq!(id.as_str(), "connection:open");
        assert!(!s.is_open());
    }

    #[test]
    fn confirm_without_items_gives_none_and_closes() {
        let (r, k) = setup();
        let mut s = PaletteState::default();
        s.open();
        s.set_query("zzzz");
        assert_eq!(s.confirm(&r, &k), None);
        assert!(!s.is_open());
    }

    #[test]
    fn registered_id_is_valid() {
        let (r, _) = setup();
        assert!(r.get(&CommandId::new(OPEN_PALETTE).unwrap()).is_some());
    }
}
