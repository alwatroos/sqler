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

/// Leads that read when the statement passes the scan.
const READ_LEADS: &[&str] = &["SELECT", "WITH", "VALUES", "TABLE", "SHOW"];
/// Leads that wrap another statement: `EXPLAIN [ANALYZE] <stmt>` is classified as `<stmt>`.
const EXPLAIN_LEADS: &[&str] = &["EXPLAIN", "DESCRIBE", "DESC"];
/// Words skipped between an `EXPLAIN` lead and the statement it wraps.
const EXPLAIN_OPTIONS: &[&str] = &[
    "ANALYZE",
    "ANALYSE",
    "VERBOSE",
    "EXTENDED",
    "PARTITIONS",
    "QUERY",
    "PLAN",
    "FORMAT",
    "JSON",
    "TREE",
    "TRADITIONAL",
    "XML",
    "YAML",
    "TEXT",
    "FOR",
    "CONNECTION",
];
/// Leads that change data, storage or server state. Every lead that can mutate belongs here or in
/// [`DDL_LEADS`]; a lead in neither table (and not in [`OTHER_LEADS`]) is `Unknown`.
const WRITE_LEADS: &[&str] = &[
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "REPLACE",
    "UPSERT",
    "COPY",
    "LOAD",
    "REFRESH",
    "VACUUM",
    "ANALYZE",
    "CLUSTER",
    "LOCK",
    "FLUSH",
    "KILL",
    "ATTACH",
    "DETACH",
    "OPTIMIZE",
    "REPAIR",
    "CHECKPOINT",
    "NOTIFY",
    "INSTALL",
    "UNINSTALL",
    "PURGE",
];
const DDL_LEADS: &[&str] = &[
    "CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME", "COMMENT", "GRANT", "REVOKE", "SECURITY",
    "IMPORT", "REINDEX",
];
/// Session and transaction control. These run the same token scans as `Read`.
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
    "UNLOCK",
    "HANDLER",
];
/// T-SQL statements that change data, storage or server state, and exist only there.
const TSQL_WRITE_LEADS: &[&str] = &[
    "BULK",
    "DBCC",
    "BACKUP",
    "RESTORE",
    "RECONFIGURE",
    "SHUTDOWN",
    "RECEIVE",
    "SEND",
    "UPDATETEXT",
    "WRITETEXT",
    "MOVE",
];
/// T-SQL statements that change permissions or the state of a trigger or constraint.
const TSQL_DDL_LEADS: &[&str] = &["DENY", "ENABLE", "DISABLE"];
/// T-SQL statements that are neither read nor write: variables, messages, savepoints.
const TSQL_OTHER_LEADS: &[&str] = &["DECLARE", "PRINT", "SAVE"];
/// T-SQL words that take locks (table hints, isolation levels): a write wherever they appear.
const TSQL_LOCK_WORDS: &[&str] = &[
    "UPDLOCK",
    "XLOCK",
    "HOLDLOCK",
    "TABLOCK",
    "TABLOCKX",
    "SERIALIZABLE",
    "REPEATABLEREAD",
];
/// T-SQL separates statements by whitespace as well as by `;`, so `select 1 kill 52` runs two
/// statements. Any word that starts a statement which is not a read is therefore scanned for in
/// the whole text, at the severity of the statement it starts.
const TSQL_WRITE_WORDS: &[&str] = &["KILL", "CHECKPOINT"];
/// Statement starters that only add `ADD SIGNATURE` to the DDL leads already scanned for.
const TSQL_DDL_WORDS: &[&str] = &["ADD"];
/// Statement starters that change session state.
const TSQL_OTHER_WORDS: &[&str] = &["USE", "SET", "BEGIN", "COMMIT", "ROLLBACK"];
/// Statement starters the classifier does not understand: never read.
const TSQL_UNKNOWN_WORDS: &[&str] = &["OPEN", "SETUSER", "REVERT"];
/// Words that make a read candidate a write wherever they appear.
const WRITE_WORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "UPSERT", "INTO", "COPY", "LOAD", "LOCK", "SHARE",
    "REFRESH",
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
/// Functions that only T-SQL has and that have no side effects; used only on T-SQL.
const TSQL_FUNCTIONS: &[&str] = &[
    // dates and times
    "getdate",
    "getutcdate",
    "sysdatetime",
    "sysutcdatetime",
    "sysdatetimeoffset",
    "dateadd",
    "datefromparts",
    "datename",
    "datepart",
    "eomonth",
    // strings
    "isnull",
    "len",
    "datalength",
    "charindex",
    "patindex",
    "stuff",
    "str",
    "space",
    "replicate",
    "quotename",
    "unicode",
    "nchar",
    "ascii",
    "format",
    // conversions and maths
    "convert",
    "try_cast",
    "try_convert",
    "square",
    "choose",
    // metadata
    "newid",
    "object_id",
    "object_name",
    "object_schema_name",
    "schema_name",
    "schema_id",
    "db_name",
    "db_id",
    // aggregates and set functions
    "count_big",
    "string_split",
    "checksum",
    "binary_checksum",
    // type names with modifiers
    "nvarchar",
    "datetime2",
    "datetimeoffset",
    // `FOR XML` and `FOR JSON` options
    "path",
    "root",
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

fn is_allowed_function(dialect: Dialect, name: &str) -> bool {
    if name.len() > MAX_NAME_LEN {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    ALLOWED_FUNCTIONS.contains(&lower.as_str())
        || (dialect == Dialect::TSql && TSQL_FUNCTIONS.contains(&lower.as_str()))
}

/// Whether `word` leads a statement that changes data, storage or server state.
fn is_write_lead(dialect: Dialect, word: &str) -> bool {
    in_list(word, WRITE_LEADS) || (dialect == Dialect::TSql && in_list(word, TSQL_WRITE_LEADS))
}

/// Whether `word` leads a statement that changes the schema or permissions.
fn is_ddl_lead(dialect: Dialect, word: &str) -> bool {
    in_list(word, DDL_LEADS) || (dialect == Dialect::TSql && in_list(word, TSQL_DDL_LEADS))
}

/// Whether `word` leads a session or transaction control statement.
fn is_other_lead(dialect: Dialect, word: &str) -> bool {
    in_list(word, OTHER_LEADS) || (dialect == Dialect::TSql && in_list(word, TSQL_OTHER_LEADS))
}

/// T-SQL leads whose class depends on the next word. `None` for every other lead.
///
/// `WAITFOR DELAY|TIME` only waits (`Other`, after the scan); any other `WAITFOR` wraps a
/// statement such as `RECEIVE` (`Write`). `BEGIN TRAN[SACTION]` and `BEGIN DISTRIBUTED ..` (and a
/// bare `BEGIN`) are transaction control; any other `BEGIN` opens a block or a dialog and is
/// `Unknown`. `END CONVERSATION` ends a Service Broker conversation (`Write`).
fn tsql_lead_class(
    src: &str,
    all: &[Token],
    sig: &[Token],
    lead_idx: usize,
) -> Option<StatementClass> {
    let word = sig[lead_idx].text(src);
    let next = sig.get(lead_idx + 1);
    let next_is = |w: &str| next.is_some_and(|t| t.is_keyword(src, w));
    if word.eq_ignore_ascii_case("WAITFOR") {
        return Some(if next_is("DELAY") || next_is("TIME") {
            scan(Dialect::TSql, src, all, sig, StatementClass::Other)
        } else {
            StatementClass::Write
        });
    }
    if word.eq_ignore_ascii_case("BEGIN") {
        return Some(
            if next.is_none() || next_is("TRAN") || next_is("TRANSACTION") || next_is("DISTRIBUTED")
            {
                scan(Dialect::TSql, src, all, sig, StatementClass::Other)
            } else {
                StatementClass::Unknown
            },
        );
    }
    if word.eq_ignore_ascii_case("END") && next_is("CONVERSATION") {
        return Some(StatementClass::Write);
    }
    None
}

/// The class a T-SQL word forces wherever it appears in a statement, or `None`. `at_lead` is
/// whether the word is the statement's lead, which `tsql_lead_class` has already judged.
fn tsql_word_class(word: &str, at_lead: bool) -> Option<StatementClass> {
    if in_list(word, TSQL_LOCK_WORDS)
        || in_list(word, TSQL_WRITE_WORDS)
        || in_list(word, TSQL_WRITE_LEADS)
    {
        Some(StatementClass::Write)
    } else if word.eq_ignore_ascii_case("WAITFOR") {
        (!at_lead).then_some(StatementClass::Write)
    } else if in_list(word, TSQL_DDL_LEADS) || in_list(word, TSQL_DDL_WORDS) {
        Some(StatementClass::Ddl)
    } else if in_list(word, TSQL_UNKNOWN_WORDS) {
        Some(StatementClass::Unknown)
    } else if in_list(word, TSQL_OTHER_WORDS) || in_list(word, TSQL_OTHER_LEADS) {
        Some(StatementClass::Other)
    } else {
        None
    }
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
    if dialect == Dialect::TSql {
        // `EXEC` runs anything, also behind `INSERT .. EXEC`, `GRANT EXECUTE` and
        // `sp_executesql`; no statement that holds the word is understood.
        if sig
            .iter()
            .any(|t| t.is_keyword(body, "EXEC") || t.is_keyword(body, "EXECUTE"))
        {
            return StatementClass::Unknown;
        }
        if let Some(class) = tsql_lead_class(body, &all, &sig, lead_idx) {
            return class;
        }
    }
    if word.eq_ignore_ascii_case("PRAGMA") {
        return if dialect == Dialect::Sqlite && all.iter().all(Token::is_terminated) {
            pragma_class(body, &sig[lead_idx + 1..])
        } else {
            StatementClass::Unknown
        };
    }
    if in_list(word, READ_LEADS) {
        return scan(dialect, body, &all, &sig, StatementClass::Read);
    }
    if in_list(word, EXPLAIN_LEADS) {
        return explain_class(dialect, body, &all, &sig, lead_idx);
    }
    if is_write_lead(dialect, word) {
        return StatementClass::Write;
    }
    if is_ddl_lead(dialect, word) {
        return StatementClass::Ddl;
    }
    if !is_other_lead(dialect, word) {
        return StatementClass::Unknown;
    }
    let next = sig.get(lead_idx + 1);
    let next_is = |w: &str| next.is_some_and(|t| t.is_keyword(body, w));
    let base = if word.eq_ignore_ascii_case("BEGIN") && is_compound(dialect, body, &all) {
        // `BEGIN NOT ATOMIC ... END` can hold anything.
        return StatementClass::Unknown;
    } else if word.eq_ignore_ascii_case("START") && !next_is("TRANSACTION") {
        // `START REPLICA` and friends change server state.
        return StatementClass::Unknown;
    } else if word.eq_ignore_ascii_case("RESET") && dialect != Dialect::Postgres {
        // `RESET MASTER` and `RESET REPLICA` destroy server state.
        return StatementClass::Write;
    } else if word.eq_ignore_ascii_case("SET")
        && (next_is("PASSWORD")
            || sig.iter().any(|t| {
                // Any assignment in the list may be server-wide: `SET a = 1, GLOBAL b = 2`.
                let text = t.text(body).to_ascii_lowercase();
                (t.kind == TokenKind::Word
                    && ["global", "persist", "persist_only"].contains(&text.as_str()))
                    || text.starts_with("@@global")
                    || text.starts_with("@@persist")
            }))
    {
        StatementClass::Write
    } else {
        StatementClass::Other
    };
    scan(dialect, body, &all, &sig, base)
}

/// `t`, `s.t`, optionally followed by a column or pattern: nothing that looks like a statement.
fn is_table_name(src: &str, rest: &[Token]) -> bool {
    rest.len() <= 4
        && rest.iter().all(|t| {
            matches!(
                t.kind,
                TokenKind::Word
                    | TokenKind::Number
                    | TokenKind::QuotedIdent { .. }
                    | TokenKind::Str { .. }
            ) || is_punct(t, src, ".")
        })
}

/// `EXPLAIN [ANALYZE] [(options)] <stmt>` is the class of `<stmt>`: `ANALYZE` executes it. When
/// the wrapped statement is not a known lead the statement is `Unknown`, except that `MySQL`
/// `EXPLAIN t` and `DESCRIBE t` describe a table.
fn explain_class(
    dialect: Dialect,
    src: &str,
    all: &[Token],
    sig: &[Token],
    lead_idx: usize,
) -> StatementClass {
    let describes_table = dialect == Dialect::MySql || !sig[lead_idx].is_keyword(src, "EXPLAIN");
    let mut i = lead_idx + 1;
    while let Some(t) = sig.get(i) {
        if is_punct(t, src, "(") {
            let mut depth = 0usize;
            loop {
                let Some(t) = sig.get(i) else {
                    return StatementClass::Unknown;
                };
                if is_punct(t, src, "(") {
                    depth += 1;
                } else if is_punct(t, src, ")") {
                    depth -= 1;
                }
                i += 1;
                if depth == 0 {
                    break;
                }
            }
        } else if (t.kind == TokenKind::Word && in_list(t.text(src), EXPLAIN_OPTIONS))
            || (t.kind == TokenKind::Op && t.text(src) == "=")
        {
            i += 1;
        } else {
            break;
        }
    }
    let inner = match sig.get(i) {
        Some(t) if t.kind == TokenKind::Word => {
            let w = t.text(src);
            if in_list(w, READ_LEADS) {
                StatementClass::Read
            } else if is_write_lead(dialect, w) {
                StatementClass::Write
            } else if is_ddl_lead(dialect, w) {
                StatementClass::Ddl
            } else if is_other_lead(dialect, w) {
                StatementClass::Other
            } else if describes_table && !in_list(w, EXPLAIN_LEADS) && is_table_name(src, &sig[i..])
            {
                StatementClass::Read
            } else {
                StatementClass::Unknown
            }
        }
        Some(_) if describes_table && is_table_name(src, &sig[i..]) => StatementClass::Read,
        None if describes_table => StatementClass::Read,
        _ => StatementClass::Unknown,
    };
    inner.max(scan(dialect, src, all, sig, StatementClass::Read))
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
        // T-SQL strings have one reading: no backslash escapes, `''` doubles the quote.
        // (`QUOTED_IDENTIFIER OFF` reads `".."` as a string; both readings are one token that
        // doubles its quote, so the token boundaries do not change.)
        Dialect::Sqlite | Dialect::TSql => false,
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

/// The unquoted text of a word or quoted name, lower-cased.
fn bare_name(src: &str, tok: &Token) -> String {
    tok.text(src)
        .trim_matches(['`', '"', '\'', '[', ']'])
        .to_ascii_lowercase()
}

/// Names that run without parentheses and may write or read hidden state: `SQLite` `pragma_*`
/// table-valued functions (`pragma_optimize` writes `sqlite_stat1`) and the writable
/// `sqlite_dbpage`; `MariaDB` sequence members (`seq.nextval` in `sql_mode=ORACLE`, bare `nextval`).
fn parenless_hazard(dialect: Dialect, src: &str, sig: &[Token], i: usize) -> bool {
    if matches!(sig[i].kind, TokenKind::Str { .. }) && dialect != Dialect::Sqlite {
        return false;
    }
    let name = bare_name(src, &sig[i]);
    let qualified = i > 0 && is_punct(&sig[i - 1], src, ".");
    match dialect {
        Dialect::Sqlite => name.starts_with("pragma_") || name == "sqlite_dbpage",
        Dialect::MySql => {
            matches!(name.as_str(), "nextval" | "currval" | "lastval")
                || (qualified && name == "previous")
        }
        Dialect::Postgres | Dialect::TSql => false,
    }
}

/// Words after which a table, expression or operator may start, so a following structural word
/// is a function name: they are never operands. Everything in [`STRUCTURAL`] counts as well.
const OPENER_WORDS: &[&str] = &["ONLY", "ORDER", "WINDOW"];

/// Whether `prev` can end an operand: a closing parenthesis, a literal, a quoted name, or a plain
/// word that is neither structural nor an opener (a column, table, alias or `NULL`).
fn ends_operand(src: &str, prev: Option<&Token>) -> bool {
    prev.is_some_and(|t| match t.kind {
        TokenKind::QuotedIdent { .. } | TokenKind::Number | TokenKind::Str { .. } => true,
        TokenKind::Punct => t.text(src) == ")",
        TokenKind::Word => !in_list(t.text(src), STRUCTURAL) && !in_list(t.text(src), OPENER_WORDS),
        _ => false,
    })
}

/// Whether a word of `kind` appears at depth one of the parentheses opened at `sig[open]`.
fn has_word_at_depth_one(src: &str, sig: &[Token], open: usize, word: &str) -> bool {
    let mut depth = 0usize;
    for t in &sig[open..] {
        if is_punct(t, src, "(") {
            depth += 1;
        } else if is_punct(t, src, ")") {
            depth -= 1;
            if depth == 0 {
                return false;
            }
        } else if depth == 1 && t.is_keyword(src, word) {
            return true;
        }
    }
    false
}

/// Whether the word at `sig[i]`, followed by `(`, is syntax rather than a function call.
///
/// A call is the default. A word is syntax only when it is in [`STRUCTURAL`], is not qualified
/// (a name after `.` is always a function name) and, for words that the servers also accept as
/// function names, sits where the grammar puts it: this match is the explicit table of allowed
/// positions, keyed by the previous token.
fn structural_here(dialect: Dialect, src: &str, sig: &[Token], i: usize) -> bool {
    let text = sig[i].text(src);
    let prev = if i > 0 { sig.get(i - 1) } else { None };
    let prev_word =
        |w: &[&str]| prev.is_some_and(|t| t.kind == TokenKind::Word && in_list(t.text(src), w));
    if dialect == Dialect::TSql
        && !prev.is_some_and(|t| is_punct(t, src, "."))
        && in_list(text, &["TOP", "OPTION"])
    {
        // `SELECT TOP (n)`, and `OPTION (hints)` after the last operand of a query.
        return if text.eq_ignore_ascii_case("TOP") {
            prev_word(&["SELECT", "DISTINCT", "ALL"])
        } else {
            ends_operand(src, prev)
        };
    }
    if prev.is_some_and(|t| is_punct(t, src, ".")) || !in_list(text, STRUCTURAL) {
        return false;
    }
    match text.to_ascii_uppercase().as_str() {
        "OVER" | "FILTER" => prev.is_some_and(|t| is_punct(t, src, ")")),
        "BY" => prev_word(&["GROUP", "ORDER", "PARTITION"]),
        "EXPLAIN" => sig[..i].iter().all(|t| is_punct(t, src, "(")),
        // `AS MATERIALIZED` or `AS NOT MATERIALIZED`.
        "MATERIALIZED" => {
            prev_word(&["AS"]) || (prev_word(&["NOT"]) && i > 1 && sig[i - 2].is_keyword(src, "AS"))
        }
        "RECURSIVE" => prev_word(&["WITH"]),
        "CAST" => has_word_at_depth_one(src, sig, i + 1, "AS"),
        "EXTRACT" => has_word_at_depth_one(src, sig, i + 1, "FROM"),
        // After an operand only: `FROM partition(1)` and `JOIN join(1)` are calls.
        "PARTITION" | "JOIN" | "LIKE" | "ILIKE" | "IS" | "OFFSET" => ends_operand(src, prev),
        _ => true,
    }
}

/// Starts from `base` (`Read` or `Other`) and returns the most severe violation found anywhere in
/// the statement; any construct the classifier cannot trust gives `Unknown`.
fn scan(
    dialect: Dialect,
    src: &str,
    all: &[Token],
    sig: &[Token],
    base: StatementClass,
) -> StatementClass {
    let mut class = base;
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
    let lead_pos = sig.iter().position(|t| !is_punct(t, src, "(")).unwrap_or(0);
    for (i, tok) in sig.iter().enumerate() {
        let next_is_paren = sig.get(i + 1).is_some_and(|n| is_punct(n, src, "("));
        match tok.kind {
            // A stray character (NUL, backslash, a foreign quote) means the text is not plain SQL.
            TokenKind::Other => return StatementClass::Unknown,
            TokenKind::Op if tok.text(src) == ":=" => return StatementClass::Unknown,
            // `SQLite` also accepts a single-quoted string as a table name.
            TokenKind::Word | TokenKind::QuotedIdent { .. } | TokenKind::Str { .. }
                if parenless_hazard(dialect, src, sig, i) =>
            {
                return StatementClass::Unknown;
            }
            TokenKind::Word => {
                let text = tok.text(src);
                if in_list(text, UNKNOWN_WORDS) {
                    return StatementClass::Unknown;
                }
                if dialect == Dialect::TSql
                    && let Some(c) = tsql_word_class(text, i == lead_pos)
                {
                    if c == StatementClass::Unknown {
                        return c;
                    }
                    class = class.max(c);
                }
                // `END` also closes a `CASE`; `END CONVERSATION` is a statement.
                if dialect == Dialect::TSql
                    && text.eq_ignore_ascii_case("END")
                    && sig
                        .get(i + 1)
                        .is_some_and(|n| n.is_keyword(src, "CONVERSATION"))
                {
                    class = class.max(StatementClass::Write);
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
                if next_is_paren && !structural_here(dialect, src, sig, i) {
                    let qualified = i > 0 && is_punct(&sig[i - 1], src, ".");
                    if qualified || !is_allowed_function(dialect, text) {
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
