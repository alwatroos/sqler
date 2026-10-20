// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! "Format SQL": turns the formatter's output into one text edit.

use std::fmt;
use std::ops::Range;

use sqler_sql::{Dialect, FormatError, FormatOptions};

use super::{EditorBuffer, TextEdit};

/// Why the text could not be formatted; the message is meant for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatFailure {
    /// What to tell the user.
    pub message: String,
}

impl fmt::Display for FormatFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FormatFailure {}

/// 1-based line and column (in chars) of a byte offset.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let head = text.get(..offset).unwrap_or(text);
    let line = head.matches('\n').count() + 1;
    let col = head.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

fn failure(text: &str, base: usize, e: &FormatError) -> FormatFailure {
    let message = match e {
        FormatError::Unterminated { offset } => {
            let (line, col) = line_col(text, base + offset);
            format!(
                "Cannot format: unterminated string, quoted identifier or comment at line {line}, column {col}"
            )
        }
        other => format!("Cannot format: {other}"),
    };
    FormatFailure { message }
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn valid_selection(text: &str, sel: &Range<usize>) -> bool {
    sel.start <= sel.end
        && sel.end <= text.len()
        && text.is_char_boundary(sel.start)
        && text.is_char_boundary(sel.end)
}

/// The edit that formats the selection, or the whole buffer when the selection is empty.
///
/// `Ok(None)` means there is nothing to change (already formatted, blank text, or a selection that is out of
/// range), so no undo step should be recorded.
///
/// # Errors
///
/// [`FormatFailure`] when a string, quoted identifier or comment is not closed.
pub fn format_edit(
    d: Dialect,
    b: &EditorBuffer,
    o: &FormatOptions,
) -> Result<Option<TextEdit>, FormatFailure> {
    if !valid_selection(&b.text, &b.selection) {
        return Ok(None);
    }
    if b.selection.is_empty() {
        return whole(d, b, o);
    }
    let old = &b.text[b.selection.clone()];
    let mut new =
        sqler_sql::format(d, old, o).map_err(|e| failure(&b.text, b.selection.start, &e))?;
    if !old.ends_with('\n') && new.ends_with('\n') {
        new.pop();
    }
    if new.trim().is_empty() || new == old {
        return Ok(None);
    }
    let end = b.selection.start + new.len();
    Ok(Some(TextEdit {
        range: b.selection.clone(),
        select_after: b.selection.start..end,
        text: new,
    }))
}

fn whole(
    d: Dialect,
    b: &EditorBuffer,
    o: &FormatOptions,
) -> Result<Option<TextEdit>, FormatFailure> {
    let new = sqler_sql::format(d, &b.text, o).map_err(|e| failure(&b.text, 0, &e))?;
    if new.trim().is_empty() || new == b.text {
        return Ok(None);
    }
    let at = floor_boundary(&new, b.cursor);
    Ok(Some(TextEdit {
        range: 0..b.text.len(),
        select_after: at..at,
        text: new,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: Dialect = Dialect::Postgres;

    fn buf(text: &str, sel: Range<usize>) -> EditorBuffer {
        EditorBuffer {
            text: text.to_owned(),
            cursor: sel.end,
            selection: sel,
        }
    }

    fn run(text: &str, sel: Range<usize>) -> Result<Option<TextEdit>, FormatFailure> {
        format_edit(PG, &buf(text, sel), &FormatOptions::default())
    }

    #[test]
    fn whole_buffer_when_the_selection_is_empty() {
        let e = run("select a from t where x = 1", 0..0).unwrap().unwrap();
        assert_eq!(e.range, 0..27);
        assert_eq!(e.text, "SELECT a\nFROM t\nWHERE x = 1\n");
        assert_eq!(e.select_after, 0..0);
    }

    #[test]
    fn cursor_is_kept_and_clamped() {
        let text = "select a from t where x = 1";
        let e = run(text, 7..7).unwrap().unwrap();
        assert_eq!(e.select_after, 7..7);
        let short = format_edit(
            PG,
            &EditorBuffer {
                text: "select   1".to_owned(),
                selection: 10..10,
                cursor: 10,
            },
            &FormatOptions::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(short.text, "SELECT 1\n");
        assert_eq!(short.select_after, 9..9);
    }

    #[test]
    fn selection_only_and_no_trailing_newline() {
        let text = "select 1;\nselect a from t where x = 1;\nselect 3;";
        let start = text.find("select a").unwrap();
        let end = start + "select a from t where x = 1".len();
        let e = run(text, start..end).unwrap().unwrap();
        assert_eq!(e.range, start..end);
        assert_eq!(e.text, "SELECT a\nFROM t\nWHERE x = 1");
        assert_eq!(e.select_after, start..start + e.text.len());
    }

    #[test]
    fn selection_keeps_a_trailing_newline_it_had() {
        let text = "select   1\n";
        let e = run(text, 0..text.len()).unwrap().unwrap();
        assert_eq!(e.text, "SELECT 1\n");
    }

    #[test]
    fn unchanged_text_is_none() {
        assert_eq!(run("SELECT 1\n", 0..0), Ok(None));
        assert_eq!(run("SELECT 1", 0..8), Ok(None));
    }

    #[test]
    fn blank_text_is_none() {
        assert_eq!(run("", 0..0), Ok(None));
        assert_eq!(run("  \n ", 0..0), Ok(None));
        assert_eq!(run("a   \n  ", 1..4), Ok(None));
    }

    #[test]
    fn bad_selection_is_none() {
        assert_eq!(run("select 1", 0..99), Ok(None));
        assert_eq!(run("żółć", 1..3), Ok(None));
    }

    #[test]
    fn unterminated_reports_line_and_column() {
        let e = run("select 1;\nselect 'oops from t", 0..0).unwrap_err();
        assert_eq!(
            e.message,
            "Cannot format: unterminated string, quoted identifier or comment at line 2, column 8"
        );
        assert_eq!(e.to_string(), e.message);
    }

    #[test]
    fn unterminated_in_a_selection_is_absolute() {
        let text = "select 1;\nselect 'oops from t";
        let e = run(text, 10..text.len()).unwrap_err();
        assert!(e.message.ends_with("line 2, column 8"), "{e}");
    }

    #[test]
    fn column_counts_chars_not_bytes() {
        let e = run("select 'żółć', 'oops", 0..0).unwrap_err();
        assert!(e.message.ends_with("line 1, column 16"), "{e}");
    }

    #[test]
    fn multibyte_text_formats() {
        let e = run("select 'żółć' from t", 0..0).unwrap().unwrap();
        assert_eq!(e.text, "SELECT 'żółć'\nFROM t\n");
    }
}
