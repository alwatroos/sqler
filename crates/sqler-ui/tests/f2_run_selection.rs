// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Independent checks for F2: exact selection runs, blank lines inside one statement,
//! comment toggles.

#![allow(clippy::unwrap_used)]

use sqler_sql::{Dialect, StatementClass};
use sqler_ui::editor::{EditorBuffer, TextEdit, toggle_block_comment, toggle_line_comment};
use sqler_ui::query::{BufferSnapshot, RunMode, plan};

fn sqls(p: &sqler_ui::query::Plan) -> Vec<String> {
    p.statements.iter().map(|s| s.sql.clone()).collect()
}

fn apply(b: &EditorBuffer, e: &TextEdit) -> String {
    let mut t = b.text.clone();
    t.replace_range(e.range.clone(), &e.text);
    t
}

#[test]
fn selection_runs_exactly_the_selection_in_every_dialect() {
    let text = "select 1; select 22 from t where a = 1; select 3";
    for d in Dialect::ALL {
        // partial line: a fragment in the middle of the second statement
        let from = text.find("22").unwrap();
        let to = from + "22 from t".len();
        for mode in [RunMode::Selection, RunMode::Statement] {
            let b = BufferSnapshot::with_selection(text, from..to);
            let p = plan(Some(d), &b, mode).unwrap();
            assert_eq!(sqls(&p), ["22 from t"], "{d:?} {mode:?}");
            // multi-statement selection runs both, nothing else
            let start = text.find("select 22").unwrap();
            let end = text.find(" select 3").unwrap();
            let b = BufferSnapshot::with_selection(text, start..end);
            let p = plan(Some(d), &b, mode).unwrap();
            assert_eq!(sqls(&p), ["select 22 from t where a = 1"], "{d:?}");
            let b = BufferSnapshot::with_selection(text, 0..end);
            let p = plan(Some(d), &b, mode).unwrap();
            assert_eq!(sqls(&p).len(), 2, "{d:?}");
        }
    }
}

#[test]
fn blank_lines_do_not_split_without_semicolons() {
    let text = "select * from a\nwhere x = 1\n\nselect * from b\n\ndelete from c";
    for d in Dialect::ALL {
        for pos in [3, text.find("from b").unwrap(), text.len()] {
            let b = BufferSnapshot::at_cursor(text, pos);
            let p = plan(Some(d), &b, RunMode::Statement).unwrap();
            assert_eq!(sqls(&p), [text], "{d:?} {pos}");
        }
    }
}

#[test]
fn blank_lines_inside_constructs_do_not_split() {
    for d in Dialect::ALL {
        for text in [
            "select 1 union\n\nselect 2",
            "select 'a\n\nb'",
            "select (1,\n\n2)",
            "select 1 /* x\n\ny */ from t",
        ] {
            let b = BufferSnapshot::at_cursor(text, 1);
            let p = plan(Some(d), &b, RunMode::All).unwrap();
            assert_eq!(p.statements.len(), 1, "{d:?} {text:?}");
        }
    }
}

#[test]
fn blank_line_joined_write_is_classified_as_write() {
    let b = BufferSnapshot::at_cursor("select 1\n\ndelete from t", 0);
    let p = plan(Some(Dialect::Postgres), &b, RunMode::All).unwrap();
    assert_eq!(p.statements.len(), 1);
    assert_eq!(p.statements[0].class, StatementClass::Write);
}

#[test]
fn comment_toggle_cycle() {
    // indentation + selection kept, mixed comments, uncomment
    let mut b = EditorBuffer::new("  a\n    b\n\n  c", 0);
    b.selection = 0..b.text.len();
    let e = toggle_line_comment(&b).unwrap();
    let t = apply(&b, &e);
    assert_eq!(t, "  -- a\n  --   b\n\n  -- c");
    let mut second = EditorBuffer::new(t.clone(), 0);
    second.selection = e.select_after.clone();
    assert_eq!(
        &t[e.select_after.clone()].trim(),
        &"-- a\n  --   b\n\n  -- c"
    );
    let undo = toggle_line_comment(&second).unwrap();
    assert_eq!(apply(&second, &undo), "  a\n    b\n\n  c");
    // mixed: comments
    let mut mixed = EditorBuffer::new("-- a\nb", 0);
    mixed.selection = 0..mixed.text.len();
    assert_eq!(
        apply(&mixed, &toggle_line_comment(&mixed).unwrap()),
        "-- -- a\n-- b"
    );
    // cursor-only line
    let line = EditorBuffer::new("x\ny", 3);
    assert_eq!(
        apply(&line, &toggle_line_comment(&line).unwrap()),
        "x\n-- y"
    );
    // block
    let mut block = EditorBuffer::new("select 1", 0);
    block.selection = 0..6;
    assert_eq!(
        apply(&block, &toggle_block_comment(&block).unwrap()),
        "/* select */ 1"
    );
}

#[test]
fn round2_selection_with_blank_line_is_one_statement() {
    let text = "insert into t (a)\n\nselect 1";
    for d in Dialect::ALL {
        for mode in [RunMode::Selection, RunMode::Statement] {
            let b = BufferSnapshot::with_selection(text, 0..text.len());
            let p = plan(Some(d), &b, mode).unwrap();
            assert_eq!(sqls(&p), [text], "{d:?} {mode:?}");
        }
        // cursor, no selection: a blank line does not end a statement
        let t2 = "select 1\n\nselect 2\n\nselect 3";
        let b = BufferSnapshot::at_cursor(t2, t2.find('2').unwrap());
        let p = plan(Some(d), &b, RunMode::Statement).unwrap();
        assert_eq!(sqls(&p), [t2], "{d:?}");
    }
}

#[test]
fn round2_tsql_if_body_is_not_split() {
    for text in [
        "IF @x = 1\n\nSELECT 1",
        "IF @x = 1\n\nSELECT 1\n\nELSE\n\nSELECT 2",
        "WHILE @i < 3\n\nBEGIN\n\nSET @i = @i + 1\n\nEND",
    ] {
        let b = BufferSnapshot::at_cursor(text, text.len());
        let p = plan(Some(Dialect::TSql), &b, RunMode::All).unwrap();
        assert_eq!(p.statements.len(), 1, "{text:?} -> {:?}", sqls(&p));
    }
}

#[test]
fn round2_block_comment_toggle_edges() {
    let mut b = EditorBuffer::new("/* a */ b /* c */", 0);
    b.selection = 0..17;
    assert_eq!(
        apply(&b, &toggle_block_comment(&b).unwrap()),
        "/* /* a */ b /* c */ */"
    );
    let mut b = EditorBuffer::new("/* a */", 0);
    b.selection = 0..7;
    assert_eq!(apply(&b, &toggle_block_comment(&b).unwrap()), "a");
    let mut b = EditorBuffer::new("/**/", 0);
    b.selection = 0..4;
    assert_eq!(apply(&b, &toggle_block_comment(&b).unwrap()), "");
    let mut b = EditorBuffer::new("/*/", 0);
    b.selection = 0..3;
    assert_eq!(apply(&b, &toggle_block_comment(&b).unwrap()), "/* /*/ */");
    let mut b = EditorBuffer::new("é /* x */", 0);
    b.selection = 3..10;
    assert_eq!(apply(&b, &toggle_block_comment(&b).unwrap()), "é x");
}
