// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of splitting, cursor lookup and placeholders, and hostile inputs.

use proptest::prelude::*;
use sqler_sql::{
    Dialect, Segment, SegmentKind, SplitOptions, placeholders, split, split_with, statement_at,
};

const FRAGMENTS: &[&str] = &[
    ";",
    "'",
    "''",
    "\"",
    "`",
    "$$",
    "$a$",
    "$1",
    "?",
    ":n",
    "@p",
    "--",
    "#",
    "/*",
    "*/",
    "/*!",
    "(",
    ")",
    "[",
    "]",
    "BEGIN",
    "END",
    "CASE",
    "create trigger t ",
    "DELIMITER //\n",
    "DELIMITER ;\n",
    "//",
    "E'\\''",
    "\\",
    "é",
    "名",
    "\0",
    "\u{FEFF}",
    "::",
    "insert",
    " ",
    "\n",
    "\r\n",
    "select",
    "if",
    "loop",
    "create procedure p() ",
    "\nGO\n",
    "N'",
    "[[",
    "]]",
    "@@",
    "exec ",
    "waitfor ",
    "GO",
    "begin tran ",
    "create or alter proc p as ",
];

fn fragments() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 0..40).prop_map(|v| v.concat())
}

fn any_sql() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        fragments(),
        "[a-z;'\"`$?:@#/*()\\[\\] \n-]{0,60}",
    ]
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r')
}

fn check_segments(text: &str, segs: &[Segment]) {
    let mut at = 0;
    let mut rebuilt = String::new();
    for s in segs {
        assert_eq!(s.span.start, at);
        assert!(s.span.start < s.span.end, "empty segment");
        assert!(text.is_char_boundary(s.span.start) && text.is_char_boundary(s.span.end));
        rebuilt.push_str(&text[s.span.clone()]);
        at = s.span.end;

        let body = &text[s.body.clone()];
        assert!(s.span.start <= s.body.start && s.body.end <= s.span.end);
        assert!(
            !body.starts_with(is_ws) && !body.ends_with(is_ws),
            "{body:?}"
        );
        if let Some(t) = &s.terminator {
            assert!(s.body.end <= t.start && t.end == s.span.end && t.start < t.end);
            assert_eq!(text[s.body.end..t.start].trim_matches(is_ws), "");
            assert_ne!(s.kind, SegmentKind::Delimiter);
        }
        if s.kind == SegmentKind::Statement {
            assert_ne!(body.len(), 0);
        }
    }
    assert_eq!(at, text.len());
    assert_eq!(rebuilt, text);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn segments_tile_the_input(text in any_sql()) {
        for d in Dialect::ALL {
            let segs = split(d, &text);
            check_segments(&text, &segs);
            // Splitting is deterministic.
            prop_assert_eq!(&segs, &split(d, &text));
        }
    }

    #[test]
    fn blank_line_segments_tile_the_input(text in any_sql()) {
        for d in Dialect::ALL {
            let segs = split_with(d, &text, SplitOptions::BLANK_LINES);
            check_segments(&text, &segs);
            // Blank lines only ever add boundaries.
            prop_assert!(segs.len() >= split(d, &text).len());
        }
    }

    #[test]
    fn statement_at_never_panics(text in any_sql()) {
        for d in Dialect::ALL {
            let segs = split(d, &text);
            for offset in 0..=text.len() + 1 {
                let found = statement_at(&segs, offset);
                if let Some(s) = found {
                    prop_assert_eq!(s.kind, SegmentKind::Statement);
                }
                let has_statement = segs.iter().any(|s| s.kind == SegmentKind::Statement);
                prop_assert_eq!(found.is_some(), has_statement);
            }
        }
    }

    #[test]
    fn placeholder_ranges_are_valid(text in any_sql()) {
        for d in Dialect::ALL {
            let found = placeholders(d, &text);
            let mut prev_end = 0;
            for p in &found {
                prop_assert!(p.range.start >= prev_end);
                prop_assert!(p.range.start < p.range.end && p.range.end <= text.len());
                prop_assert!(text.is_char_boundary(p.range.start));
                prop_assert!(text.is_char_boundary(p.range.end));
                prev_end = p.range.end;
            }
        }
    }

    #[test]
    fn quoted_placeholders_are_ignored(inner in "[^'\\\\]{0,30}", pre in "[a-z ]{0,8}") {
        for d in Dialect::ALL {
            let text = format!("select {pre}'{inner}$1 ? :n @p $x'");
            prop_assert_eq!(placeholders(d, &text).len(), 0, "{} {:?}", d.as_str(), text);
        }
    }

    #[test]
    fn leading_statements_are_independent(a in "[a-z ]{1,10}", b in "[a-z ]{1,10}") {
        for d in Dialect::ALL {
            let text = format!("select {a};select {b}");
            let n = split(d, &text).iter().filter(|s| s.kind == SegmentKind::Statement).count();
            prop_assert_eq!(n, 2);
        }
    }
}

#[test]
fn hostile_inputs_finish_and_tile() {
    let mib = 1 << 20;
    let cases: Vec<(&str, String)> = vec![
        ("quotes", "'".repeat(2 * mib)),
        ("double quotes", "\"".repeat(mib)),
        ("parens", "(".repeat(mib)),
        ("close parens", ")".repeat(mib)),
        ("block comments", "/*".repeat(500_000)),
        ("comment ends", "*/".repeat(500_000)),
        ("many statements", "select 1;".repeat(200_000)),
        ("semicolons", ";".repeat(mib)),
        ("begin", "create trigger t begin ".repeat(mib / 23)),
        ("end", "create trigger t end ".repeat(mib / 21)),
        ("case", "create trigger t case when ".repeat(mib / 27) + ";"),
        ("dollar", "$a$".repeat(mib / 3)),
        ("dollar pairs", "$$ x ".repeat(mib / 5)),
        ("nul", "\0".repeat(mib)),
        ("multibyte", "名;é'".repeat(mib / 8)),
        (
            "delimiter word",
            format!("DELIMITER {}\nselect 1//", "x".repeat(mib)),
        ),
        (
            "delimiter lines",
            "DELIMITER //\nselect 1//\nDELIMITER ;\n".repeat(30_000),
        ),
        (
            "delimiter prefix",
            format!("DELIMITER {}x\n{}", "(".repeat(16), "(".repeat(mib)),
        ),
        ("exec comment", "/*!".repeat(300_000)),
        ("dashes", "--".repeat(mib / 2)),
        ("colons", ":a:".repeat(mib / 3)),
        ("brackets", "[".repeat(mib)),
        (
            "end if",
            "create procedure p() begin ".to_owned() + &"end if;".repeat(100_000),
        ),
        ("go words", "GO ".repeat(mib / 3)),
        ("go lines", "GO\n".repeat(mib / 3)),
        ("go after blanks", " ".repeat(mib) + "GO"),
        ("go before blanks", "GO".to_owned() + &" ".repeat(mib)),
        ("go comments", "GO -- x\n".repeat(100_000)),
        ("tsql begin", "begin ".repeat(mib / 6)),
        ("tsql begin semicolons", "begin;".repeat(mib / 6)),
        ("tsql end", "end ".repeat(mib / 4)),
        ("tsql case", "select case when ".repeat(mib / 17) + ";"),
        (
            "tsql routine",
            "create or alter proc p as ".to_owned() + &"select 1;".repeat(100_000),
        ),
        ("at signs", "@".repeat(mib)),
        ("hashes", "#".repeat(mib)),
        ("closing brackets", "[a]]".repeat(mib / 4)),
        ("money", "$".repeat(mib)),
    ];
    for (name, text) in &cases {
        for d in Dialect::ALL {
            let segs = split(d, text);
            let mut at = 0;
            for s in &segs {
                assert_eq!(s.span.start, at, "{name} {d:?}");
                at = s.span.end;
            }
            assert_eq!(at, text.len(), "{name} {d:?}");
            let _ = statement_at(&segs, text.len() / 2);
            let _ = placeholders(d, text);
        }
    }
}

#[test]
fn statement_at_is_fast_on_many_statements() {
    let text = "select 1;".repeat(100_000);
    let segs = split(Dialect::Postgres, &text);
    assert_eq!(segs.len(), 100_000);
    for offset in (0..text.len()).step_by(997) {
        assert!(statement_at(&segs, offset).is_some());
    }
}

#[test]
fn utf8_boundaries_next_to_delimiters() {
    for d in Dialect::ALL {
        for text in [
            "é;é;é",
            "名'名'名;名",
            "select '名';名",
            "DELIMITER é\nselect 1éselect 2é",
            "DELIMITER 名名\nx名名\u{FEFF}名名",
            "/*名*/;--名\n;#名",
            "$名$;$名$;$$名$$",
        ] {
            let segs = split(d, text);
            check_segments(text, &segs);
            for offset in 0..=text.len() + 1 {
                let _ = statement_at(&segs, offset);
            }
            let _ = placeholders(d, text);
        }
    }
}
