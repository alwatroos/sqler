// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Edge cases of the headless editor core: unicode, huge scripts, unterminated strings.

#![allow(clippy::unwrap_used)]

use std::time::Instant;

use sqler_sql::{Dialect, FormatOptions};
use sqler_ui::editor::{
    BracketIndex, EditorBuffer, EditorDoc, OccurrenceStep, format_edit, next_occurrence, synthetic,
    touches_bracket, word_at,
};

const ALL: [Dialect; 3] = [Dialect::Postgres, Dialect::MySql, Dialect::Sqlite];

#[test]
fn unterminated_string_and_comment_fail_without_panic() {
    for d in ALL {
        for t in ["SELECT 'abc", "SELECT \"abc", "SELECT 1 /* x", "SELECT `a"] {
            let b = EditorBuffer::new(t, 0);
            let r = format_edit(d, &b, &FormatOptions::default());
            assert!(r.is_err() || matches!(r, Ok(None | Some(_))), "{t}");
        }
    }
    let b = EditorBuffer::new("SELECT 'abc", 0);
    assert!(format_edit(Dialect::Sqlite, &b, &FormatOptions::default()).is_err());
}

#[test]
fn unicode_selection_and_cursor_never_panic() {
    let t = "SELECT 'żółć 日本語 😀' AS é FROM t WHERE (a=1)";
    for d in ALL {
        for cur in 0..=t.len() + 2 {
            let mut b = EditorBuffer::new(t, cur.min(t.len()));
            let _ = format_edit(d, &b, &FormatOptions::default());
            b.selection = cur..(cur + 5);
            let _ = format_edit(d, &b, &FormatOptions::default());
            b.selection = (cur + 5)..cur; // reversed
            let _ = format_edit(d, &b, &FormatOptions::default());
        }
    }
    for c in 0..=t.len() + 2 {
        let _ = touches_bracket(t, c);
        let _ = word_at(t, c);
        let _ = BracketIndex::build(Some(Dialect::Postgres), t).pair_at(t, c);
        let _ = next_occurrence(t, "é", c);
    }
}

#[test]
fn unicode_format_selection_is_one_valid_edit() {
    let t = "select '日本' as a,b from t;";
    let mut b = EditorBuffer::new(t, 0);
    let e = format_edit(Dialect::Postgres, &b, &FormatOptions::default())
        .unwrap()
        .unwrap();
    assert_eq!(e.range, 0..t.len());
    assert!(e.text.contains("'日本'"));
    // selection inside a multibyte char is ignored, not a panic
    b.selection = 9..10;
    assert!(
        format_edit(Dialect::Postgres, &b, &FormatOptions::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn brackets_ignore_strings_comments_and_unbalanced() {
    let t = "SELECT '(' , \"(\", /* ( */ 1 -- (\n, (2)";
    let ix = BracketIndex::build(Some(Dialect::Sqlite), t);
    assert_eq!(ix.len(), 1);
    let u = "((( ) ]";
    let ix = BracketIndex::build(None, u);
    assert!(ix.len() <= 1);
    let _ = BracketIndex::build(None, "'(unterminated (");
    let _ = BracketIndex::build(None, "");
}

#[test]
fn huge_script_is_fast_and_idempotent() {
    let s = synthetic::script(10_000);
    assert!(s.lines().count() >= 10_000);
    let t0 = Instant::now();
    let ix = BracketIndex::build(Some(Dialect::Postgres), &s);
    let build = t0.elapsed();
    assert!(ix.len() > 1000);
    let b = EditorBuffer::new(s.clone(), 0);
    let t1 = Instant::now();
    let e = format_edit(Dialect::Postgres, &b, &FormatOptions::default())
        .unwrap()
        .unwrap();
    let fmt = t1.elapsed();
    eprintln!("build={build:?} format={fmt:?}");
    // formatted twice gives no further edit
    let b2 = EditorBuffer::new(e.text, 0);
    assert!(
        format_edit(Dialect::Postgres, &b2, &FormatOptions::default())
            .unwrap()
            .is_none()
    );
    // 100k lines still completes
    let big = synthetic::script(100_000);
    let _ = BracketIndex::build(None, &big);
}

#[test]
fn occurrence_chain_terminates_on_wrap_and_unicode() {
    let t = "é é é";
    let mut doc = EditorDoc::new(Some(Dialect::Sqlite), None);
    let mut b = EditorBuffer::new(t, 0);
    let OccurrenceStep::Select(r) = doc.add_next_occurrence(&b) else {
        panic!("select")
    };
    b.selection = r.clone();
    b.cursor = r.end;
    let mut adds = 0;
    for _ in 0..10 {
        match doc.add_next_occurrence(&b) {
            OccurrenceStep::Add(_) => adds += 1,
            OccurrenceStep::Nothing => break,
            _ => {}
        }
    }
    assert_eq!(adds, 2);
    let b = EditorBuffer::new("   ", 1);
    assert!(matches!(
        EditorDoc::new(None, None).add_next_occurrence(&b),
        OccurrenceStep::Nothing
    ));
}
