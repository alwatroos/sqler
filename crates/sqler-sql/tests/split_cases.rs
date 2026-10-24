// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Splitting and cursor-lookup cases per dialect.

use sqler_sql::{Dialect, PlaceholderKind, SegmentKind, placeholders, split, statement_at};

use Dialect::{MySql, Postgres, Sqlite};

/// The statement bodies (not delimiter lines, not trivia) in order.
fn stmts(dialect: Dialect, text: &str) -> Vec<&str> {
    split(dialect, text)
        .into_iter()
        .filter(|s| s.kind == SegmentKind::Statement)
        .map(|s| &text[s.body])
        .collect()
}

fn check_tiling(dialect: Dialect, text: &str) {
    let segs = split(dialect, text);
    let mut at = 0;
    for s in &segs {
        assert_eq!(s.span.start, at, "{text:?}");
        assert!(s.body.start >= s.span.start && s.body.end <= s.span.end);
        at = s.span.end;
    }
    assert_eq!(at, text.len(), "{text:?}");
}

#[test]
fn plain_statements() {
    for d in Dialect::ALL {
        assert_eq!(stmts(d, "select 1; select 2;"), ["select 1", "select 2"]);
        assert_eq!(stmts(d, "select 1;select 2"), ["select 1", "select 2"]);
        assert_eq!(stmts(d, "  select 1  "), ["select 1"]);
    }
}

#[test]
fn strings_hide_semicolons() {
    for d in Dialect::ALL {
        assert_eq!(stmts(d, "select ';'; select 2"), ["select ';'", "select 2"]);
        assert_eq!(
            stmts(d, "select 'it''s;'; select 2"),
            ["select 'it''s;'", "select 2"]
        );
    }
    assert_eq!(
        stmts(Postgres, r"select E'\';'; select 2"),
        [r"select E'\';'", "select 2"]
    );
    assert_eq!(
        stmts(MySql, r"select 'a\'; b'; select 2"),
        [r"select 'a\'; b'", "select 2"]
    );
    // Without the E prefix a PostgreSQL backslash is plain text, so the quote closes.
    assert_eq!(
        stmts(Postgres, r"select 'a\'; select 2"),
        [r"select 'a\'", "select 2"]
    );
    assert_eq!(
        stmts(Sqlite, r"select 'a\'; select 2"),
        [r"select 'a\'", "select 2"]
    );
}

#[test]
fn quoted_identifiers_hide_semicolons() {
    assert_eq!(
        stmts(Postgres, r#"select "a;b"; select 2"#),
        [r#"select "a;b""#, "select 2"]
    );
    assert_eq!(
        stmts(Sqlite, r#"select "a;b"; select 2"#),
        [r#"select "a;b""#, "select 2"]
    );
    assert_eq!(
        stmts(MySql, r#"select "a;b"; select 2"#),
        [r#"select "a;b""#, "select 2"]
    );
    assert_eq!(
        stmts(MySql, "select `a;b`; select 2"),
        ["select `a;b`", "select 2"]
    );
    assert_eq!(
        stmts(Sqlite, "select `a;b`; select 2"),
        ["select `a;b`", "select 2"]
    );
    assert_eq!(
        stmts(Sqlite, "select [a;b]; select 2"),
        ["select [a;b]", "select 2"]
    );
    assert_eq!(
        stmts(Postgres, r#"select U&"a;b"; select 2"#),
        [r#"select U&"a;b""#, "select 2"]
    );
}

#[test]
fn comments_hide_semicolons() {
    for d in Dialect::ALL {
        assert_eq!(
            stmts(d, "select 1 -- ;\n; select 2"),
            ["select 1 -- ;", "select 2"]
        );
        assert_eq!(
            stmts(d, "select /* ; */ 1; select 2"),
            ["select /* ; */ 1", "select 2"]
        );
    }
    // `#` is a comment on MySQL only.
    assert_eq!(
        stmts(MySql, "select 1 # ;\n; select 2"),
        ["select 1 # ;", "select 2"]
    );
    assert_eq!(
        stmts(Postgres, "select 1 # ;\n; select 2"),
        ["select 1 #", "select 2"]
    );
    assert_eq!(
        stmts(Sqlite, "select 1 # ;\n; select 2"),
        ["select 1 #", "select 2"]
    );
    // MySQL needs whitespace after `--`.
    assert_eq!(
        stmts(MySql, "select 1 --; select 2"),
        ["select 1 --", "select 2"]
    );
    assert_eq!(
        stmts(Postgres, "select 1 --; select 2"),
        ["select 1 --; select 2"]
    );
}

#[test]
fn block_comments_nest_on_postgres_only() {
    let text = "/* a /* ; */ ; */ select 1; select 2";
    assert_eq!(
        stmts(Postgres, text),
        ["/* a /* ; */ ; */ select 1", "select 2"]
    );
    assert_eq!(stmts(Sqlite, text).len(), 2);
    assert_eq!(stmts(Sqlite, text)[0], "*/ select 1");
    // Unterminated comment: the rest is trivia.
    let g = split(Postgres, "select 1; /* open ; select 2");
    assert_eq!(g.len(), 2);
    assert_eq!(g[1].kind, SegmentKind::Trivia);
}

#[test]
fn dollar_quotes() {
    assert_eq!(
        stmts(Postgres, "select $$;$$; select 2"),
        ["select $$;$$", "select 2"]
    );
    assert_eq!(
        stmts(Postgres, "select $f$ $$ ; $f$; select 2"),
        ["select $f$ $$ ; $f$", "select 2"]
    );
    assert_eq!(
        stmts(Postgres, "select $1; select $2"),
        ["select $1", "select $2"]
    );
    assert_eq!(
        stmts(Postgres, "select $1$a; select 2"),
        ["select $1$a", "select 2"]
    );
    // Other dialects have no dollar quotes.
    assert_eq!(stmts(Sqlite, "select $$;$$"), ["select $$", "$$"]);
    assert_eq!(stmts(MySql, "select $$;$$"), ["select $$", "$$"]);
    // An unterminated dollar quote swallows the rest.
    assert_eq!(stmts(Postgres, "select $a$ ; select 2").len(), 1);
}

#[test]
fn postgres_function_bodies() {
    let f = "create function f() returns int as $$ begin return 1; end; $$ language plpgsql";
    assert_eq!(stmts(Postgres, &format!("{f}; select 2")), [f, "select 2"]);
    let atomic =
        "create function f() returns int language sql begin atomic select 1; select 2; end";
    assert_eq!(
        stmts(Postgres, &format!("{atomic}; select 3")),
        [atomic, "select 3"]
    );
}

#[test]
fn postgres_parens_do_not_split() {
    assert_eq!(
        stmts(Postgres, "select (1; 2); select 3"),
        ["select (1; 2)", "select 3"]
    );
    assert_eq!(
        stmts(Sqlite, "select (1; 2); select 3"),
        ["select (1", "2)", "select 3"]
    );
    // Stray `)` saturates at zero.
    assert_eq!(
        stmts(Postgres, "select 1); select 2"),
        ["select 1)", "select 2"]
    );
}

#[test]
fn sqlite_trigger_with_case() {
    let t = "create trigger tr after insert on t begin \
             update u set a = case when new.x > 0 then 1 else 2 end; \
             delete from v; end";
    assert_eq!(stmts(Sqlite, &format!("{t}; select 2")), [t, "select 2"]);
    let t2 = "CREATE TRIGGER IF NOT EXISTS tr BEFORE DELETE ON t FOR EACH ROW BEGIN select 1; END";
    assert_eq!(stmts(Sqlite, &format!("{t2};")), [t2]);
}

#[test]
fn mysql_procedure_with_end_if() {
    let p = "CREATE PROCEDURE p(x int) BEGIN IF x > 0 THEN select 1; ELSEIF x < 0 THEN select 2; \
             END IF; WHILE x < 5 DO set x = x + 1; END WHILE; \
             CASE x WHEN 1 THEN select 1; ELSE select 2; END CASE; \
             loop1: LOOP leave loop1; END LOOP loop1; END";
    // Without DELIMITER the block depth keeps the body together.
    assert_eq!(stmts(MySql, &format!("{p}; select 9")), [p, "select 9"]);
    // With DELIMITER the delimiter splits.
    let script = format!("DELIMITER //\n{p}//\nDELIMITER ;\nselect 9;");
    let segs = split(MySql, &script);
    let kinds: Vec<_> = segs.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        [
            SegmentKind::Delimiter,
            SegmentKind::Statement,
            SegmentKind::Delimiter,
            SegmentKind::Statement
        ]
    );
    assert_eq!(&script[segs[0].body.clone()], "DELIMITER //");
    assert_eq!(&script[segs[1].body.clone()], p);
    assert_eq!(&script[segs[1].terminator.clone().unwrap()], "//");
    assert_eq!(&script[segs[2].body.clone()], "DELIMITER ;");
    assert_eq!(&script[segs[3].body.clone()], "select 9");
    check_tiling(MySql, &script);
}

#[test]
fn mysql_begin_not_atomic() {
    let b = "BEGIN NOT ATOMIC select 1; select 2; END";
    assert_eq!(stmts(MySql, &format!("{b}; select 3")), [b, "select 3"]);
}

#[test]
fn mysql_delimiter_variants() {
    // `;;` and a delimiter glued to a word (`END$$`).
    let s = "delimiter ;;\nselect 1; select 2;;\nselect 3;;\nDELIMITER ;\nselect 4;";
    assert_eq!(
        stmts(MySql, s),
        ["select 1; select 2", "select 3", "select 4"]
    );
    let s = "DELIMITER $$\ncreate procedure p() begin select 1; end$$\nDELIMITER ;";
    assert_eq!(
        stmts(MySql, s),
        ["create procedure p() begin select 1; end"]
    );
    // Only at a statement start, and only on MySQL.
    assert_eq!(
        stmts(MySql, "select 1\nDELIMITER //\nselect 2;"),
        ["select 1\nDELIMITER //\nselect 2"]
    );
    assert_eq!(
        stmts(Postgres, "DELIMITER //\nselect 2;"),
        ["DELIMITER //\nselect 2"]
    );
    assert_eq!(
        stmts(Sqlite, "DELIMITER //\nselect 2;"),
        ["DELIMITER //\nselect 2"]
    );
    // No word: ordinary text.
    assert_eq!(
        stmts(MySql, "DELIMITER\nselect 1;"),
        ["DELIMITER\nselect 1"]
    );
    assert_eq!(stmts(MySql, "DELIMITER   ;"), Vec::<&str>::new());
    assert_eq!(stmts(MySql, "DELIMITER"), ["DELIMITER"]);
    // A leading comment stays in the delimiter segment's body.
    let g = split(MySql, "-- hi\nDELIMITER //\n");
    assert_eq!(g[0].kind, SegmentKind::Delimiter);
    assert_eq!(g[0].body, 0..18);
    assert_eq!(g[1].kind, SegmentKind::Trivia);
    // A delimiter segment ends before the line break; `\r\n` goes to the next segment.
    let s = "DELIMITER //\r\nselect 1//";
    let g = split(MySql, s);
    assert_eq!(g[0].span, 0..12);
    assert_eq!(&s[g[1].span.clone()], "\r\nselect 1//");
    // Too long a word is ordinary text.
    let long = format!("DELIMITER {}\nselect 1;", "x".repeat(17));
    assert_eq!(split(MySql, &long).len(), 1);
}

#[test]
fn transaction_begin_is_not_a_block() {
    for d in Dialect::ALL {
        assert_eq!(
            stmts(d, "BEGIN; select 1; COMMIT;"),
            ["BEGIN", "select 1", "COMMIT"]
        );
    }
    assert_eq!(
        stmts(MySql, "begin work; select 1; end"),
        ["begin work", "select 1", "end"]
    );
}

#[test]
fn lone_semicolons_and_trailing_comments() {
    for d in Dialect::ALL {
        let s = ";;select 1; -- bye";
        let kinds: Vec<_> = split(d, s).iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            [
                SegmentKind::Trivia,
                SegmentKind::Trivia,
                SegmentKind::Statement,
                SegmentKind::Trivia
            ]
        );
        check_tiling(d, s);
        assert_eq!(stmts(d, ";"), Vec::<&str>::new());
        assert_eq!(stmts(d, "-- only"), Vec::<&str>::new());
        assert_eq!(stmts(d, "select 1 -- end"), ["select 1 -- end"]);
    }
}

#[test]
fn unterminated_tokens_swallow_the_rest() {
    for d in Dialect::ALL {
        assert_eq!(stmts(d, "select 1; select 'abc; select 3").len(), 2);
        assert_eq!(
            stmts(d, "select 1; select 'abc; select 3")[1],
            "select 'abc; select 3"
        );
    }
    let t = "create trigger t after insert on x begin select 1; select 2;";
    assert_eq!(
        stmts(Sqlite, t),
        [t.trim_end_matches(';').to_string() + ";"]
    );
}

#[test]
fn non_ascii_text() {
    for d in Dialect::ALL {
        let s = "select 'é;名'; select 名 from é; select \u{FEFF}1;\0";
        check_tiling(d, s);
        assert_eq!(stmts(d, s).len(), 4, "{d:?}");
    }
    let s = "DELIMITER 名名\nselect 1名名select é名名";
    check_tiling(MySql, s);
    assert_eq!(stmts(MySql, s), ["select 1", "select é"]);
}

#[test]
fn statement_at_rules() {
    let s = "  select 1 ;  \n select 2 ;\n -- c\n";
    //       0         1          2
    //       0123456789012345 6789012345678901
    for d in Dialect::ALL {
        let g = split(d, s);
        let at = |o| statement_at(&g, o).map(|g| &s[g.body.clone()]);
        // (c) before the first statement.
        assert_eq!(at(0), Some("select 1"));
        assert_eq!(at(1), Some("select 1"));
        // (a) inside, at body.start, on the terminator.
        assert_eq!(at(2), Some("select 1"));
        assert_eq!(at(9), Some("select 1"));
        assert_eq!(at(11), Some("select 1"));
        // (b) at the end of a statement and on the whitespace after it.
        assert_eq!(at(12), Some("select 1"));
        assert_eq!(at(14), Some("select 1"));
        assert_eq!(at(15), Some("select 1"));
        // Next statement starts at its body.
        assert_eq!(at(16), Some("select 2"));
        assert_eq!(at(25), Some("select 2"));
        // Trailing comment, end of text and beyond.
        assert_eq!(at(30), Some("select 2"));
        assert_eq!(at(s.len()), Some("select 2"));
        assert_eq!(at(s.len() + 100), Some("select 2"));
        assert_eq!(at(usize::MAX), Some("select 2"));
    }
}

#[test]
fn statement_at_edge_cases() {
    assert!(statement_at(&[], 0).is_none());
    for d in Dialect::ALL {
        assert!(statement_at(&split(d, ";; -- x"), 3).is_none());
        let s = "select 1;select 2";
        let g = split(d, s);
        assert_eq!(
            statement_at(&g, 8).map(|g| &s[g.body.clone()]),
            Some("select 1")
        );
        assert_eq!(
            statement_at(&g, 9).map(|g| &s[g.body.clone()]),
            Some("select 2")
        );
        // Unterminated last statement: offset at its end resolves to it.
        assert_eq!(
            statement_at(&g, 17).map(|g| &s[g.body.clone()]),
            Some("select 2")
        );
        // Delimiter lines are never returned.
        let s = "DELIMITER //\nselect 1//";
        let g = split(MySql, s);
        assert_eq!(
            statement_at(&g, 3).map(|g| g.kind),
            Some(SegmentKind::Statement)
        );
    }
}

fn ph(dialect: Dialect, text: &str) -> Vec<(&str, PlaceholderKind)> {
    placeholders(dialect, text)
        .into_iter()
        .map(|p| (&text[p.range], p.kind))
        .collect()
}

fn named(s: &str) -> PlaceholderKind {
    PlaceholderKind::Named(s.to_owned())
}

#[test]
fn placeholder_table() {
    use PlaceholderKind::{Anonymous, Positional};
    let text = "select $1, ?, ?2, :n, @a, $b";
    assert_eq!(
        ph(Postgres, text),
        [("$1", Positional(1)), (":n", named(":n"))]
    );
    assert_eq!(
        ph(MySql, text),
        [("?", Anonymous), ("?", Anonymous), (":n", named(":n"))]
    );
    assert_eq!(
        ph(Sqlite, text),
        [
            // SQLite reads `$1` as a name, like `$b`.
            ("$1", named("$1")),
            ("?", Anonymous),
            ("?2", Positional(2)),
            (":n", named(":n")),
            ("@a", named("@a")),
            ("$b", named("$b")),
        ]
    );
}

#[test]
fn placeholders_ignore_literals_and_comments() {
    for d in Dialect::ALL {
        for text in [
            "select '$1 ? :n @a'",
            "select 1 -- ? :n $1\n",
            "select /* ? :n $1 */ 1",
            "select 'x'':n'",
        ] {
            assert_eq!(ph(d, text).len(), 0, "{d:?} {text}");
        }
    }
    assert_eq!(ph(Postgres, "select $$ $1 :n $$").len(), 0);
    assert_eq!(ph(Postgres, "select \"$1\", \"a:n\"").len(), 0);
    assert_eq!(ph(MySql, "select \"?\", `:n`").len(), 0);
    assert_eq!(ph(Sqlite, "select [:n], \"?\"").len(), 0);
}

#[test]
fn placeholders_postgres_specifics() {
    // Array slices, casts, jsonb operators.
    assert_eq!(ph(Postgres, "select a[1:n] from t").len(), 0);
    assert_eq!(ph(Postgres, "select a[1:n], :m").len(), 1);
    assert_eq!(ph(Postgres, "select x::int, y::text").len(), 0);
    assert_eq!(
        ph(Postgres, "select a ? 'k', a ?| array['k'], a ?& array['k']").len(),
        0
    );
    assert_eq!(ph(Postgres, "select a ? $1").len(), 1);
    assert_eq!(ph(Postgres, "select $0").len(), 0);
    // Cast right after a placeholder.
    assert_eq!(ph(Postgres, "select $1::int, :a::text").len(), 2);
}

#[test]
fn placeholder_ranges_are_global_and_ascending() {
    let text = "select :a; select\n  :b, :c;\n-- :d\nselect :e";
    for d in [Postgres, MySql, Sqlite] {
        let p = placeholders(d, text);
        let names: Vec<_> = p.iter().map(|p| &text[p.range.clone()]).collect();
        assert_eq!(names, [":a", ":b", ":c", ":e"]);
        assert!(p.windows(2).all(|w| w[0].range.end <= w[1].range.start));
    }
    // T-SQL has `@P<k>` only; `:a` is not a placeholder there.
    assert_eq!(placeholders(Dialect::TSql, text).len(), 0);
    let text = "select @P1; select\n  @P2, @p3;\n-- @P4\nselect @P5, @x";
    let p = placeholders(Dialect::TSql, text);
    let names: Vec<_> = p.iter().map(|p| &text[p.range.clone()]).collect();
    assert_eq!(names, ["@P1", "@P2", "@p3", "@P5"]);
    assert!(p.windows(2).all(|w| w[0].range.end <= w[1].range.start));
}

#[test]
fn compound_statements_have_no_placeholders() {
    let t = "create trigger t after insert on x begin select :a; end; select :b";
    assert_eq!(ph(Sqlite, t), [(":b", named(":b"))]);
    let p = "create procedure p() begin select ?; end";
    assert_eq!(ph(MySql, p).len(), 0);
}

#[test]
fn unterminated_mysql_quoted_variable_swallows_the_rest() {
    for text in [
        "select @'a; select 2",
        "select @\"a; select 2",
        "select @`a; select 2",
    ] {
        assert_eq!(stmts(MySql, text), vec![text], "{text:?}");
        check_tiling(MySql, text);
    }
    assert_eq!(stmts(MySql, "select @'a;b'; select 2").len(), 2);
}
