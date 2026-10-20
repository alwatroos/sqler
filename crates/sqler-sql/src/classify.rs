// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Statement classification, token based and fail safe.
//!
//! A statement is [`StatementClass::Read`] only when it starts with a read verb and a scan of the
//! whole statement finds no write or DDL word, no function call outside a positive allowlist,
//! no `MySQL` executable comment, no `:=` and no unterminated token. Everything unknown is not
//! `Read`. `Read` therefore means "no write syntax found": it cannot see what a view, a
//! user-defined operator or a cast does, so callers that must be sure also run the statement in a
//! read-only transaction.

use crate::Dialect;
use crate::lexer::{Token, TokenKind, tokenize};
use crate::split::{Segment, SegmentKind, is_compound, split};

/// How dangerous a statement is, ordered by severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum StatementClass {
    /// Proven read-only.
    Read,
    /// Neither read nor write: transaction control, settings.
    Other,
    /// Changes data.
    Write,
    /// Changes the schema or permissions.
    Ddl,
    /// Not recognised; never treated as read.
    Unknown,
}

impl StatementClass {
    /// Whether the class is [`StatementClass::Read`].
    #[must_use]
    pub fn is_read_only(self) -> bool {
        self == Self::Read
    }

    /// A stable lowercase name: `read`, `other`, `write`, `ddl` or `unknown`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Other => "other",
            Self::Write => "write",
            Self::Ddl => "ddl",
            Self::Unknown => "unknown",
        }
    }
}

/// Classifies every statement of `text`; the most severe class wins, and no statement gives
/// [`StatementClass::Unknown`].
#[must_use]
pub fn classify(dialect: Dialect, text: &str) -> StatementClass {
    classify_each(dialect, text)
        .into_iter()
        .map(|(_, class)| class)
        .max()
        .unwrap_or(StatementClass::Unknown)
}

/// Classifies each statement segment of `text`, in order.
#[must_use]
pub fn classify_each(dialect: Dialect, text: &str) -> Vec<(Segment, StatementClass)> {
    split(dialect, text)
        .into_iter()
        .filter(|s| s.kind == SegmentKind::Statement)
        .map(|s| {
            let body = text.get(s.body.clone()).unwrap_or("");
            let class = classify_body(dialect, body);
            (s, class)
        })
        .collect()
}

const READ_LEADS: &[&str] = &[
    "SELECT", "WITH", "VALUES", "TABLE", "SHOW", "EXPLAIN", "DESCRIBE", "DESC",
];
const WRITE_LEADS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "UPSERT", "COPY", "LOAD", "REFRESH",
];
const DDL_LEADS: &[&str] = &[
    "CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME", "COMMENT", "GRANT", "REVOKE", "SECURITY",
    "IMPORT", "REINDEX",
];
const OTHER_LEADS: &[&str] = &[
    "BEGIN",
    "START",
    "COMMIT",
    "END",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
    "SET",
    "RESET",
    "USE",
    "PREPARE",
    "DEALLOCATE",
    "DISCARD",
    "LISTEN",
    "UNLISTEN",
    "NOTIFY",
    "CHECKPOINT",
    "VACUUM",
    "ANALYZE",
    "CLUSTER",
    "LOCK",
    "UNLOCK",
    "FLUSH",
    "KILL",
    "ATTACH",
    "DETACH",
    "OPTIMIZE",
    "REPAIR",
    "HANDLER",
];
/// Words that make a read candidate a write wherever they appear.
const WRITE_WORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "UPSERT", "INTO", "COPY", "LOAD", "LOCK", "SHARE",
];
/// Words that make a read candidate unknown wherever they appear.
const UNKNOWN_WORDS: &[&str] = &["CALL", "DO", "EXEC", "EXECUTE"];
/// Words that may be followed by `(` without being a function call.
const STRUCTURAL: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "IN",
    "EXISTS",
    "ANY",
    "ALL",
    "SOME",
    "AS",
    "ON",
    "USING",
    "JOIN",
    "LATERAL",
    "VALUES",
    "WHEN",
    "THEN",
    "ELSE",
    "CASE",
    "BY",
    "HAVING",
    "OVER",
    "FILTER",
    "GROUP",
    "PARTITION",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "LIMIT",
    "OFFSET",
    "DISTINCT",
    "IS",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "CAST",
    "EXTRACT",
    "ROW",
    "ARRAY",
    "EXPLAIN",
    "WITH",
    "MATERIALIZED",
    "RECURSIVE",
];
/// Functions known to have no side effects: explicit lowercase unqualified names, no prefixes.
const ALLOWED_FUNCTIONS: &[&str] = &[
    // aggregates and window functions
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "array_agg",
    "string_agg",
    "group_concat",
    "bool_and",
    "bool_or",
    "json_agg",
    "jsonb_agg",
    "stddev",
    "variance",
    "row_number",
    "rank",
    "dense_rank",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "ntile",
    // scalar functions
    "abs",
    "ceil",
    "ceiling",
    "floor",
    "round",
    "trunc",
    "mod",
    "power",
    "sqrt",
    "sign",
    "greatest",
    "least",
    "coalesce",
    "nullif",
    "ifnull",
    "iif",
    "if",
    "length",
    "char_length",
    "octet_length",
    "lower",
    "upper",
    "trim",
    "ltrim",
    "rtrim",
    "substr",
    "substring",
    "replace",
    "left",
    "right",
    "lpad",
    "rpad",
    "concat",
    "concat_ws",
    "position",
    "strpos",
    "instr",
    "reverse",
    "split_part",
    "regexp_replace",
    "to_char",
    "to_date",
    "to_timestamp",
    "date_trunc",
    "date_part",
    "age",
    "now",
    "current_date",
    "current_timestamp",
    "date",
    "time",
    "datetime",
    "julianday",
    "strftime",
    "date_format",
    "datediff",
    "year",
    "month",
    "day",
    "json_extract",
    "json_object",
    "json_array",
    "json_build_object",
    "jsonb_build_object",
    "to_json",
    "to_jsonb",
    "json_typeof",
    "jsonb_typeof",
    "generate_series",
    "unnest",
    "json_each",
    "jsonb_each",
    "json_array_elements",
    "md5",
    "hex",
    "typeof",
    "uuid",
    "gen_random_uuid",
    "random",
    "rand",
    "version",
    "current_database",
    "current_schema",
    "database",
    // type names with modifiers
    "varchar",
    "char",
    "character",
    "numeric",
    "decimal",
    "float",
    "double",
    "timestamp",
    "interval",
    "bit",
    "varbinary",
    "binary",
];
/// Longest allowlisted name; longer words skip lowercasing.
const MAX_NAME_LEN: usize = 32;
/// `SQLite` pragmas that only read.
const READ_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "database_list",
    "collation_list",
    "function_list",
    "module_list",
    "pragma_list",
    "compile_options",
];

fn in_list(text: &str, list: &[&str]) -> bool {
    list.iter().any(|w| text.eq_ignore_ascii_case(w))
}

fn is_punct(tok: &Token, src: &str, p: &str) -> bool {
    tok.kind == TokenKind::Punct && tok.text(src) == p
}

fn is_allowed_function(name: &str) -> bool {
    if name.len() > MAX_NAME_LEN {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    ALLOWED_FUNCTIONS.contains(&lower.as_str())
}

/// Classifies one statement body.
fn classify_body(dialect: Dialect, body: &str) -> StatementClass {
    let all = tokenize(dialect, body);
    let sig: Vec<Token> = all.iter().copied().filter(|t| !t.is_trivia()).collect();
    let Some(lead_idx) = sig.iter().position(|t| !is_punct(t, body, "(")) else {
        return StatementClass::Unknown;
    };
    let lead = sig[lead_idx];
    if lead.kind != TokenKind::Word {
        return StatementClass::Unknown;
    }
    let word = lead.text(body);
    if in_list(word, READ_LEADS) {
        // An unterminated token makes the statement's extent unreliable.
        return read_candidate(dialect, body, &all, &sig);
    }
    if word.eq_ignore_ascii_case("PRAGMA") {
        return if dialect == Dialect::Sqlite && all.iter().all(Token::is_terminated) {
            pragma_class(body, &sig[lead_idx + 1..])
        } else {
            StatementClass::Unknown
        };
    }
    if in_list(word, WRITE_LEADS) {
        StatementClass::Write
    } else if in_list(word, DDL_LEADS) {
        StatementClass::Ddl
    } else if in_list(word, OTHER_LEADS) {
        if word.eq_ignore_ascii_case("BEGIN") && is_compound(dialect, body, &all) {
            // `BEGIN NOT ATOMIC ... END` can hold anything.
            StatementClass::Unknown
        } else {
            StatementClass::Other
        }
    } else {
        StatementClass::Unknown
    }
}

/// `PRAGMA [schema.]name[(args)]` for a read-only pragma is `Read`; any other pragma is `Other`.
fn pragma_class(src: &str, rest: &[Token]) -> StatementClass {
    let mut i = 0;
    // Optional `schema.`.
    if rest.len() > 2 && rest[0].kind == TokenKind::Word && is_punct(&rest[1], src, ".") {
        i = 2;
    }
    let Some(name) = rest.get(i).filter(|t| t.kind == TokenKind::Word) else {
        return StatementClass::Other;
    };
    if !in_list(name.text(src), READ_PRAGMAS) {
        return StatementClass::Other;
    }
    let args = &rest[i + 1..];
    let plain = match args {
        [] => true,
        [open, inner @ .., close] => {
            is_punct(open, src, "(")
                && is_punct(close, src, ")")
                && inner.iter().all(|t| {
                    !is_punct(t, src, "(") && !is_punct(t, src, ")") && t.kind != TokenKind::Op
                })
        }
        [_] => false,
    };
    if plain {
        StatementClass::Read
    } else {
        StatementClass::Other
    }
}

/// A string whose lexing depends on a server mode the classifier does not model:
/// `MySQL` `NO_BACKSLASH_ESCAPES` and `ANSI_QUOTES`, `PostgreSQL` `standard_conforming_strings`.
/// Only a backslash (or, on `MySQL`, a `"` quote) makes the readings differ, so only those fail
/// safe. `SQLite` has one reading: a backslash is always literal.
fn mode_dependent_string(dialect: Dialect, src: &str, tok: &Token) -> bool {
    if !matches!(tok.kind, TokenKind::Str { .. }) {
        return false;
    }
    let text = tok.text(src);
    match dialect {
        Dialect::MySql => text.contains('\\') || text.starts_with('"') || text.starts_with("@\""),
        Dialect::Postgres => text.contains('\\') && !text.starts_with(['e', 'E']),
        Dialect::Sqlite => false,
    }
}

/// `MariaDB` sequence forms: `NEXT VALUE FOR s` advances the sequence (a write);
/// `PREVIOUS VALUE FOR s` reads session state (unknown).
fn sequence_form(src: &str, sig: &[Token], i: usize) -> Option<StatementClass> {
    let text = sig[i].text(src);
    let followed_by_value = sig.get(i + 1).is_some_and(|n| n.is_keyword(src, "VALUE"));
    if !followed_by_value {
        return None;
    }
    if text.eq_ignore_ascii_case("NEXT") {
        Some(StatementClass::Write)
    } else if text.eq_ignore_ascii_case("PREVIOUS") {
        Some(StatementClass::Unknown)
    } else {
        None
    }
}

/// Words that cannot start a function call: the previous token puts the next one in expression
/// position.
const EXPR_PREV: &[&str] = &[
    "SELECT", "DISTINCT", "ALL", "WHERE", "AND", "OR", "NOT", "ON", "WHEN", "THEN", "ELSE", "CASE",
    "HAVING", "BY", "LIMIT", "OFFSET", "BETWEEN", "IN", "IS", "LIKE", "ILIKE", "SOME", "ANY",
    "EXISTS", "AS", "USING",
];
/// Structural words that are also valid function names (unreserved or type/function keywords).
const POSITIONAL: &[&str] = &[
    "OVER",
    "FILTER",
    "BY",
    "PARTITION",
    "EXPLAIN",
    "MATERIALIZED",
    "RECURSIVE",
    "JOIN",
    "LIKE",
    "ILIKE",
    "IS",
];

/// Whether the word at `sig[i]`, followed by `(`, is syntax rather than a function call.
/// Words that can double as function names count only in their real syntactic position.
fn structural_here(src: &str, sig: &[Token], i: usize) -> bool {
    let text = sig[i].text(src);
    if !in_list(text, STRUCTURAL) {
        return false;
    }
    if !in_list(text, POSITIONAL) {
        return true;
    }
    let prev = if i > 0 { sig.get(i - 1) } else { None };
    let prev_is = |p: &str| prev.is_some_and(|t| is_punct(t, src, p));
    let prev_word =
        |w: &[&str]| prev.is_some_and(|t| t.kind == TokenKind::Word && in_list(t.text(src), w));
    let upper = text.to_ascii_uppercase();
    match upper.as_str() {
        "OVER" | "FILTER" => prev_is(")"),
        "BY" => prev_word(&["GROUP", "ORDER", "PARTITION"]),
        "EXPLAIN" => sig[..i].iter().all(|t| is_punct(t, src, "(")),
        "MATERIALIZED" => prev_word(&["AS", "NOT"]),
        "RECURSIVE" => prev_word(&["WITH"]),
        _ => {
            // PARTITION, JOIN, LIKE, ILIKE, IS: after an operand, not in expression position.
            prev.is_some_and(|t| {
                is_punct(t, src, ")")
                    || matches!(t.kind, TokenKind::QuotedIdent { .. } | TokenKind::Number)
                    || (t.kind == TokenKind::Word && !in_list(t.text(src), EXPR_PREV))
            })
        }
    }
}

/// Starts from `Read` and returns the most severe violation found anywhere in the statement.
fn read_candidate(dialect: Dialect, src: &str, all: &[Token], sig: &[Token]) -> StatementClass {
    let mut class = StatementClass::Read;
    for tok in all {
        if !tok.is_terminated() {
            return StatementClass::Unknown;
        }
        // `/*M!` is the `MariaDB` flavour of an executable comment.
        if matches!(tok.kind, TokenKind::ExecComment { .. })
            || (matches!(tok.kind, TokenKind::BlockComment { .. })
                && tok.text(src).starts_with("/*M!"))
        {
            return StatementClass::Unknown;
        }
    }
    if sig.iter().any(|t| mode_dependent_string(dialect, src, t)) {
        return StatementClass::Unknown;
    }
    for (i, tok) in sig.iter().enumerate() {
        let next_is_paren = sig.get(i + 1).is_some_and(|n| is_punct(n, src, "("));
        match tok.kind {
            // A stray character (NUL, backslash, a foreign quote) means the text is not plain SQL.
            TokenKind::Other => return StatementClass::Unknown,
            TokenKind::Op if tok.text(src) == ":=" => return StatementClass::Unknown,
            TokenKind::Word => {
                let text = tok.text(src);
                if in_list(text, UNKNOWN_WORDS) {
                    return StatementClass::Unknown;
                }
                if in_list(text, DDL_LEADS) {
                    class = class.max(StatementClass::Ddl);
                } else if in_list(text, WRITE_WORDS)
                    || (text.eq_ignore_ascii_case("REPLACE") && !next_is_paren)
                {
                    class = class.max(StatementClass::Write);
                }
                if let Some(c) = sequence_form(src, sig, i) {
                    class = class.max(c);
                }
                if next_is_paren && !structural_here(src, sig, i) {
                    let qualified = i > 0 && is_punct(&sig[i - 1], src, ".");
                    if qualified || !is_allowed_function(text) {
                        return StatementClass::Unknown;
                    }
                }
            }
            // A quoted function name never matches the allowlist.
            // A quoted name (or a `MySQL` double-quoted string) used as a call.
            TokenKind::QuotedIdent { .. } | TokenKind::Str { .. } if next_is_paren => {
                return StatementClass::Unknown;
            }
            _ => {}
        }
    }
    class
}
