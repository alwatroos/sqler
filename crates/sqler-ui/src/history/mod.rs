// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Query history and saved queries.
//!
//! Everything here is plain Rust and headless, like [`crate::query`]:
//!
//! - [`history_entry`] turns the terminal [`RunEvent`](crate::query::RunEvent) of a statement
//!   into the [`NewHistoryEntry`](sqler_config::NewHistoryEntry) the store records. The SQL is the
//!   statement text before binding, so bound values never reach the store, and error messages are
//!   dropped.
//! - [`HistoryTab`], [`HistoryRow`], [`SavedState`], [`HistoryDialog`] and [`SavedPicker`] are the
//!   state of the history tab, the saved-queries list and their dialogs. The workspace owns them.
//! - [`HistoryService`] is a blocking facade over the store and `queries.toml`. Its methods lock
//!   files and do I/O, so the view calls them on a blocking thread, never on the UI thread.
//! - [`HistoryCommand`] lists the commands.

mod action;
mod model;
mod record;
mod service;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use action::{COMMANDS, HistoryCommand, action_for, register_commands};
pub use model::{
    HISTORY_PAGE, HistoryDialog, HistoryRow, HistoryScope, HistorySearch, HistoryTab, NamePurpose,
    SavedLoad, SavedPicker, SavedState, history_row,
};
pub use record::history_entry;
pub use service::HistoryService;
