// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The query toolbar model: items, titles, command IDs and why an item is disabled.
//!
//! Headless; the view only renders it and runs `command_id` through the command pipeline.

use crate::command::CommandId;

use super::explain::EXPLAIN_UNSUPPORTED;

/// The reason Run, Run All and Explain give while a run, prompt or confirmation is open.
const BUSY: &str = "A query is already running or waiting for input";
const NOT_CONNECTED: &str = "The connection of this tab is not available.";

/// A toolbar button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarItem {
    /// Run the statement at the cursor.
    Run,
    /// Run the whole buffer.
    RunAll,
    /// Cancel the running query.
    Cancel,
    /// Explain the statement at the cursor.
    Explain,
    /// Format the SQL.
    Format,
    /// Save the query.
    Save,
}

impl ToolbarItem {
    /// Every item, in toolbar order.
    pub const ALL: [Self; 6] = [
        Self::Run,
        Self::RunAll,
        Self::Cancel,
        Self::Explain,
        Self::Format,
        Self::Save,
    ];

    fn id_str(self) -> &'static str {
        match self {
            Self::Run => "query:run-statement",
            Self::RunAll => "query:run-all",
            Self::Cancel => "query:cancel",
            Self::Explain => "query:explain",
            Self::Format => "editor:format-sql",
            Self::Save => "query:save",
        }
    }

    /// The command the item runs.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(self) -> CommandId {
        CommandId::new(self.id_str()).expect("constant command ID is valid")
    }

    /// The title, used for the tooltip and the accessibility label.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Run => "Run Statement at Cursor",
            Self::RunAll => "Run All",
            Self::Cancel => "Cancel Query",
            Self::Explain => "Explain Statement at Cursor",
            Self::Format => "Format SQL",
            Self::Save => "Save Query",
        }
    }
}

/// Where the run lifecycle of the tab stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Nothing is happening.
    Idle,
    /// A parameter prompt or the confirmation is open.
    Busy,
    /// A job is running.
    Running,
    /// Cancel was requested.
    Cancelling,
}

/// What decides which items are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolbarContext {
    /// The run state.
    pub run: RunState,
    /// The tab has a session.
    pub connected: bool,
    /// The driver has an explain form.
    pub explain: bool,
}

impl ToolbarContext {
    /// Why `item` is disabled, or `None` when it is enabled.
    #[must_use]
    pub fn disabled_reason(&self, item: ToolbarItem) -> Option<&'static str> {
        match item {
            ToolbarItem::Run | ToolbarItem::RunAll | ToolbarItem::Explain => {
                if self.run != RunState::Idle {
                    Some(BUSY)
                } else if !self.connected {
                    Some(NOT_CONNECTED)
                } else if item == ToolbarItem::Explain && !self.explain {
                    Some(EXPLAIN_UNSUPPORTED)
                } else {
                    None
                }
            }
            ToolbarItem::Cancel => match self.run {
                RunState::Idle => Some("No query is running."),
                RunState::Cancelling => Some("Cancelling…"),
                RunState::Busy | RunState::Running => None,
            },
            ToolbarItem::Format | ToolbarItem::Save => None,
        }
    }
}

/// The tooltip: `"{title} ({hint})"` when enabled, `"{title}: {reason}"` when disabled.
#[must_use]
pub fn toolbar_tooltip(item: ToolbarItem, hint: Option<&str>, reason: Option<&str>) -> String {
    match (reason, hint) {
        (Some(reason), _) => format!("{}: {reason}", item.title()),
        (None, Some(hint)) => format!("{} ({hint})", item.title()),
        (None, None) => item.title().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::RunError;

    fn ctx(run: RunState, connected: bool, explain: bool) -> ToolbarContext {
        ToolbarContext {
            run,
            connected,
            explain,
        }
    }

    #[test]
    fn ids_are_valid_and_titles_fixed() {
        let expected = [
            ("query:run-statement", "Run Statement at Cursor"),
            ("query:run-all", "Run All"),
            ("query:cancel", "Cancel Query"),
            ("query:explain", "Explain Statement at Cursor"),
            ("editor:format-sql", "Format SQL"),
            ("query:save", "Save Query"),
        ];
        for (item, (id, title)) in ToolbarItem::ALL.into_iter().zip(expected) {
            assert_eq!(item.command_id().as_str(), id);
            assert_eq!(item.title(), title);
        }
    }

    #[test]
    fn busy_reason_matches_the_run_error() {
        assert_eq!(BUSY, RunError::Busy.to_string());
    }

    #[test]
    fn reason_table() {
        use RunState::{Busy, Cancelling, Idle, Running};
        use ToolbarItem::{Cancel, Explain, Format, Run, RunAll, Save};
        let ok = ctx(Idle, true, true);
        for item in ToolbarItem::ALL {
            let want = if item == Cancel {
                Some("No query is running.")
            } else {
                None
            };
            assert_eq!(ok.disabled_reason(item), want, "{item:?}");
        }
        for run in [Busy, Running, Cancelling] {
            let c = ctx(run, true, true);
            for item in [Run, RunAll, Explain] {
                assert_eq!(c.disabled_reason(item), Some(BUSY));
            }
            assert_eq!(c.disabled_reason(Format), None);
            assert_eq!(c.disabled_reason(Save), None);
        }
        assert_eq!(ctx(Busy, true, true).disabled_reason(Cancel), None);
        assert_eq!(ctx(Running, true, true).disabled_reason(Cancel), None);
        assert_eq!(
            ctx(Cancelling, true, true).disabled_reason(Cancel),
            Some("Cancelling…")
        );
        let off = ctx(Idle, false, true);
        for item in [Run, RunAll, Explain] {
            assert_eq!(off.disabled_reason(item), Some(NOT_CONNECTED));
        }
        let no_explain = ctx(Idle, true, false);
        assert_eq!(no_explain.disabled_reason(Run), None);
        assert_eq!(no_explain.disabled_reason(RunAll), None);
        assert_eq!(
            no_explain.disabled_reason(Explain),
            Some(EXPLAIN_UNSUPPORTED)
        );
        // Not connected wins over unsupported.
        assert_eq!(
            ctx(Idle, false, false).disabled_reason(Explain),
            Some(NOT_CONNECTED)
        );
    }

    #[test]
    fn tooltip_forms() {
        let run = ToolbarItem::Run;
        assert_eq!(
            toolbar_tooltip(run, Some("Cmd+Enter"), None),
            "Run Statement at Cursor (Cmd+Enter)"
        );
        assert_eq!(toolbar_tooltip(run, None, None), "Run Statement at Cursor");
        assert_eq!(
            toolbar_tooltip(run, Some("Cmd+Enter"), Some("busy")),
            "Run Statement at Cursor: busy"
        );
    }
}
