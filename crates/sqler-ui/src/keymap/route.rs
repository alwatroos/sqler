// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Key routing: what a chord does, given the palette state and the keymap.

use sqler_config::{KeyChord, Modifiers};

use super::dispatcher::KeyDispatcher;
use crate::command::{CommandId, CommandRegistry};
use crate::palette::{OPEN_PALETTE, PaletteState};
use crate::workspace::{Workspace, WorkspaceAction};

/// What the view has to do with a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Not ours: let the focused element handle it.
    Ignored,
    /// Consumed; the palette state may have changed.
    Handled,
    /// Run this command.
    Run(CommandId),
}

/// Routes a chord. While the palette is open only the palette keys (and the shortcut that opens
/// it) apply; everything else goes to the text input.
pub fn route_key(
    chord: &KeyChord,
    palette: &mut PaletteState,
    keys: &KeyDispatcher,
    registry: &CommandRegistry,
) -> KeyOutcome {
    let resolved = keys.resolve(chord);
    let opens_palette = resolved.is_some_and(|id| id.as_str() == OPEN_PALETTE);
    if palette.is_open() {
        let plain = chord.modifiers() == Modifiers::default();
        return match chord.key() {
            "escape" if plain => {
                palette.close();
                KeyOutcome::Handled
            }
            "enter" if plain => palette
                .confirm(registry, keys)
                .map_or(KeyOutcome::Handled, KeyOutcome::Run),
            "up" | "down" if plain => {
                let len = palette.items(registry, keys).len();
                palette.move_selection(if chord.key() == "up" { -1 } else { 1 }, len);
                KeyOutcome::Handled
            }
            _ if opens_palette => {
                palette.close();
                KeyOutcome::Handled
            }
            _ => KeyOutcome::Ignored,
        };
    }
    match resolved {
        Some(_) if opens_palette => {
            palette.open();
            KeyOutcome::Handled
        }
        Some(id) => KeyOutcome::Run(id.clone()),
        None => KeyOutcome::Ignored,
    }
}

/// The workspace action a command stands for, or `None` when it is not a workspace action or
/// cannot run now (`connection:open` with no selected entry).
#[must_use]
pub fn action_for(id: &CommandId, ws: &Workspace) -> Option<WorkspaceAction> {
    match id.as_str() {
        "tab:close" => Some(WorkspaceAction::CloseActiveTab),
        "tab:next" => Some(WorkspaceAction::NextTab),
        "tab:previous" => Some(WorkspaceAction::PreviousTab),
        "app:quit" => Some(WorkspaceAction::Quit),
        "settings:open" => Some(WorkspaceAction::OpenSettings),
        "theme:toggle-mode" => Some(WorkspaceAction::ToggleThemeMode),
        "connection:open" => ws.sidebar().selected().map(WorkspaceAction::OpenConnection),
        "dev:open-synthetic-grid" => Some(WorkspaceAction::OpenSyntheticGrid),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette;
    use crate::sidebar::SidebarModel;
    use crate::workspace;
    use sqler_config::{KeybindingOverrides, Platform};
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s, Platform::Other).unwrap()
    }

    fn setup() -> (CommandRegistry, KeyDispatcher) {
        let mut r = CommandRegistry::new();
        workspace::register_commands(&mut r).unwrap();
        palette::register_commands(&mut r).unwrap();
        let k = KeyDispatcher::new(Platform::Other, Ok(KeybindingOverrides::new()), &r);
        (r, k)
    }

    fn run(c: &str, p: &mut PaletteState, k: &KeyDispatcher, r: &CommandRegistry) -> KeyOutcome {
        route_key(&chord(c), p, k, r)
    }

    #[test]
    fn closed_palette() {
        let (r, k) = setup();
        let mut p = PaletteState::default();
        assert_eq!(run("ctrl+shift+p", &mut p, &k, &r), KeyOutcome::Handled);
        assert!(p.is_open());
        let mut p = PaletteState::default();
        let out = run("ctrl+w", &mut p, &k, &r);
        assert_eq!(out, KeyOutcome::Run(CommandId::new("tab:close").unwrap()));
        assert!(!p.is_open());
        assert_eq!(run("a", &mut p, &k, &r), KeyOutcome::Ignored);
        assert_eq!(run("escape", &mut p, &k, &r), KeyOutcome::Ignored);
        assert_eq!(run("enter", &mut p, &k, &r), KeyOutcome::Ignored);
    }

    #[test]
    fn open_palette_escape_closes() {
        let (r, k) = setup();
        let mut p = PaletteState::default();
        p.open();
        assert_eq!(run("escape", &mut p, &k, &r), KeyOutcome::Handled);
        assert!(!p.is_open());
    }

    #[test]
    fn open_palette_enter_runs_selected() {
        let (r, k) = setup();
        let mut p = PaletteState::default();
        p.open();
        p.set_query("quit");
        let out = run("enter", &mut p, &k, &r);
        assert_eq!(out, KeyOutcome::Run(CommandId::new("app:quit").unwrap()));
        assert!(!p.is_open());
    }

    #[test]
    fn open_palette_enter_with_nothing_is_handled() {
        let (r, k) = setup();
        let mut p = PaletteState::default();
        p.open();
        p.set_query("zzzz");
        assert_eq!(run("enter", &mut p, &k, &r), KeyOutcome::Handled);
        assert!(!p.is_open());
    }

    #[test]
    fn open_palette_arrows_move_and_wrap() {
        let (r, k) = setup();
        let mut p = PaletteState::default();
        p.open();
        assert_eq!(run("down", &mut p, &k, &r), KeyOutcome::Handled);
        assert_eq!(p.selected(), 1);
        assert_eq!(run("up", &mut p, &k, &r), KeyOutcome::Handled);
        assert_eq!(run("up", &mut p, &k, &r), KeyOutcome::Handled);
        assert_eq!(p.selected(), p.items(&r, &k).len() - 1);
    }

    #[test]
    fn open_palette_arrows_with_modifiers_are_ignored() {
        let (r, k) = setup();
        let mut p = PaletteState::default();
        p.open();
        for c in ["shift+down", "ctrl+up", "alt+enter", "shift+escape"] {
            assert_eq!(run(c, &mut p, &k, &r), KeyOutcome::Ignored, "{c}");
        }
        assert_eq!(p.selected(), 0);
        assert!(p.is_open());
    }

    #[test]
    fn open_palette_toggle_chord_closes() {
        let (r, k) = setup();
        let mut p = PaletteState::default();
        p.open();
        assert_eq!(run("ctrl+shift+p", &mut p, &k, &r), KeyOutcome::Handled);
        assert!(!p.is_open());
    }

    #[test]
    fn open_palette_other_chords_go_to_the_input() {
        let (r, k) = setup();
        let mut p = PaletteState::default();
        p.open();
        for c in ["a", "ctrl+w", "backspace", "left"] {
            assert_eq!(run(c, &mut p, &k, &r), KeyOutcome::Ignored, "{c}");
        }
        assert!(p.is_open());
    }

    fn ws(select: Option<usize>) -> Workspace {
        let d = DriverId::new("sqlite").unwrap();
        let mut sidebar = SidebarModel::from_connections(vec![
            ConnectionConfig::new("a", d.clone(), ":memory:"),
            ConnectionConfig::new("b", d, ":memory:"),
        ]);
        if let Some(i) = select {
            assert!(sidebar.select(i));
        }
        Workspace::new(sidebar)
    }

    fn act(id: &str, w: &Workspace) -> Option<WorkspaceAction> {
        action_for(&CommandId::new(id).unwrap(), w)
    }

    #[test]
    fn action_mappings() {
        let w = ws(None);
        assert_eq!(act("tab:close", &w), Some(WorkspaceAction::CloseActiveTab));
        assert_eq!(act("tab:next", &w), Some(WorkspaceAction::NextTab));
        assert_eq!(act("tab:previous", &w), Some(WorkspaceAction::PreviousTab));
        assert_eq!(act("app:quit", &w), Some(WorkspaceAction::Quit));
        assert_eq!(
            act("settings:open", &w),
            Some(WorkspaceAction::OpenSettings)
        );
        assert_eq!(
            act("theme:toggle-mode", &w),
            Some(WorkspaceAction::ToggleThemeMode)
        );
        assert_eq!(act("settings:set-theme", &w), None);
        assert_eq!(act(OPEN_PALETTE, &w), None);
        assert_eq!(act("connection:select", &w), None);
        assert_eq!(act("tab:activate", &w), None);
        assert_eq!(act("query:run", &w), None);
        assert_eq!(
            act("dev:open-synthetic-grid", &w),
            Some(WorkspaceAction::OpenSyntheticGrid)
        );
    }

    #[test]
    fn connection_open_needs_a_selection() {
        assert_eq!(act("connection:open", &ws(None)), None);
        assert_eq!(
            act("connection:open", &ws(Some(1))),
            Some(WorkspaceAction::OpenConnection(1))
        );
    }
}
