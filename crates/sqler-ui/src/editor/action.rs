// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `EditorAction`: SQL editor actions, each mapped to a command ID.

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

/// A user action on the SQL editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditorAction {
    /// Open a new SQL editor for the active or selected connection.
    New,
    /// Format the selection, or the whole buffer when nothing is selected.
    FormatSql,
    /// Select the word at the cursor, then add the next occurrence of the selection.
    AddNextOccurrence,
    /// Add a cursor on the line above.
    AddCursorAbove,
    /// Add a cursor on the line below.
    AddCursorBelow,
    /// Select the bracket that matches the one at the cursor.
    GoToMatchingBracket,
    /// Open the completion popup at the cursor.
    TriggerCompletion,
    /// Open a synthetic 10,000-line script in a new editor (for checking responsiveness).
    OpenSyntheticScript,
}

/// A key the editor itself handles (the view forwards it to the editor state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditorKey {
    /// Add a cursor on the line above.
    AddCursorAbove,
    /// Add a cursor on the line below.
    AddCursorBelow,
    /// Open the completion popup at the cursor.
    ShowCompletions,
}

/// The commands: ID and title.
pub const COMMANDS: &[(&str, &str)] = &[
    ("editor:new", "Editor: New SQL Editor"),
    ("editor:format-sql", "Editor: Format SQL"),
    ("editor:add-next-occurrence", "Editor: Add Next Occurrence"),
    ("editor:add-cursor-above", "Editor: Add Cursor Above"),
    ("editor:add-cursor-below", "Editor: Add Cursor Below"),
    (
        "editor:go-to-matching-bracket",
        "Editor: Go to Matching Bracket",
    ),
    ("editor:trigger-completion", "Editor: Trigger Completion"),
    (
        "dev:open-large-sql-script",
        "Open Synthetic 10,000-Line SQL Script",
    ),
];

impl EditorAction {
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

    fn id_str(self) -> &'static str {
        match self {
            Self::New => "editor:new",
            Self::FormatSql => "editor:format-sql",
            Self::AddNextOccurrence => "editor:add-next-occurrence",
            Self::AddCursorAbove => "editor:add-cursor-above",
            Self::AddCursorBelow => "editor:add-cursor-below",
            Self::GoToMatchingBracket => "editor:go-to-matching-bracket",
            Self::TriggerCompletion => "editor:trigger-completion",
            Self::OpenSyntheticScript => "dev:open-large-sql-script",
        }
    }
}

/// The action a command ID runs, if it is an editor command.
#[must_use]
pub fn action_for(id: &CommandId) -> Option<EditorAction> {
    Some(match id.as_str() {
        "editor:new" => EditorAction::New,
        "editor:format-sql" => EditorAction::FormatSql,
        "editor:add-next-occurrence" => EditorAction::AddNextOccurrence,
        "editor:add-cursor-above" => EditorAction::AddCursorAbove,
        "editor:add-cursor-below" => EditorAction::AddCursorBelow,
        "editor:go-to-matching-bracket" => EditorAction::GoToMatchingBracket,
        "editor:trigger-completion" => EditorAction::TriggerCompletion,
        "dev:open-large-sql-script" => EditorAction::OpenSyntheticScript,
        _ => return None,
    })
}

/// Registers the editor commands.
///
/// # Errors
///
/// Returns [`CommandError::Duplicate`] if one of the IDs is already registered.
pub fn register_commands(registry: &mut CommandRegistry) -> Result<(), CommandError> {
    for (id, title) in COMMANDS {
        registry.register(Command::new(id, *title)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const ALL: [EditorAction; 8] = [
        EditorAction::New,
        EditorAction::FormatSql,
        EditorAction::AddNextOccurrence,
        EditorAction::AddCursorAbove,
        EditorAction::AddCursorBelow,
        EditorAction::GoToMatchingBracket,
        EditorAction::TriggerCompletion,
        EditorAction::OpenSyntheticScript,
    ];

    #[test]
    fn ids_are_valid() {
        for (id, title) in COMMANDS {
            CommandId::new(id).unwrap();
            assert_ne!(title.len(), 0);
        }
    }

    #[test]
    fn there_are_no_duplicates() {
        let ids: HashSet<_> = COMMANDS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), COMMANDS.len());
        let titles: HashSet<_> = COMMANDS.iter().map(|(_, t)| *t).collect();
        assert_eq!(titles.len(), COMMANDS.len());
    }

    #[test]
    fn every_action_is_registered() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), COMMANDS.len());
        assert_eq!(ALL.len(), COMMANDS.len());
        for a in ALL {
            assert!(r.get(&a.command_id()).is_some(), "{a:?}");
        }
    }

    #[test]
    fn every_command_has_an_action_that_maps_back() {
        for (id, _) in COMMANDS {
            let cid = CommandId::new(id).unwrap();
            let action = action_for(&cid).unwrap_or_else(|| panic!("no action for {id}"));
            assert_eq!(action.command_id(), cid);
            assert!(ALL.contains(&action));
        }
        assert_eq!(
            action_for(&CommandId::new("object:open-data").unwrap()),
            None
        );
        assert_eq!(action_for(&CommandId::new("editor:nope").unwrap()), None);
    }

    #[test]
    fn spec_ids_and_titles() {
        assert_eq!(EditorAction::New.command_id().to_string(), "editor:new");
        assert_eq!(
            EditorAction::OpenSyntheticScript.command_id().to_string(),
            "dev:open-large-sql-script"
        );
        assert_eq!(
            EditorAction::TriggerCompletion.command_id().to_string(),
            "editor:trigger-completion"
        );
        let title = |id: &str| COMMANDS.iter().find(|(i, _)| *i == id).map(|(_, t)| *t);
        assert_eq!(title("editor:format-sql"), Some("Editor: Format SQL"));
        assert_eq!(
            title("editor:trigger-completion"),
            Some("Editor: Trigger Completion")
        );
        assert_eq!(
            title("dev:open-large-sql-script"),
            Some("Open Synthetic 10,000-Line SQL Script")
        );
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
}
