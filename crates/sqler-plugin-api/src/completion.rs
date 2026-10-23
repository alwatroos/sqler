// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plain data for plugin completion providers: the request the editor sends and the items a plugin answers with.
//!
//! The host cleans and caps what a plugin returns against the limits below; the UI merges the items after its
//! built-in ones.

/// The most items kept from one plugin answer, after cleaning.
pub const MAX_COMPLETION_ITEMS: usize = 50;
/// The longest label, in bytes.
pub const MAX_COMPLETION_LABEL_BYTES: usize = 128;
/// The longest detail, in bytes.
pub const MAX_COMPLETION_DETAIL_BYTES: usize = 256;
/// The longest insert text, in bytes.
pub const MAX_COMPLETION_INSERT_BYTES: usize = 1024;
/// The longest statement a request may carry, in bytes.
pub const MAX_COMPLETION_STATEMENT_BYTES: usize = 64 * 1024;

/// What the editor asks a completion plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    /// The engine id of the tab's connection, when there is one.
    pub driver: Option<String>,
    /// The statement under the cursor.
    pub statement: String,
    /// The cursor as a byte offset into `statement` (on a char boundary).
    pub cursor: usize,
}

/// What kind of thing an item completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompletionKind {
    /// A SQL keyword.
    Keyword,
    /// A table or view.
    Table,
    /// A column.
    Column,
    /// A function.
    Function,
    /// A snippet.
    Snippet,
    /// Anything else.
    Other,
}

/// One suggestion from a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// The text shown in the popup.
    pub label: String,
    /// What the item is.
    pub kind: CompletionKind,
    /// The text inserted on accept (one line).
    pub insert_text: String,
    /// A short note shown next to the label.
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caps_are_consistent() {
        const {
            assert!(MAX_COMPLETION_STATEMENT_BYTES < u32::MAX as usize);
            assert!(MAX_COMPLETION_LABEL_BYTES <= MAX_COMPLETION_INSERT_BYTES);
            assert!(MAX_COMPLETION_ITEMS > 0);
            assert!(MAX_COMPLETION_DETAIL_BYTES > 0);
        }
    }
}
