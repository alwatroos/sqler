// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of the formatter and hostile inputs.

use std::time::Instant;

use proptest::prelude::*;
use sqler_sql::{Dialect, FormatError, FormatOptions, format};

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
    "-- c\n",
    "#",
    "/*",
    "*/",
    "/* c */",
    "/*!50000 x */",
    "(",
    ")",
    "[",
    "]",
    ",",
    ".",
    "BEGIN",
    "END",
    "CASE",
    "create trigger t ",
    "create procedure p() ",
    "DELIMITER //\n",
    "DELIMITER ;\n",
    "DELIMITER\n",
    "//",
    "E'\\''",
    "\\",
    "é",
    "名",
    "\0",
    "\u{FEFF}",
    "::",
    "insert",
    "select",
    "SELECT",
    "from",
    "where",
    "and",
    "or",
    "between",
    "join",
    "left",
    "outer",
    "inner",
    "order",
    "by",
    "union",
    "values",
    "set",
    "limit",
    "t",
    "1",
    "1e",
    "+",
    "-",
    "=",
    " ",
    "  ",
    "\n",
    "\r\n",
    "\t",
];

fn fragments() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 0..40).prop_map(|v| v.concat())
}

fn any_sql() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        fragments(),
        fragments(),
        "[a-zA-Z0-9 \n;,.()'\"`$:?@#*/+=<>-]{0,80}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn never_panics_and_is_idempotent(text in any_sql(), upper in any::<bool>(), width in 0u8..=255) {
        let mut options = FormatOptions::default();
        options.uppercase_keywords = upper;
        options.indent_width = width;
        for dialect in Dialect::ALL {
            if let Ok(once) = format(dialect, &text, &options) {
                prop_assert_eq!(format(dialect, &once, &options), Ok(once.clone()), "{:?}", dialect);
                prop_assert!(once.is_empty() || once.ends_with('\n'));
            }
        }
    }

    #[test]
    fn an_open_string_or_comment_is_an_error(junk in "[^'*]{0,40}", which in 0usize..3) {
        let opener = ["'", "/*", "'it''s"][which];
        let text = format!("select 1; select {opener}{junk}");
        for dialect in Dialect::ALL {
            let result = format(dialect, &text, &FormatOptions::default());
            prop_assert!(
                matches!(result, Err(FormatError::Unterminated { offset }) if offset >= 10),
                "{:?} {:?} {:?}", dialect, text, result
            );
        }
    }

    #[test]
    fn a_closed_quote_is_copied_byte_for_byte(body in "[^'\\\\]{0,40}") {
        let text = format!("select   '{body}'  ,  1");
        for dialect in Dialect::ALL {
            let out = format(dialect, &text, &FormatOptions::default());
            prop_assert_eq!(out, Ok(format!("SELECT '{body}', 1\n")));
        }
    }
}

fn check_hostile(dialect: Dialect, text: &str) {
    let options = FormatOptions::default();
    let started = Instant::now();
    let result = format(dialect, text, &options);
    if let Ok(out) = result {
        assert!(out.len() <= text.len() * 80 + 1024, "output too large");
        assert_eq!(format(dialect, &out, &options), Ok(out));
    }
    assert!(started.elapsed().as_secs() < 60, "too slow");
}

#[test]
fn hostile_inputs_finish() {
    const MIB: usize = 1 << 20;
    let inputs = [
        "select a from t where x = 1 and y in (select 1);\n".repeat(MIB / 50),
        "(select ".repeat(MIB / 8),
        "(".repeat(MIB),
        ")".repeat(MIB),
        "select 1 -- c\n".repeat(MIB / 14),
        "/* c */ ".repeat(MIB / 8),
        "a and ".repeat(MIB / 6),
        "x between 1 and 2 and ".repeat(MIB / 22),
        "'a' ".repeat(MIB / 4),
        "\0".repeat(MIB),
        ";".repeat(MIB),
        "select ".repeat(MIB / 7),
        "left join ".repeat(MIB / 10),
        "名é ".repeat(MIB / 7),
    ];
    // One dialect per input (rotating) keeps the debug build fast; each dialect sees several.
    for (i, text) in inputs.iter().enumerate() {
        check_hostile(Dialect::ALL[i % Dialect::ALL.len()], text);
    }
    let unterminated = "'".repeat(MIB + 1);
    for dialect in Dialect::ALL {
        assert_eq!(
            format(dialect, &unterminated, &FormatOptions::default()),
            Err(FormatError::Unterminated { offset: 0 })
        );
    }
}

#[test]
fn deep_nesting_has_bounded_indentation() {
    let text = "(select ".repeat(50_000);
    let out = format(Dialect::Postgres, &text, &FormatOptions::default()).expect("formats");
    assert!(out.len() < text.len() * 40);
}

/// R7: a keyword glued to a non-ASCII char is one identifier for the server; the formatter must
/// not split it.
#[test]
fn glued_other_tokens_stay_glued() {
    let opts = FormatOptions::default();
    for d in Dialect::ALL {
        for text in [
            "select€x",
            "select\u{a0}1",
            "select 1 from t where a=€b",
            "select 1€",
        ] {
            let out = format(d, text, &opts).unwrap();
            let strip = |s: &str| {
                s.chars()
                    .filter(|c| !c.is_ascii_whitespace())
                    .collect::<String>()
            };
            assert_eq!(
                strip(&out).to_lowercase(),
                strip(text).to_lowercase(),
                "{d:?}: {text:?}"
            );
            assert!(!out.contains(" €") || text.contains(" €"), "{d:?}: {out:?}");
            assert_eq!(format(d, &out, &opts).as_deref(), Ok(out.as_str()));
        }
    }
    let out = format(Dialect::Postgres, "select€x", &opts).unwrap();
    assert!(out.to_lowercase().contains("select€x"), "{out:?}");
}
