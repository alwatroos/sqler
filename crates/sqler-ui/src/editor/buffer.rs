// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The editor text as the headless logic sees it.

use std::ops::Range;

/// A snapshot of the editor: the text, the active selection and the cursor.
///
/// All offsets are byte offsets on char boundaries. Only the active selection is visible (the editor
/// exposes no other); an empty selection sits at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorBuffer {
    /// The whole text.
    pub text: String,
    /// The active selection; empty when nothing is selected.
    pub selection: Range<usize>,
    /// The cursor (the caret of the active selection).
    pub cursor: usize,
}

impl EditorBuffer {
    /// A buffer with the cursor at `cursor` and nothing selected.
    #[must_use]
    pub fn new(text: impl Into<String>, cursor: usize) -> Self {
        Self {
            text: text.into(),
            selection: cursor..cursor,
            cursor,
        }
    }

    /// The selected text, or `None` if the selection is empty or not on char boundaries.
    #[must_use]
    pub fn selected_text(&self) -> Option<&str> {
        if self.selection.is_empty() {
            return None;
        }
        self.text.get(self.selection.clone())
    }
}

/// A replacement the view applies as one undo step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    /// The range to replace.
    pub range: Range<usize>,
    /// The new text.
    pub text: String,
    /// The selection after the edit, in the coordinates of the new text.
    pub select_after: Range<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_an_empty_selection_at_the_cursor() {
        let b = EditorBuffer::new("select 1", 3);
        assert_eq!(b.selection, 3..3);
        assert_eq!(b.cursor, 3);
        assert_eq!(b.selected_text(), None);
    }

    #[test]
    fn selected_text_reads_the_selection() {
        let mut b = EditorBuffer::new("select 1", 8);
        b.selection = 0..6;
        assert_eq!(b.selected_text(), Some("select"));
        b.selection = 0..99;
        assert_eq!(b.selected_text(), None);
    }
}
