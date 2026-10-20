// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of the completion engine.

use std::fmt::Write as _;

use proptest::prelude::*;
use sqler_completion::{
    CompletionRequest, ContextKind, Ident, ItemKind, MemorySchema, analyze, complete,
};
use sqler_core::{ColumnInfo, DataType, ObjectKind};
use sqler_sql::Dialect;

const CASES: u32 = 512;

const FRAGMENTS: &[&str] = &[
    "SELECT",
    "select",
    "FROM",
    "from",
    "WHERE",
    "JOIN",
    "LEFT",
    "ON",
    "AS",
    "WITH",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "UNION",
    "users",
    "u",
    "orders",
    "o",
    "t1",
    "a1",
    "id",
    "name",
    "public",
    "\"Quoted Name\"",
    "`bt`",
    "[br ket]",
    "'str'",
    "42",
    "1.5",
    "*",
    "=",
    "+",
    "$1",
    "'",
    "\"",
    "\\",
    "`",
    "[",
    "]",
    "$$",
    "$tag$",
    "--",
    "/*",
    "*/",
    "#",
    "(",
    ")",
    ".",
    ",",
    ";",
    "é",
    "名",
    "🙂",
    "e'\\'",
    " ",
    "  ",
    "\n",
    "\t",
];

fn schema() -> MemorySchema {
    let mut s = MemorySchema::new(["public"]);
    for (name, cols) in [
        ("users", ["id", "name", "email"]),
        ("orders", ["id", "user_id", "total"]),
        ("t1", ["c1_0", "c1_1", "c1_2"]),
    ] {
        let cols = cols
            .iter()
            .map(|c| ColumnInfo::new(*c, DataType::Text))
            .collect();
        s.add_table("public", name, ObjectKind::Table, Some(cols));
    }
    s.add_table("public", "pending", ObjectKind::Table, None);
    s
}

fn dialect() -> impl Strategy<Value = Dialect> {
    prop::sample::select(Dialect::ALL.to_vec())
}

fn fragment_text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS.to_vec()), 0..40).prop_map(|v| {
        let mut out = String::new();
        for (i, f) in v.iter().enumerate() {
            if i % 3 != 0 {
                out.push(' ');
            }
            out.push_str(f);
        }
        out
    })
}

fn check_total(text: &str, offset: usize, d: Dialect, limit: usize) -> Result<(), TestCaseError> {
    let s = schema();
    let mut req = CompletionRequest::new(text, offset, d);
    req.limit = limit;
    let done = complete(&req, &s);
    prop_assert!(done.replace.start <= done.replace.end);
    prop_assert!(done.replace.end <= text.len());
    prop_assert!(text.is_char_boundary(done.replace.start));
    prop_assert!(text.is_char_boundary(done.replace.end));
    prop_assert!(done.items.len() <= limit);
    let scores: Vec<u32> = done.items.iter().map(|i| i.score).collect();
    prop_assert!(
        scores.windows(2).all(|w| w[0] > w[1]),
        "scores strictly decrease"
    );
    let a = analyze(text, offset, d);
    prop_assert!(a.statement.end <= text.len());
    prop_assert!(text.is_char_boundary(a.statement.start));
    prop_assert!(text.is_char_boundary(a.statement.end));
    prop_assert_eq!(a.prefix, done.replace);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// Never panics, and the answer is always well-formed, for SQL-like text.
    #[test]
    fn fragments_never_panic(text in fragment_text(), seed in any::<usize>(), d in dialect(), limit in 0usize..60) {
        check_total(&text, seed % (text.len() + 5), d, limit)?;
    }

    /// Never panics for arbitrary strings and offsets, including non-char-boundaries.
    #[test]
    fn arbitrary_text_never_panics(text in any::<String>(), seed in any::<usize>(), d in dialect()) {
        check_total(&text, seed % (text.len() + 5), d, 200)?;
    }

    /// Every offset of a text is safe.
    #[test]
    fn every_offset_is_safe(text in fragment_text(), d in dialect()) {
        for offset in 0..text.len() + 5 {
            check_total(&text, offset, d, 20)?;
        }
    }
}

/// `FROM t1 a1 JOIN t2 a2 ...` with `n` tables; table `tN` has columns `cN_0..cN_2`.
fn join_schema(n: usize) -> MemorySchema {
    let mut s = MemorySchema::new(["public"]);
    for i in 1..=n {
        let cols = (0..3)
            .map(|c| ColumnInfo::new(format!("c{i}_{c}"), DataType::Integer))
            .collect();
        s.add_table("public", &format!("t{i}"), ObjectKind::Table, Some(cols));
    }
    s
}

/// Reads inserted text back as an identifier: quoted when it starts with a quote.
fn reparse(inserted: &str) -> Ident {
    let mut chars = inserted.chars();
    match chars.next() {
        Some(q @ ('"' | '`')) => {
            let inner = inserted[1..].strip_suffix(q).unwrap_or(&inserted[1..]);
            Ident::new(inner.replace(&format!("{q}{q}"), &q.to_string()), true)
        }
        _ => Ident::new(inserted, false),
    }
}

fn from_clause(n: usize, kw: &str) -> String {
    let mut out = String::from(" FROM t1 a1");
    for i in 2..=n {
        let _ = write!(
            out,
            " {kw} t{i} a{i} ON a{i}.c{i}_0 = a{}.c{}_0",
            i - 1,
            i - 1
        );
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// Every alias candidate resolves to a relation in scope.
    #[test]
    fn alias_candidates_resolve(n in 1usize..6, d in dialect(), kw in prop::sample::select(vec!["JOIN", "LEFT JOIN", "INNER JOIN"]), prefix in prop::sample::select(vec!["", "a", "A"]), upper in any::<bool>()) {
        let s = join_schema(n);
        let from = from_clause(n, kw);
        let from = if upper { from.to_uppercase() } else { from };
        let text = format!("SELECT {prefix}{from}");
        let offset = "SELECT ".len() + prefix.len();
        let done = complete(&CompletionRequest::new(&text, offset, d), &s);
        prop_assert_eq!(done.context, ContextKind::Expression);
        let scope = analyze(&text, offset, d).scope;
        let aliases: Vec<_> = done.items.iter().filter(|i| i.kind == ItemKind::Alias).collect();
        if prefix.is_empty() || prefix.eq_ignore_ascii_case("a") {
            prop_assert_eq!(aliases.len(), n, "every alias is offered");
        }
        for item in aliases {
            let rel = scope.resolve(&Ident::new(item.label.clone(), false), d);
            prop_assert!(rel.is_some(), "{} resolves", item.label);
            // The text that would be inserted names the same relation.
            let written = reparse(&item.insert_text);
            let again = scope.resolve(&written, d);
            prop_assert!(again.is_some(), "{} re-resolves", item.insert_text);
            prop_assert_eq!(again.map(|r| &r.span), rel.map(|r| &r.span));
        }
    }

    /// After `aK.`, the columns offered belong to `tK` and all of them are offered.
    #[test]
    fn qualified_columns_belong_to_the_alias(n in 1usize..6, k in 1usize..6, d in dialect(), upper in any::<bool>()) {
        let k = (k - 1) % n + 1;
        let s = join_schema(n);
        let alias = if upper { format!("A{k}") } else { format!("a{k}") };
        let head = format!("SELECT {alias}.");
        let text = format!("{head}{}", from_clause(n, "JOIN"));
        let done = complete(&CompletionRequest::new(&text, head.len(), d), &s);
        prop_assert_eq!(done.context, ContextKind::Qualified);
        prop_assert_eq!(done.items.len(), 3);
        for item in &done.items {
            prop_assert_eq!(item.kind, ItemKind::Column);
            prop_assert!(item.label.starts_with(&format!("c{k}_")), "{} belongs to t{}", item.label, k);
        }
    }
}

/// Cursor strictly inside a string or comment: no items.
fn literal_case() -> impl Strategy<Value = (Dialect, String, usize)> {
    let body = "[a-zé名 ;,.()]{0,12}";
    (dialect(), 0usize..4, body, "[a-z ,;]{0,10}", any::<usize>()).prop_map(
        |(d, shape, body, tail, seed)| {
            let (open, close) = match shape {
                0 => ("'", "'"),
                1 => ("-- ", ""),
                2 => ("/* ", " */"),
                _ if d == Dialect::Postgres => ("$$", "$$"),
                _ => ("'", "'"),
            };
            let head = "SELECT a FROM t WHERE x = ";
            let sep = if shape == 1 { "\n" } else { "" };
            let text = format!("{head}{open}{body}{close}{sep}{tail}");
            // Offsets strictly inside the literal: after the opener, up to the last body char
            // (or, for a line comment, anywhere before the newline).
            let first = head.len() + open.len();
            let body_chars: Vec<usize> = body
                .char_indices()
                .map(|(i, _)| first + i)
                .chain(std::iter::once(first + body.len()))
                .collect();
            let offset = body_chars[seed % body_chars.len()];
            (d, text, offset)
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn nothing_inside_strings_and_comments((d, text, offset) in literal_case()) {
        let s = schema();
        let done = complete(&CompletionRequest::new(&text, offset, d), &s);
        prop_assert_eq!(done.context, ContextKind::None, "{:?} at {}", text, offset);
        prop_assert!(done.items.is_empty());
        prop_assert!(done.requests.is_empty());
    }
}
