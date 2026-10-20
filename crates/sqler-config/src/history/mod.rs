// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Query history in `history.jsonl` (ADR-0021): one JSON object per line,
//! appended per executed query and compacted to a bounded size.
//!
//! Privacy: history holds the SQL text as typed, including any literals in it.
//! Credentials in recognised statements are redacted
//! ([`redact_credentials`]); bound parameters and error messages are never
//! recorded.

mod entry;
mod file;
mod redact;
mod store;

use std::path::PathBuf;

pub use entry::{
    DEFAULT_HISTORY_CAP, HISTORY_FORMAT_VERSION, HistoryEntry, HistoryIssue, HistoryIssueKind,
    HistoryOptions, HistoryOutcome, HistoryQuery, MAX_HISTORY_SQL_BYTES, NewHistoryEntry,
};
pub(crate) use file::valid_connection_id;
pub use redact::redact_credentials;
pub use store::HistoryStore;

use crate::{ConfigDir, ConfigError};

impl ConfigDir {
    /// Path of `history.jsonl`.
    #[must_use]
    pub fn history_path(&self) -> PathBuf {
        self.root().join("history.jsonl")
    }

    /// Opens the query history in this directory; see [`HistoryStore::open`].
    ///
    /// # Errors
    /// [`ConfigError::Read`] when the file exists but cannot be read.
    pub fn open_history(&self, options: HistoryOptions) -> Result<HistoryStore, ConfigError> {
        HistoryStore::open(self.history_path(), options)
    }
}
