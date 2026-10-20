// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! AC4: a rendered `INSERT` re-lexes with the `sqler-sql` lexer into the expected tokens, for
//! arbitrary text including quotes, backslashes and NUL, and every literal decodes back to the
//! input.

use std::num::NonZeroUsize;

use proptest::prelude::*;
use sqler_core::{Column, DataType, Row, Value};
use sqler_export::{ExportOptions, SqlFormat, export_to_vec};
use sqler_sql::{Dialect, Token, TokenKind, tokenize};

/// Arbitrary text, or text built from the pieces that break naive escaping.
fn text_strategy() -> impl Strategy<Value = String> {
    let pieces = prop::sample::select(vec![
        "'", "''", "\\", "\"", "\0", ";", "--", "/*", "*/", "$$", "`", "a", " ", "\n", "\u{e9}",
        "E'", "x",
    ]);
    prop_oneof![
        any::<String>(),
        prop::collection::vec(pieces, 0..10).prop_map(|v| v.concat()),
    ]
}

fn value_strategy() -> impl Strategy<Value = Value> {
    prop_oneof![
        3 => text_strategy().prop_map(Value::Text),
        1 => Just(Value::Null),
        1 => any::<i64>().prop_map(Value::Int),
        1 => prop::collection::vec(any::<u8>(), 0..6).prop_map(Value::Bytes),
    ]
}

fn name_strategy() -> impl Strategy<Value = String> {
    text_strategy().prop_filter("quote_identifier needs a non-empty name without NUL", |s| {
        !s.is_empty() && !s.contains('\0')
    })
}

/// What one significant token must look like.
#[derive(Debug, Clone, PartialEq)]
enum Expect {
    Word(&'static str),
    Punct(char),
    Ident,
    Str,
    Number,
    Minus,
}

fn expected_literal(dialect: Dialect, value: &Value) -> Vec<Expect> {
    use Expect::{Punct, Str, Word};
    match (dialect, value) {
        (_, Value::Null) => vec![Word("NULL")],
        (_, Value::Int(i)) if *i < 0 => vec![Expect::Minus, Expect::Number],
        (_, Value::Int(_)) => vec![Expect::Number],
        (Dialect::Postgres, Value::Bytes(_)) => {
            vec![Word("decode"), Punct('('), Str, Punct(','), Str, Punct(')')]
        }
        (Dialect::MySql, Value::Text(t)) if t.contains(['\\', '\0']) => {
            vec![Word("_utf8mb4"), Str]
        }
        (Dialect::Sqlite, Value::Text(t)) if t.contains('\0') => vec![
            Word("CAST"),
            Punct('('),
            Str,
            Word("AS"),
            Word("TEXT"),
            Punct(')'),
        ],
        (_, _) => vec![Str],
    }
}

fn matches_expect(token: &Token, src: &str, expect: &Expect) -> bool {
    let text = token.text(src);
    match expect {
        Expect::Word(w) => token.kind == TokenKind::Word && text == *w,
        Expect::Punct(c) => token.kind == TokenKind::Punct && text.chars().eq([*c]),
        Expect::Ident => token.kind == (TokenKind::QuotedIdent { terminated: true }),
        Expect::Str => token.kind == (TokenKind::Str { terminated: true }),
        Expect::Number => token.kind == TokenKind::Number,
        Expect::Minus => token.kind == TokenKind::Op && text == "-",
    }
}

/// The part of a string token between the first and the last quote, with `''` undoubled.
/// Returns the prefix before the first quote as well.
fn unquote(token_text: &str) -> Option<(&str, String)> {
    let open = token_text.find('\'')?;
    let inner = token_text[open + 1..].strip_suffix('\'')?;
    Some((&token_text[..open], inner.replace("''", "'")))
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || !hex.is_ascii() {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Undoes the `E'..'` escapes the writer produces: `\\` and `\x00`.
fn unescape_e(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '\\' {
            if let Some(r) = rest.strip_prefix("\\\\") {
                out.push('\\');
                rest = r;
            } else {
                rest = rest.strip_prefix("\\x00")?;
                out.push('\0');
            }
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    Some(out)
}

/// Decodes the string tokens of one literal back to the value.
fn decode_check(dialect: Dialect, value: &Value, strs: &[String]) -> Result<(), String> {
    match value {
        Value::Text(t) => {
            let (prefix, body) = unquote(&strs[0]).ok_or("not a string token")?;
            let decoded = match (dialect, prefix) {
                (Dialect::Postgres, "E") => unescape_e(&body).ok_or("bad E escape")?,
                (Dialect::MySql | Dialect::Sqlite, "X") => {
                    String::from_utf8(unhex(&body).ok_or("bad hex")?).map_err(|e| e.to_string())?
                }
                (_, "") => body,
                other => return Err(format!("unexpected prefix {other:?}")),
            };
            if decoded == *t {
                Ok(())
            } else {
                Err(format!("decoded {decoded:?}, wanted {t:?}"))
            }
        }
        Value::Bytes(b) => {
            let (prefix, body) = unquote(&strs[0]).ok_or("not a string token")?;
            let want_prefix = if dialect == Dialect::Postgres {
                ""
            } else {
                "X"
            };
            if prefix != want_prefix {
                return Err(format!("bytes prefix {prefix:?}"));
            }
            if unhex(&body).as_deref() == Some(b.as_slice()) {
                Ok(())
            } else {
                Err(format!("hex {body:?} is not {b:?}"))
            }
        }
        _ => Ok(()),
    }
}

fn check(
    dialect: Dialect,
    names: &[String],
    rows: &[Vec<Value>],
    per_statement: usize,
) -> Result<(), String> {
    let columns: Vec<_> = names
        .iter()
        .map(|n| Column::new(n.clone(), DataType::Text))
        .collect();
    let rows_in: Vec<_> = rows.iter().cloned().map(Row).collect();
    let mut options = ExportOptions::default();
    options.sql.dialect = dialect;
    "t".clone_into(&mut options.sql.table);
    options.sql.rows_per_statement = NonZeroUsize::new(per_statement).ok_or("zero")?;
    let bytes =
        export_to_vec(&SqlFormat, &options, &columns, &rows_in).map_err(|e| e.to_string())?;
    let src = String::from_utf8(bytes).map_err(|e| e.to_string())?;

    let tokens = tokenize(dialect, &src);
    if let Some(open) = tokens.iter().find(|t| !t.is_terminated()) {
        return Err(format!("unterminated token {:?}", open.text(&src)));
    }
    let sig: Vec<&Token> = tokens
        .iter()
        .filter(|t| t.kind != TokenKind::Whitespace)
        .collect();
    if sig.iter().any(|t| {
        matches!(
            t.kind,
            TokenKind::LineComment
                | TokenKind::BlockComment { .. }
                | TokenKind::ExecComment { .. }
                | TokenKind::DollarStr { .. }
        )
    }) {
        return Err("a comment or dollar string appeared outside a literal".to_owned());
    }

    // The expected sequence, with the string tokens of each value literal kept for decoding.
    let mut expected: Vec<Expect> = Vec::new();
    let mut literal_slots: Vec<(usize, &Value)> = Vec::new();
    let statements = rows.len().div_ceil(per_statement);
    let mut row_iter = rows.iter();
    for _ in 0..statements {
        expected.extend([Expect::Word("INSERT"), Expect::Word("INTO"), Expect::Ident]);
        expected.push(Expect::Punct('('));
        for i in 0..names.len() {
            if i > 0 {
                expected.push(Expect::Punct(','));
            }
            expected.push(Expect::Ident);
        }
        expected.extend([Expect::Punct(')'), Expect::Word("VALUES")]);
        for r in 0..per_statement {
            let Some(row) = row_iter.next() else { break };
            if r > 0 {
                expected.push(Expect::Punct(','));
            }
            expected.push(Expect::Punct('('));
            for (i, value) in row.iter().enumerate() {
                if i > 0 {
                    expected.push(Expect::Punct(','));
                }
                literal_slots.push((expected.len(), value));
                expected.extend(expected_literal(dialect, value));
            }
            expected.push(Expect::Punct(')'));
        }
        expected.push(Expect::Punct(';'));
    }

    if sig.len() != expected.len() {
        return Err(format!(
            "{} tokens, expected {}\n{src}",
            sig.len(),
            expected.len()
        ));
    }
    for (i, (token, want)) in sig.iter().zip(&expected).enumerate() {
        if !matches_expect(token, &src, want) {
            return Err(format!(
                "token {i} {:?} {:?} is not {want:?}\n{src}",
                token.kind,
                token.text(&src)
            ));
        }
    }

    let semicolons = sig
        .iter()
        .filter(|t| t.kind == TokenKind::Punct && t.text(&src) == ";")
        .count();
    if semicolons != statements {
        return Err(format!("{semicolons} semicolons, expected {statements}"));
    }

    for (start, value) in literal_slots {
        let len = expected_literal(dialect, value).len();
        let strs: Vec<String> = sig[start..start + len]
            .iter()
            .filter(|t| t.kind == (TokenKind::Str { terminated: true }))
            .map(|t| t.text(&src).to_owned())
            .collect();
        decode_check(dialect, value, &strs).map_err(|e| format!("{e}\n{src}"))?;
    }
    Ok(())
}

proptest! {
    #[test]
    fn insert_relexes_and_decodes(
        names in prop::collection::vec(name_strategy(), 1..4),
        cells in prop::collection::vec(prop::collection::vec(value_strategy(), 3), 0..7),
        per_statement in 1_usize..4,
    ) {
        for dialect in Dialect::ALL {
            // Every row has as many values as there are columns.
            let rows: Vec<Vec<Value>> = cells
                .iter()
                .map(|r| r.iter().cycle().take(names.len()).cloned().collect())
                .collect();
            let result = check(dialect, &names, &rows, per_statement);
            prop_assert!(result.is_ok(), "{dialect:?}: {}", result.unwrap_err());
        }
    }
}

#[test]
fn known_hostile_values() {
    let hostile = [
        "'",
        "''",
        "\\",
        "\\'",
        "'; DROP TABLE t; --",
        "\0",
        "a\0\\'b",
        "$$ x $$",
        "/* x",
        "-- x",
        "\u{e9}'\u{540d}\\",
        "E'",
        "",
    ];
    for dialect in Dialect::ALL {
        let rows: Vec<Vec<Value>> = hostile
            .iter()
            .map(|t| {
                vec![
                    Value::Text((*t).to_owned()),
                    Value::Bytes(t.as_bytes().to_vec()),
                ]
            })
            .collect();
        let names = vec!["a".to_owned(), "b\"`'".to_owned()];
        for per in [1, 2, 5, 100] {
            check(dialect, &names, &rows, per).unwrap();
        }
    }
}
