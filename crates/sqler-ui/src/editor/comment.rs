// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! "Toggle Line Comment" and "Toggle Block Comment": each turns the buffer into one text edit (one undo step).

use std::ops::Range;

use super::{EditorBuffer, TextEdit};

/// One change to the old text: `removed` bytes at `pos` were replaced by `inserted` bytes.
struct Op {
    pos: usize,
    removed: usize,
    inserted: usize,
}

/// Maps an old offset through the changes (ascending, non-overlapping): an offset at or after an insertion point
/// moves by its length; an offset inside a removed range clamps to its start.
fn map_offset(ops: &[Op], offset: usize) -> usize {
    let mut shift: isize = 0;
    for op in ops {
        if offset >= op.pos + op.removed {
            shift += op.inserted.cast_signed() - op.removed.cast_signed();
        } else if offset > op.pos {
            return op.pos.saturating_add_signed(shift) + op.inserted;
        } else {
            break;
        }
    }
    offset.saturating_add_signed(shift)
}

fn valid_selection(text: &str, sel: &Range<usize>) -> bool {
    sel.start <= sel.end
        && sel.end <= text.len()
        && text.is_char_boundary(sel.start)
        && text.is_char_boundary(sel.end)
}

/// Toggles a `-- ` comment on the cursor's line, or on every line of the selection.
///
/// When every non-blank line is already commented the comments are removed, otherwise all non-blank lines are
/// commented at the smallest indentation. Blank lines are left alone. `None` when nothing changes or the
/// selection is out of range.
#[must_use]
pub fn toggle_line_comment(b: &EditorBuffer) -> Option<TextEdit> {
    let text = &b.text;
    let sel = &b.selection;
    if !valid_selection(text, sel) {
        return None;
    }
    let first_start = text[..sel.start].rfind('\n').map_or(0, |i| i + 1);
    // A selection that ends at column 0 of a later line does not include that line.
    let anchor = if sel.end > sel.start && text[..sel.end].ends_with('\n') {
        sel.end - 1
    } else {
        sel.end
    };
    let last_end = text[anchor..].find('\n').map_or(text.len(), |i| anchor + i);
    if last_end < first_start {
        return None;
    }

    // (start offset, line without its line break, line break)
    let mut lines: Vec<(usize, &str, &str)> = Vec::new();
    let mut at = first_start;
    for raw in text[first_start..last_end].split('\n') {
        let (body, cr) = raw
            .strip_suffix('\r')
            .map_or((raw, ""), |body| (body, "\r"));
        lines.push((at, body, cr));
        at += raw.len() + 1;
    }
    let indent_of = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
    let non_blank: Vec<&(usize, &str, &str)> = lines
        .iter()
        .filter(|(_, l, _)| !l.trim().is_empty())
        .collect();
    if non_blank.is_empty() {
        return None;
    }
    let uncomment = non_blank
        .iter()
        .all(|(_, l, _)| l[indent_of(l)..].starts_with("--"));
    let min_indent = non_blank
        .iter()
        .map(|(_, l, _)| indent_of(l))
        .min()
        .unwrap_or(0);

    let mut out = String::new();
    let mut ops = Vec::new();
    for (i, (start, body, cr)) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if body.trim().is_empty() {
            out.push_str(body);
        } else if uncomment {
            let ind = indent_of(body);
            let rest = &body[ind + 2..];
            let removed = if rest.starts_with(' ') { 3 } else { 2 };
            out.push_str(&body[..ind]);
            out.push_str(&body[ind + removed..]);
            ops.push(Op {
                pos: start + ind,
                removed,
                inserted: 0,
            });
        } else {
            out.push_str(&body[..min_indent]);
            out.push_str("-- ");
            out.push_str(&body[min_indent..]);
            ops.push(Op {
                pos: start + min_indent,
                removed: 0,
                inserted: 3,
            });
        }
        out.push_str(cr);
    }
    if out == text[first_start..last_end] {
        return None;
    }
    let select_after = map_offset(&ops, sel.start)..map_offset(&ops, sel.end);
    Some(TextEdit {
        range: first_start..last_end,
        text: out,
        select_after,
    })
}

/// Toggles a `/* */` comment around the selection (inserts an empty one at the cursor).
///
/// `None` when the selection is out of range.
#[must_use]
pub fn toggle_block_comment(b: &EditorBuffer) -> Option<TextEdit> {
    let text = &b.text;
    let sel = &b.selection;
    if !valid_selection(text, sel) {
        return None;
    }
    if sel.is_empty() {
        return Some(TextEdit {
            range: sel.clone(),
            text: "/*  */".to_owned(),
            select_after: sel.start + 3..sel.start + 3,
        });
    }
    let selected = &text[sel.clone()];
    let trimmed = selected.trim();
    // Unwrap only when the whole selection is one comment: the first `*/` is the last two bytes.
    let one_comment = trimmed.len() >= 4
        && trimmed.starts_with("/*")
        && trimmed[2..].find("*/") == Some(trimmed.len() - 4);
    if one_comment {
        let lead = selected.len() - selected.trim_start().len();
        let start = sel.start + lead;
        let mut inner = &trimmed[2..trimmed.len() - 2];
        inner = inner.strip_prefix(' ').unwrap_or(inner);
        inner = inner.strip_suffix(' ').unwrap_or(inner);
        return Some(TextEdit {
            range: start..start + trimmed.len(),
            text: inner.to_owned(),
            select_after: start..start + inner.len(),
        });
    }
    Some(TextEdit {
        range: sel.clone(),
        text: format!("/* {selected} */"),
        select_after: sel.start + 3..sel.start + 3 + selected.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str, sel: Range<usize>) -> EditorBuffer {
        EditorBuffer {
            text: text.to_owned(),
            cursor: sel.end,
            selection: sel,
        }
    }

    fn apply(b: &EditorBuffer, e: &TextEdit) -> String {
        let mut t = b.text.clone();
        t.replace_range(e.range.clone(), &e.text);
        t
    }

    fn line(text: &str, sel: Range<usize>) -> Option<(String, Range<usize>)> {
        let b = buf(text, sel);
        toggle_line_comment(&b).map(|e| (apply(&b, &e), e.select_after))
    }

    #[test]
    fn comments_the_cursor_line_only() {
        let (t, s) = line("select 1;\nselect 2;", 12..12).unwrap();
        assert_eq!(t, "select 1;\n-- select 2;");
        assert_eq!(s, 15..15);
    }

    #[test]
    fn uncomments_and_maps_the_cursor() {
        let (t, s) = line("-- select 1", 5..5).unwrap();
        assert_eq!(t, "select 1");
        assert_eq!(s, 2..2);
        let (t, _) = line("--select 1", 0..0).unwrap();
        assert_eq!(t, "select 1");
    }

    #[test]
    fn selection_comments_all_lines_and_is_kept() {
        let (t, s) = line("a\nb\nc", 0..5).unwrap();
        assert_eq!(t, "-- a\n-- b\n-- c");
        assert_eq!(s, 3..14);
        let (t, s) = line("-- a\n-- b\nc", 0..4).unwrap();
        assert_eq!(t, "a\n-- b\nc");
        assert_eq!(s, 0..1);
    }

    #[test]
    fn mixed_lines_are_all_commented() {
        let (t, _) = line("-- a\nb", 0..6).unwrap();
        assert_eq!(t, "-- -- a\n-- b");
    }

    #[test]
    fn all_commented_lines_are_uncommented() {
        let (t, _) = line("-- a\n-- b", 0..9).unwrap();
        assert_eq!(t, "a\nb");
    }

    #[test]
    fn indentation_is_kept_at_the_minimum_column() {
        let (t, _) = line("    a\n  b\n    c", 0..15).unwrap();
        assert_eq!(t, "  --   a\n  -- b\n  --   c");
        let (t, _) = line("  -- a\n    -- b", 0..14).unwrap();
        assert_eq!(t, "  a\n    b");
    }

    #[test]
    fn blank_lines_are_untouched() {
        let (t, _) = line("a\n\n  \nb", 0..7).unwrap();
        assert_eq!(t, "-- a\n\n  \n-- b");
        let (t, _) = line("-- a\n\n-- b", 0..10).unwrap();
        assert_eq!(t, "a\n\nb");
        assert_eq!(line("\n\n", 0..2), None);
        assert_eq!(line("", 0..0), None);
    }

    #[test]
    fn selection_ending_at_column_zero_excludes_that_line() {
        let (t, _) = line("a\nb\nc", 0..4).unwrap();
        assert_eq!(t, "-- a\n-- b\nc");
    }

    #[test]
    fn it_is_one_edit_covering_only_the_lines() {
        let b = buf("x\ny\nz", 2..2);
        let e = toggle_line_comment(&b).unwrap();
        assert_eq!(e.range, 2..3);
        assert_eq!(e.text, "-- y");
    }

    #[test]
    fn crlf_text_keeps_its_line_breaks() {
        let (t, _) = line("a\r\nb\r\nc", 0..7).unwrap();
        assert_eq!(t, "-- a\r\n-- b\r\n-- c");
        let (t, _) = line("-- a\r\n-- b", 0..10).unwrap();
        assert_eq!(t, "a\r\nb");
    }

    #[test]
    fn out_of_range_selection_is_none() {
        assert_eq!(line("abc", 2..9), None);
        assert_eq!(line("é", 1..1), None);
    }

    #[test]
    fn block_wraps_and_unwraps() {
        let b = buf("select 1", 0..8);
        let e = toggle_block_comment(&b).unwrap();
        assert_eq!(apply(&b, &e), "/* select 1 */");
        assert_eq!(e.select_after, 3..11);
        let b = buf("/* select 1 */", 0..14);
        let e = toggle_block_comment(&b).unwrap();
        assert_eq!(apply(&b, &e), "select 1");
        assert_eq!(e.select_after, 0..8);
        let b = buf("/*x*/", 0..5);
        let e = toggle_block_comment(&b).unwrap();
        assert_eq!(apply(&b, &e), "x");
    }

    #[test]
    fn block_unwrap_ignores_surrounding_whitespace_in_the_selection() {
        let b = buf("a /* b */ c", 1..10);
        let e = toggle_block_comment(&b).unwrap();
        assert_eq!(apply(&b, &e), "a b c");
        assert_eq!(e.select_after, 2..3);
    }

    #[test]
    fn block_unwrap_needs_one_comment_for_the_whole_selection() {
        let b = buf("/* a */ b /* c */", 0..17);
        let e = toggle_block_comment(&b).unwrap();
        assert_eq!(apply(&b, &e), "/* /* a */ b /* c */ */");
    }

    #[test]
    fn block_with_empty_selection_inserts_a_pair() {
        let b = buf("ab", 1..1);
        let e = toggle_block_comment(&b).unwrap();
        assert_eq!(apply(&b, &e), "a/*  */b");
        assert_eq!(e.select_after, 4..4);
    }

    #[test]
    fn block_out_of_range_is_none() {
        assert_eq!(toggle_block_comment(&buf("abc", 1..9)), None);
    }
}
