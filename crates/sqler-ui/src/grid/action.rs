// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `GridAction`: grid-local user actions, each mapped to a command ID.

use sqler_config::Platform;

use super::selection::{Motion, key_command};
use crate::command::{Command, CommandError, CommandId, CommandRegistry};
use crate::keymap::KeyInput;

/// The grid action a key press runs, deciding the routing of grid keys against the global
/// chord router.
///
/// `table_focused` is whether the data table (or something in it) has the keyboard focus. Only
/// then does the grid claim a key, so Ctrl+Space is "select column" in the grid and
/// `editor:trigger-completion` everywhere else. A key the grid does not claim gives `None` and
/// goes on to the chord router, where the user's bindings apply. `overlay_open` is whether the
/// palette, a dialog or a prompt is open: the grid then claims nothing.
///
/// Tab and Shift+Tab are claimed only when `moves(motion)` says the move changes the active
/// cell; at the last (first) cell they go on to the router, so a keyboard user can tab out of
/// the table.
///
/// Plain Enter and F2 start editing the active cell and plain Delete (Cmd+Backspace on macOS)
/// deletes the selected rows; they are claimed before the motion keys. While a cell `editing`
/// the grid claims nothing: the cell editor has the keys, see [`cell_editor_key`].
#[must_use]
pub fn grid_key_action(
    input: &KeyInput,
    platform: Platform,
    table_focused: bool,
    overlay_open: bool,
    editing: bool,
    moves: impl Fn(Motion) -> bool,
) -> Option<GridAction> {
    if !table_focused || overlay_open || editing {
        return None;
    }
    if let Some(action) = edit_key_action(input, platform) {
        return Some(action);
    }
    let action = key_command(input, platform)
        .and_then(|id| CommandId::new(id).ok())
        .and_then(|id| GridAction::from_command_id(&id))?;
    if input.key == "tab"
        && let GridAction::Move { to, .. } = action
        && !moves(to)
    {
        return None;
    }
    Some(action)
}

/// The editing action a key runs, if any: Enter and F2 edit, Delete deletes.
fn edit_key_action(input: &KeyInput, platform: Platform) -> Option<GridAction> {
    let plain = !(input.ctrl || input.alt || input.shift || input.cmd);
    match input.key.as_str() {
        "enter" | "f2" if plain => Some(GridAction::EditCell),
        "delete" if plain => Some(GridAction::DeleteRows),
        "backspace"
            if platform == Platform::MacOs
                && input.cmd
                && !(input.ctrl || input.alt || input.shift) =>
        {
            Some(GridAction::DeleteRows)
        }
        _ => None,
    }
}

/// What a key does inside the cell editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditorKey {
    /// Accept the input; then move the active cell if `then` is set.
    Commit {
        /// Where the active cell goes next.
        then: Option<Motion>,
    },
    /// Close the editor and drop the input.
    Cancel,
}

/// The cell-editor command of a key, if any: Enter commits, Tab commits and moves right,
/// Shift+Tab commits and moves left, Escape cancels. Any other key (and any key with Ctrl, Alt or
/// Cmd) is the input's own: Shift+Enter, for instance, is a line break in a multi-line editor.
#[must_use]
pub fn cell_editor_key(input: &KeyInput) -> Option<EditorKey> {
    if input.ctrl || input.alt || input.cmd {
        return None;
    }
    match (input.key.as_str(), input.shift) {
        ("enter", false) => Some(EditorKey::Commit { then: None }),
        ("tab", false) => Some(EditorKey::Commit {
            then: Some(Motion::Right),
        }),
        ("tab", true) => Some(EditorKey::Commit {
            then: Some(Motion::Left),
        }),
        ("escape", false) => Some(EditorKey::Cancel),
        _ => None,
    }
}

/// A user action on a results grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GridAction {
    /// Scroll to the first row.
    ScrollToTop,
    /// Scroll to the last row.
    ScrollToEnd,
    /// Move the active cell; with `extend`, grow the selected range instead.
    Move {
        /// Where to.
        to: Motion,
        /// Keep the anchor (Shift held).
        extend: bool,
    },
    /// Select every cell.
    SelectAll,
    /// Select the row of the active cell.
    SelectRow,
    /// Select the column of the active cell.
    SelectColumn,
    /// Pin the column of the active cell.
    PinColumn,
    /// Unpin the column of the active cell.
    UnpinColumn,
    /// Move the column of the active cell one position left.
    MoveColumnLeft,
    /// Move the column of the active cell one position right.
    MoveColumnRight,
    /// Fit the column of the active cell to its visible cells.
    AutofitColumn,
    /// Restore column order, widths and pinning.
    ResetLayout,
    /// Sort ascending by the column of the active cell.
    SortAscending,
    /// Sort descending by the column of the active cell.
    SortDescending,
    /// Remove the sort.
    ClearSort,
    /// Remove the quick filter and every condition.
    ClearFilter,
    /// Switch between case-insensitive and code point text order and filter matching.
    ToggleMatchCase,
    /// Load the next page of a result that stopped at its page limit.
    FetchMore,
    /// Edit the active cell.
    EditCell,
    /// Set the selected cells to `NULL`.
    SetNull,
    /// Take back the pending changes in the selection.
    RevertSelection,
    /// Add a new row at the end.
    AddRow,
    /// Delete the selected rows (or take the deletion back).
    DeleteRows,
    /// Show the statements the pending changes make.
    PreviewChanges,
    /// Apply the pending changes.
    ApplyChanges,
    /// Drop every pending change.
    DiscardChanges,
    /// Allow editing a result without a key, matching rows on all their values.
    EditWithoutKey,
    /// Show or hide the detail pane of the active cell.
    ToggleCellDetail,
}

const fn mv(to: Motion) -> GridAction {
    GridAction::Move { to, extend: false }
}

const fn ext(to: Motion) -> GridAction {
    GridAction::Move { to, extend: true }
}

impl GridAction {
    /// Every action, in palette order.
    pub const ALL: [GridAction; 47] = [
        Self::ScrollToTop,
        Self::ScrollToEnd,
        mv(Motion::Up),
        mv(Motion::Down),
        mv(Motion::Left),
        mv(Motion::Right),
        mv(Motion::RowStart),
        mv(Motion::RowEnd),
        mv(Motion::PageUp),
        mv(Motion::PageDown),
        mv(Motion::First),
        mv(Motion::Last),
        ext(Motion::Up),
        ext(Motion::Down),
        ext(Motion::Left),
        ext(Motion::Right),
        ext(Motion::RowStart),
        ext(Motion::RowEnd),
        ext(Motion::PageUp),
        ext(Motion::PageDown),
        ext(Motion::First),
        ext(Motion::Last),
        Self::SelectAll,
        Self::SelectRow,
        Self::SelectColumn,
        Self::PinColumn,
        Self::UnpinColumn,
        Self::MoveColumnLeft,
        Self::MoveColumnRight,
        Self::AutofitColumn,
        Self::ResetLayout,
        Self::SortAscending,
        Self::SortDescending,
        Self::ClearSort,
        Self::ClearFilter,
        Self::ToggleMatchCase,
        Self::FetchMore,
        Self::EditCell,
        Self::SetNull,
        Self::RevertSelection,
        Self::AddRow,
        Self::DeleteRows,
        Self::PreviewChanges,
        Self::ApplyChanges,
        Self::DiscardChanges,
        Self::EditWithoutKey,
        Self::ToggleCellDetail,
    ];

    /// The ID string of the command this action runs.
    fn id_str(self) -> &'static str {
        match self {
            Self::ScrollToTop => "grid:scroll-to-top",
            Self::ScrollToEnd => "grid:scroll-to-end",
            Self::Move { to, extend: false } => match to {
                Motion::Up => "grid:move-up",
                Motion::Down => "grid:move-down",
                Motion::Left => "grid:move-left",
                Motion::Right => "grid:move-right",
                Motion::RowStart => "grid:move-row-start",
                Motion::RowEnd => "grid:move-row-end",
                Motion::PageUp => "grid:move-page-up",
                Motion::PageDown => "grid:move-page-down",
                Motion::First => "grid:move-first-cell",
                Motion::Last => "grid:move-last-cell",
            },
            Self::Move { to, extend: true } => match to {
                Motion::Up => "grid:extend-up",
                Motion::Down => "grid:extend-down",
                Motion::Left => "grid:extend-left",
                Motion::Right => "grid:extend-right",
                Motion::RowStart => "grid:extend-row-start",
                Motion::RowEnd => "grid:extend-row-end",
                Motion::PageUp => "grid:extend-page-up",
                Motion::PageDown => "grid:extend-page-down",
                Motion::First => "grid:extend-first-cell",
                Motion::Last => "grid:extend-last-cell",
            },
            Self::SelectAll => "grid:select-all",
            Self::SelectRow => "grid:select-row",
            Self::SelectColumn => "grid:select-column",
            Self::PinColumn => "grid:pin-column",
            Self::UnpinColumn => "grid:unpin-column",
            Self::MoveColumnLeft => "grid:move-column-left",
            Self::MoveColumnRight => "grid:move-column-right",
            Self::AutofitColumn => "grid:autofit-column",
            Self::ResetLayout => "grid:reset-layout",
            Self::SortAscending => "grid:sort-ascending",
            Self::SortDescending => "grid:sort-descending",
            Self::ClearSort => "grid:clear-sort",
            Self::ClearFilter => "grid:clear-filter",
            Self::ToggleMatchCase => "grid:toggle-match-case",
            Self::FetchMore => "grid:fetch-more",
            Self::EditCell => "grid:edit-cell",
            Self::SetNull => "grid:set-null",
            Self::RevertSelection => "grid:revert-selection",
            Self::AddRow => "grid:add-row",
            Self::DeleteRows => "grid:delete-rows",
            Self::PreviewChanges => "grid:preview-changes",
            Self::ApplyChanges => "grid:apply-changes",
            Self::DiscardChanges => "grid:discard-changes",
            Self::EditWithoutKey => "grid:edit-without-key",
            Self::ToggleCellDetail => "grid:toggle-cell-detail",
        }
    }

    /// The palette title of the command.
    #[must_use]
    pub fn title(&self) -> &'static str {
        match *self {
            Self::ScrollToTop => "Scroll Results to Top",
            Self::ScrollToEnd => "Scroll Results to End",
            Self::Move { to, extend: false } => match to {
                Motion::Up => "Results: Move Up",
                Motion::Down => "Results: Move Down",
                Motion::Left => "Results: Move Left",
                Motion::Right => "Results: Move Right",
                Motion::RowStart => "Results: Move to Row Start",
                Motion::RowEnd => "Results: Move to Row End",
                Motion::PageUp => "Results: Move Page Up",
                Motion::PageDown => "Results: Move Page Down",
                Motion::First => "Results: Move to First Cell",
                Motion::Last => "Results: Move to Last Cell",
            },
            Self::Move { to, extend: true } => match to {
                Motion::Up => "Results: Extend Selection Up",
                Motion::Down => "Results: Extend Selection Down",
                Motion::Left => "Results: Extend Selection Left",
                Motion::Right => "Results: Extend Selection Right",
                Motion::RowStart => "Results: Extend Selection to Row Start",
                Motion::RowEnd => "Results: Extend Selection to Row End",
                Motion::PageUp => "Results: Extend Selection Page Up",
                Motion::PageDown => "Results: Extend Selection Page Down",
                Motion::First => "Results: Extend Selection to First Cell",
                Motion::Last => "Results: Extend Selection to Last Cell",
            },
            Self::SelectAll => "Results: Select All",
            Self::SelectRow => "Results: Select Row",
            Self::SelectColumn => "Results: Select Column",
            Self::PinColumn => "Results: Pin Column",
            Self::UnpinColumn => "Results: Unpin Column",
            Self::MoveColumnLeft => "Results: Move Column Left",
            Self::MoveColumnRight => "Results: Move Column Right",
            Self::AutofitColumn => "Results: Autofit Column",
            Self::ResetLayout => "Results: Reset Column Layout",
            Self::SortAscending => "Results: Sort Ascending",
            Self::SortDescending => "Results: Sort Descending",
            Self::ClearSort => "Results: Clear Sort",
            Self::ClearFilter => "Results: Clear Filter",
            Self::ToggleMatchCase => "Results: Match Case in Sort and Filter",
            Self::FetchMore => "Fetch More Rows",
            Self::EditCell => "Results: Edit Cell",
            Self::SetNull => "Results: Set Cell to NULL",
            Self::RevertSelection => "Results: Revert Selected Changes",
            Self::AddRow => "Results: Add Row",
            Self::DeleteRows => "Results: Delete Rows",
            Self::PreviewChanges => "Results: Preview Changes",
            Self::ApplyChanges => "Results: Apply Changes",
            Self::DiscardChanges => "Results: Discard Changes",
            Self::EditWithoutKey => "Results: Edit Without a Key",
            Self::ToggleCellDetail => "Results: Toggle Cell Detail",
        }
    }

    /// The ID of the command this action runs.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(&self) -> CommandId {
        CommandId::new(self.id_str()).expect("constant command ID is valid")
    }

    /// The action a command ID stands for, if it is a grid command.
    #[must_use]
    pub fn from_command_id(id: &CommandId) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.command_id() == *id)
    }
}

/// Registers the grid commands.
///
/// # Errors
///
/// Returns [`CommandError::Duplicate`] if one of the IDs is already registered.
pub fn register_commands(registry: &mut CommandRegistry) -> Result<(), CommandError> {
    for a in GridAction::ALL {
        registry.register(Command::new(a.id_str(), a.title())?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn ids_are_valid_unique_and_prefixed() {
        let mut seen = HashSet::new();
        for a in GridAction::ALL {
            let id = CommandId::new(a.id_str()).unwrap();
            assert!(id.to_string().starts_with("grid:"), "{a:?}");
            assert!(seen.insert(id), "duplicate ID for {a:?}");
        }
        assert_eq!(seen.len(), 47);
    }

    #[test]
    fn titles_are_distinct_and_not_empty() {
        let titles: HashSet<_> = GridAction::ALL.iter().map(GridAction::title).collect();
        assert_eq!(titles.len(), GridAction::ALL.len());
        assert!(titles.iter().all(|t| !t.is_empty()));
    }

    #[test]
    fn every_action_is_registered() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), GridAction::ALL.len());
        for a in GridAction::ALL {
            let c = r.get(&a.command_id()).unwrap();
            assert_eq!(c.title(), a.title(), "{a:?}");
        }
    }

    #[test]
    fn known_ids() {
        assert_eq!(
            GridAction::ScrollToTop.command_id().to_string(),
            "grid:scroll-to-top"
        );
        assert_eq!(
            GridAction::ScrollToEnd.command_id().to_string(),
            "grid:scroll-to-end"
        );
        assert_eq!(
            GridAction::FetchMore.command_id().to_string(),
            "grid:fetch-more"
        );
        assert_eq!(
            GridAction::ToggleCellDetail.command_id().to_string(),
            "grid:toggle-cell-detail"
        );
        assert_eq!(
            GridAction::ToggleCellDetail.title(),
            "Results: Toggle Cell Detail"
        );
        assert_eq!(
            GridAction::PinColumn.command_id().to_string(),
            "grid:pin-column"
        );
        assert_eq!(
            GridAction::AutofitColumn.command_id().to_string(),
            "grid:autofit-column"
        );
        for (a, id, title) in [
            (
                GridAction::SortAscending,
                "grid:sort-ascending",
                "Results: Sort Ascending",
            ),
            (
                GridAction::SortDescending,
                "grid:sort-descending",
                "Results: Sort Descending",
            ),
            (
                GridAction::ClearSort,
                "grid:clear-sort",
                "Results: Clear Sort",
            ),
            (
                GridAction::ClearFilter,
                "grid:clear-filter",
                "Results: Clear Filter",
            ),
            (
                GridAction::ToggleMatchCase,
                "grid:toggle-match-case",
                "Results: Match Case in Sort and Filter",
            ),
        ] {
            assert_eq!(a.command_id().to_string(), id);
            assert_eq!(a.title(), title);
        }
        assert_eq!(mv(Motion::Down).command_id().to_string(), "grid:move-down");
        assert_eq!(
            ext(Motion::Last).command_id().to_string(),
            "grid:extend-last-cell"
        );
        assert_eq!(mv(Motion::Down).title(), "Results: Move Down");
        assert_eq!(ext(Motion::Down).title(), "Results: Extend Selection Down");
        assert_eq!(GridAction::PinColumn.title(), "Results: Pin Column");
        assert_eq!(
            GridAction::ResetLayout.title(),
            "Results: Reset Column Layout"
        );
    }

    #[test]
    fn from_command_id_round_trips() {
        for a in GridAction::ALL {
            assert_eq!(GridAction::from_command_id(&a.command_id()), Some(a));
        }
        let other = CommandId::new("editor:run").unwrap();
        assert_eq!(GridAction::from_command_id(&other), None);
        let unknown = CommandId::new("grid:nope").unwrap();
        assert_eq!(GridAction::from_command_id(&unknown), None);
    }

    #[test]
    fn registering_twice_is_a_duplicate_error() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert!(matches!(
            register_commands(&mut r),
            Err(CommandError::Duplicate(_))
        ));
    }

    #[test]
    fn key_commands_are_registered_actions() {
        use crate::keymap::KeyInput;
        use sqler_config::Platform;

        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        for key in [
            "up", "down", "left", "right", "home", "end", "pageup", "pagedown", "tab", "a", "space",
        ] {
            for (ctrl, shift) in [(false, false), (true, false), (false, true), (true, true)] {
                let input = KeyInput {
                    ctrl,
                    shift,
                    key: key.into(),
                    ..KeyInput::default()
                };
                if let Some(id) = super::super::selection::key_command(&input, Platform::Other) {
                    let id = CommandId::new(id).unwrap();
                    assert!(GridAction::from_command_id(&id).is_some(), "{id}");
                }
            }
        }
    }

    #[test]
    fn ctrl_space_belongs_to_the_grid_only_while_the_table_has_focus() {
        let ctrl_space = KeyInput {
            ctrl: true,
            key: "space".into(),
            ..KeyInput::default()
        };
        for p in [Platform::MacOs, Platform::Other] {
            assert_eq!(
                grid_key_action(&ctrl_space, p, true, false, false, |_| true),
                Some(GridAction::SelectColumn)
            );
            // Editor (or anything else) focused: the chord router resolves the completion.
            assert_eq!(
                grid_key_action(&ctrl_space, p, false, false, false, |_| true),
                None
            );
            // Palette or dialog open: the grid claims nothing.
            assert_eq!(
                grid_key_action(&ctrl_space, p, true, true, false, |_| true),
                None
            );
        }
    }

    #[test]
    fn tab_is_claimed_only_when_it_moves_the_active_cell() {
        let tab = |shift: bool| KeyInput {
            shift,
            key: "tab".into(),
            ..KeyInput::default()
        };
        let p = Platform::Other;
        assert_eq!(
            grid_key_action(&tab(false), p, true, false, false, |_| true),
            Some(mv(Motion::Right))
        );
        assert_eq!(
            grid_key_action(&tab(true), p, true, false, false, |_| true),
            Some(mv(Motion::Left))
        );
        // At the edge the move changes nothing: the key is left to the router.
        assert_eq!(
            grid_key_action(&tab(false), p, true, false, false, |_| false),
            None
        );
        assert_eq!(
            grid_key_action(&tab(true), p, true, false, false, |_| false),
            None
        );
        // Only Tab is conditional: an arrow at the edge is still claimed.
        let right = KeyInput {
            key: "right".into(),
            ..KeyInput::default()
        };
        assert_eq!(
            grid_key_action(&right, p, true, false, false, |_| false),
            Some(mv(Motion::Right))
        );
    }

    #[test]
    fn keys_the_grid_does_not_claim_go_to_the_router() {
        let input = |ctrl: bool, key: &str| KeyInput {
            ctrl,
            key: key.into(),
            ..KeyInput::default()
        };
        let down = grid_key_action(
            &input(false, "down"),
            Platform::Other,
            true,
            false,
            false,
            |_| true,
        );
        assert!(matches!(down, Some(GridAction::Move { .. })));
        // Global chords of the defaults are not grid keys.
        for (ctrl, key) in [
            (true, "w"),
            (true, "tab"),
            (true, "n"),
            (true, "q"),
            (true, "."),
        ] {
            assert_eq!(
                grid_key_action(
                    &input(ctrl, key),
                    Platform::Other,
                    true,
                    false,
                    false,
                    |_| true
                ),
                None,
                "{key}"
            );
        }
    }

    fn key(mods: &str, key: &str) -> KeyInput {
        KeyInput {
            ctrl: mods.contains('c'),
            alt: mods.contains('a'),
            shift: mods.contains('s'),
            cmd: mods.contains('m'),
            key: key.into(),
        }
    }

    fn claimed(k: &KeyInput, p: Platform) -> Option<GridAction> {
        grid_key_action(k, p, true, false, false, |_| true)
    }

    #[test]
    fn the_edit_actions_have_ids_and_titles() {
        let ids = [
            (GridAction::EditCell, "grid:edit-cell", "Results: Edit Cell"),
            (
                GridAction::SetNull,
                "grid:set-null",
                "Results: Set Cell to NULL",
            ),
            (
                GridAction::RevertSelection,
                "grid:revert-selection",
                "Results: Revert Selected Changes",
            ),
            (GridAction::AddRow, "grid:add-row", "Results: Add Row"),
            (
                GridAction::DeleteRows,
                "grid:delete-rows",
                "Results: Delete Rows",
            ),
            (
                GridAction::PreviewChanges,
                "grid:preview-changes",
                "Results: Preview Changes",
            ),
            (
                GridAction::ApplyChanges,
                "grid:apply-changes",
                "Results: Apply Changes",
            ),
            (
                GridAction::DiscardChanges,
                "grid:discard-changes",
                "Results: Discard Changes",
            ),
            (
                GridAction::EditWithoutKey,
                "grid:edit-without-key",
                "Results: Edit Without a Key",
            ),
        ];
        for (action, id, title) in ids {
            assert_eq!(action.command_id().to_string(), id);
            assert_eq!(action.title(), title);
            assert!(title.starts_with("Results: "));
            assert_eq!(
                GridAction::from_command_id(&action.command_id()),
                Some(action)
            );
        }
        assert_eq!(GridAction::ALL.len(), 47);
    }

    #[test]
    fn enter_f2_and_delete_edit_before_the_motion_keys() {
        for p in [Platform::MacOs, Platform::Other] {
            assert_eq!(claimed(&key("", "enter"), p), Some(GridAction::EditCell));
            assert_eq!(claimed(&key("", "f2"), p), Some(GridAction::EditCell));
            assert_eq!(claimed(&key("", "delete"), p), Some(GridAction::DeleteRows));
            // Only the plain keys.
            for mods in ["c", "a", "s", "m", "cs"] {
                assert_eq!(claimed(&key(mods, "enter"), p), None, "{mods}");
                assert_eq!(claimed(&key(mods, "f2"), p), None, "{mods}");
                assert_eq!(claimed(&key(mods, "delete"), p), None, "{mods}");
            }
            // Plain Backspace never deletes rows.
            assert_eq!(claimed(&key("", "backspace"), p), None);
        }
        assert_eq!(
            claimed(&key("m", "backspace"), Platform::MacOs),
            Some(GridAction::DeleteRows)
        );
        assert_eq!(claimed(&key("m", "backspace"), Platform::Other), None);
        assert_eq!(claimed(&key("c", "backspace"), Platform::MacOs), None);
        assert_eq!(claimed(&key("ms", "backspace"), Platform::MacOs), None);
    }

    #[test]
    fn the_grid_claims_no_key_while_editing_or_without_focus() {
        let p = Platform::Other;
        for k in [
            key("", "enter"),
            key("", "delete"),
            key("", "down"),
            key("", "tab"),
        ] {
            assert_eq!(grid_key_action(&k, p, true, false, true, |_| true), None);
            assert_eq!(grid_key_action(&k, p, false, false, false, |_| true), None);
            assert_eq!(grid_key_action(&k, p, true, true, false, |_| true), None);
        }
    }

    #[test]
    fn cell_editor_keys() {
        use EditorKey::{Cancel, Commit};
        assert_eq!(
            cell_editor_key(&key("", "enter")),
            Some(Commit { then: None })
        );
        assert_eq!(
            cell_editor_key(&key("", "tab")),
            Some(Commit {
                then: Some(Motion::Right)
            })
        );
        assert_eq!(
            cell_editor_key(&key("s", "tab")),
            Some(Commit {
                then: Some(Motion::Left)
            })
        );
        assert_eq!(cell_editor_key(&key("", "escape")), Some(Cancel));
        // Everything else belongs to the input.
        for k in [
            key("s", "enter"),
            key("c", "enter"),
            key("m", "enter"),
            key("a", "tab"),
            key("c", "escape"),
            key("", "a"),
            key("", "left"),
            key("", "delete"),
            key("", "f2"),
            key("", "backspace"),
        ] {
            assert_eq!(cell_editor_key(&k), None, "{k:?}");
        }
    }
}
