// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The SQL editor core: everything about an editor tab that is not drawing.
//!
//! The text lives in the `GPUI` editor state; the view copies an [`EditorBuffer`] (text, active selection, cursor)
//! out of it at command time and hands it to the headless logic here, which answers with [`TextEdit`]s and
//! ranges that the view applies. Query execution is reached through the narrow [`RunPort`] seam.

mod action;
mod brackets;
mod buffer;
mod doc;
mod format;
mod occurrence;
mod run;
mod run_t8;
pub mod synthetic;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use action::{COMMANDS, EditorAction, EditorKey, action_for, register_commands};
pub use brackets::{BracketIndex, touches_bracket};
pub use buffer::{EditorBuffer, TextEdit};
pub use doc::{EditorDoc, EditorDocs};
pub use format::{FormatFailure, format_edit};
pub use occurrence::{OccurrenceChain, OccurrenceStep, next_occurrence, word_at};
pub use run::{RunMode, RunPort};
pub use run_t8::{QueryRunPort, query_mode, run_mode, run_snapshot};

#[cfg(test)]
pub(crate) use run::FakeRunPort;
