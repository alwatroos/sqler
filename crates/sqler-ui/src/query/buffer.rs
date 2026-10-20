// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The abstract editor buffer the run commands act on.

use std::ops::Range;

/// A snapshot of an editor buffer: text, selection and cursor.
///
/// Offsets are UTF-8 byte offsets on char boundaries. An empty `selection` means no selection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BufferSnapshot {
    /// The whole buffer text.
    pub text: String,
    /// The selected byte range; empty when nothing is selected.
    pub selection: Range<usize>,
    /// The cursor byte offset.
    pub cursor: usize,
}

impl BufferSnapshot {
    /// A snapshot with no selection and the cursor at `cursor`.
    #[must_use]
    pub fn at_cursor(text: impl Into<String>, cursor: usize) -> Self {
        Self {
            text: text.into(),
            selection: cursor..cursor,
            cursor,
        }
    }

    /// A snapshot with `selection` selected and the cursor at its end.
    #[must_use]
    pub fn with_selection(text: impl Into<String>, selection: Range<usize>) -> Self {
        let cursor = selection.end;
        Self {
            text: text.into(),
            selection,
            cursor,
        }
    }
}

/// Which part of the buffer a run covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunMode {
    /// The selected text (the statement at the cursor when nothing is selected).
    Selection,
    /// The statement under the cursor.
    Statement,
    /// Every statement of the buffer, in order.
    All,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors() {
        let b = BufferSnapshot::at_cursor("select 1", 3);
        assert_eq!(b.selection, 3..3);
        assert_eq!(b.cursor, 3);
        let b = BufferSnapshot::with_selection("select 1", 0..6);
        assert_eq!(b.selection, 0..6);
        assert_eq!(b.cursor, 6);
    }
}
