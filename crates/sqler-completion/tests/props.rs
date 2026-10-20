// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of the completion engine.

use std::fmt::Write as _;

use proptest::prelude::*;
use sqler_completion::{
    CompletionRequest, ContextKind, DiagnosticCode, HoverRequest, Ident, ItemKind, MemorySchema,
    analyze, complete, diagnose, hover,
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

/// `diagnose` and `hover` are total and their ranges are well-formed.
fn check_diagnose_and_hover(text: &str, offset: usize, d: Dialect) -> Result<(), TestCaseError> {
    let s = schema();
    for diag in diagnose(text, d, &s) {
        prop_assert!(diag.range.start < diag.range.end, "{:?}", diag);
        prop_assert!(diag.range.end <= text.len(), "{:?}", diag);
        prop_assert!(text.is_char_boundary(diag.range.start));
        prop_assert!(text.is_char_boundary(diag.range.end));
    }
    let mut clamped = offset.min(text.len());
    while !text.is_char_boundary(clamped) {
        clamped -= 1;
    }
    if let Some(h) = hover(&HoverRequest::new(text, offset, d), &s) {
        prop_assert!(h.range.start < h.range.end, "{:?}", h.range);
        prop_assert!(h.range.end <= text.len());
        prop_assert!(text.is_char_boundary(h.range.start));
        prop_assert!(text.is_char_boundary(h.range.end));
        prop_assert!(
            h.range.start <= clamped && clamped <= h.range.end,
            "{:?} contains or ends at {}",
            h.range,
            clamped
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn diagnose_and_hover_never_panic_on_fragments(text in fragment_text(), seed in any::<usize>(), d in dialect()) {
        check_diagnose_and_hover(&text, seed % (text.len() + 5), d)?;
    }

    #[test]
    fn diagnose_and_hover_never_panic_on_arbitrary_text(text in any::<String>(), seed in any::<usize>(), d in dialect()) {
        check_diagnose_and_hover(&text, seed % (text.len() + 5), d)?;
    }

    #[test]
    fn hover_is_safe_at_every_offset(text in fragment_text(), d in dialect()) {
        for offset in 0..text.len() + 3 {
            check_diagnose_and_hover(&text, offset, d)?;
        }
    }
}

/// A schema name: mostly plain lower case, sometimes mixed case, with spaces or non-ASCII.
fn name_part(prefix: &'static str) -> impl Strategy<Value = String> {
    prop_oneof![
        5 => "[a-z][a-z0-9_]{0,6}".prop_map(move |s| format!("{prefix}{s}")),
        1 => "[A-Za-z][A-Za-z0-9 _é名]{0,6}".prop_map(move |s| format!("{}{s}", prefix.to_uppercase())),
    ]
}

/// Tables with columns; names are unique when case is ignored.
fn generated_schema() -> impl Strategy<Value = Vec<(String, Vec<String>)>> {
    prop::collection::vec(
        (
            name_part("t_"),
            prop::collection::vec(name_part("c_"), 1..5),
        ),
        1..5,
    )
    .prop_map(|tables| {
        let fold = |s: &str| s.to_lowercase();
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (name, cols) in tables {
            if seen.contains(&fold(&name)) {
                continue;
            }
            seen.push(fold(&name));
            let mut cs: Vec<String> = Vec::new();
            for c in cols {
                if !cs.iter().any(|x| fold(x) == fold(&c)) {
                    cs.push(c);
                }
            }
            out.push((name, cs));
        }
        out
    })
}

/// A name as written in SQL: bare when plain lower case, else quoted for the dialect.
fn written(d: Dialect, name: &str) -> String {
    let plain = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain {
        name.to_owned()
    } else {
        match sqler_sql::quote_identifier(d, name) {
            Ok(quoted) => quoted,
            Err(e) => panic!("cannot quote {name:?}: {e}"),
        }
    }
}

/// One statement over the schema, built only from its names. `pick` drives every choice.
fn statement(
    tables: &[(String, Vec<String>)],
    d: Dialect,
    pick: &mut impl Iterator<Item = usize>,
) -> String {
    let mut next = |n: usize| pick.next().unwrap_or(0) % n.max(1);
    let first = next(tables.len());
    let mut chosen = vec![first];
    if tables.len() > 1 && next(2) == 1 {
        let mut second = next(tables.len());
        if second == first {
            second = (second + 1) % tables.len();
        }
        chosen.push(second);
    }
    // Aliases and the names the columns can be qualified with.
    let mut quals = Vec::new();
    let mut from = String::new();
    for (k, &t) in chosen.iter().enumerate() {
        let name = written(d, &tables[t].0);
        let alias = next(2) == 1;
        let q = if alias { format!("a{k}") } else { name.clone() };
        if k > 0 {
            let (qa, ca) = (&quals[0], &tables[chosen[0]].1[0]);
            let cb = &tables[t].1[0];
            let _ = write!(from, " JOIN {name}");
            if alias {
                let _ = write!(from, " AS {q}");
            }
            let _ = write!(from, " ON {qa}.{} = {q}.{}", written(d, ca), written(d, cb));
        } else {
            let _ = write!(from, "{name}");
            if alias {
                let _ = write!(from, " {q}");
            }
        }
        quals.push(q);
    }
    // A column is unqualified only if no other table in the statement has it.
    let all: Vec<(usize, &String)> = chosen
        .iter()
        .enumerate()
        .flat_map(|(k, &t)| tables[t].1.iter().map(move |c| (k, c)))
        .collect();
    let col = |n: &mut dyn FnMut(usize) -> usize| -> String {
        let (k, c) = all[n(all.len())];
        let unique = all
            .iter()
            .filter(|(_, o)| o.to_lowercase() == c.to_lowercase())
            .count()
            == 1;
        if unique && n(2) == 1 {
            written(d, c)
        } else {
            format!("{}.{}", quals[k], written(d, c))
        }
    };
    let items: Vec<String> = (0..=next(3)).map(|_| col(&mut next)).collect();
    let mut sql = format!("SELECT {} FROM {from}", items.join(", "));
    if next(2) == 1 {
        let _ = write!(sql, " WHERE {} = 1", col(&mut next));
    }
    if next(2) == 1 {
        let _ = write!(sql, " ORDER BY {}", col(&mut next));
    }
    sql
}

fn memory_schema(tables: &[(String, Vec<String>)]) -> MemorySchema {
    let mut s = MemorySchema::new(["public"]);
    for (name, cols) in tables {
        let cols = cols
            .iter()
            .map(|c| ColumnInfo::new(c.as_str(), DataType::Integer))
            .collect();
        s.add_table("public", name, ObjectKind::Table, Some(cols));
    }
    s
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// Text built only from schema names has no unknown-identifier diagnostics.
    #[test]
    fn schema_only_text_has_no_unknown_identifiers(
        tables in generated_schema(),
        picks in prop::collection::vec(any::<usize>(), 64),
        count in 1usize..4,
        d in dialect(),
    ) {
        let s = memory_schema(&tables);
        let mut it = picks.into_iter();
        let sql = (0..count)
            .map(|_| statement(&tables, d, &mut it))
            .collect::<Vec<_>>()
            .join("; ");
        for diag in diagnose(&sql, d, &s) {
            prop_assert!(
                !matches!(
                    diag.code,
                    DiagnosticCode::UnknownSchema
                        | DiagnosticCode::UnknownRelation
                        | DiagnosticCode::UnknownColumn
                        | DiagnosticCode::UnknownQualifier
                ),
                "{:?} in {}", diag, sql
            );
        }
    }
}
