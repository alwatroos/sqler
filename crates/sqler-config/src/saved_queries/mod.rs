// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Saved queries and their user-editable `queries.toml` format (ADR-0021).

mod file;
mod model;

pub use model::{
    SAVED_QUERIES_FORMAT_VERSION, SavedQueries, SavedQuery, SavedQueryDraft, SavedQueryError,
    SavedQueryId,
};

use std::path::PathBuf;

use crate::fs::{read_optional, write_atomic};
use crate::{ConfigDir, ConfigError};

impl ConfigDir {
    /// Path of `queries.toml`.
    #[must_use]
    pub fn saved_queries_path(&self) -> PathBuf {
        self.root().join("queries.toml")
    }

    /// Loads `queries.toml` as an editable [`SavedQueries`]; a missing file
    /// yields an empty list. The file is never rewritten or discarded here.
    ///
    /// # Errors
    /// [`ConfigError::Read`]; [`ConfigError::Parse`] for malformed TOML, wrong
    /// types or unknown keys (the message gives line and column, never the
    /// source text); [`ConfigError::Invalid`] naming the entry (index and
    /// name) with a bad value, a duplicate id or name, a missing `version`, or
    /// a `version` newer than [`SAVED_QUERIES_FORMAT_VERSION`].
    pub fn load_saved_queries(&self) -> Result<SavedQueries, ConfigError> {
        let path = self.saved_queries_path();
        let Some(text) = read_optional(&path)? else {
            return Ok(SavedQueries::new());
        };
        file::parse(&path, &text)
    }

    /// Writes the list to `queries.toml` as `version = 1`, creating the
    /// directory when missing. The write is atomic: a temporary file is
    /// flushed and renamed over the target.
    ///
    /// # Errors
    /// [`ConfigError::Serialize`] or [`ConfigError::Write`].
    pub fn save_saved_queries(&self, queries: &SavedQueries) -> Result<(), ConfigError> {
        let path = self.saved_queries_path();
        let text = file::serialize(&path, queries)?;
        write_atomic(&path, &text)
    }
}

/// Describes a parse error by line, column and message only. The `toml` Display
/// output quotes the offending source line.
fn parse_message(text: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("line {line}, column {col}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}
