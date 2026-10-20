// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Per-dialect static data (keywords, functions, snippets) and identifier rules.

use std::borrow::Cow;

use sqler_sql::Dialect;

use crate::scope::Ident;

/// The three dialect families the data tables cover.
///
/// `Dialect` is `#[non_exhaustive]`; unknown dialects use the `PostgreSQL` rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    Postgres,
    MySql,
    Sqlite,
}

pub(crate) fn flavor(dialect: Dialect) -> Flavor {
    match dialect {
        Dialect::MySql => Flavor::MySql,
        Dialect::Sqlite => Flavor::Sqlite,
        _ => Flavor::Postgres,
    }
}

/// A built-in function: name and a short signature shown as detail.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Func {
    pub name: &'static str,
    pub sig: &'static str,
    /// Whether the function takes no arguments (inserted as `name()`).
    pub nullary: bool,
}

/// A snippet: trigger, description and body with LSP tab stops.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Snippet {
    pub trigger: &'static str,
    pub detail: &'static str,
    pub body: &'static str,
}

const fn f(name: &'static str, sig: &'static str) -> Func {
    Func {
        name,
        sig,
        nullary: false,
    }
}

const fn f0(name: &'static str, sig: &'static str) -> Func {
    Func {
        name,
        sig,
        nullary: true,
    }
}

/// Fold used for case-insensitive comparison (single char, Unicode lowercase).
pub(crate) fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Case-insensitive equality (Unicode lowercase), without allocating.
pub(crate) fn eq_fold(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    a.chars().map(fold).eq(b.chars().map(fold))
}

fn lower(s: &str) -> String {
    s.chars().map(fold).collect()
}

/// `PostgreSQL` folds unquoted identifiers with ASCII rules only: non-ASCII letters keep their case.
fn pg_fold(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// The identifier as the dialect stores it: `PostgreSQL` folds unquoted names to lower case, the
/// others compare case-insensitively (lower case here).
pub(crate) fn normalize(dialect: Dialect, id: &Ident) -> String {
    match flavor(dialect) {
        Flavor::Postgres if id.quoted => id.value.clone(),
        Flavor::Postgres => pg_fold(&id.value),
        _ => lower(&id.value),
    }
}

/// The name an identifier written in the SQL text denotes, as it must be inserted: `PostgreSQL`
/// folds an unquoted name to lower case, a quoted name keeps its case; the other dialects keep
/// the name as written (their case rules depend on the server and the object).
pub(crate) fn text_name(dialect: Dialect, id: &Ident) -> Cow<'_, str> {
    if flavor(dialect) == Flavor::Postgres
        && !id.quoted
        && id.value.bytes().any(|b| b.is_ascii_uppercase())
    {
        Cow::Owned(pg_fold(&id.value))
    } else {
        Cow::Borrowed(id.value.as_str())
    }
}

/// Whether two identifiers from SQL text name the same object.
pub(crate) fn idents_match(dialect: Dialect, a: &Ident, b: &Ident) -> bool {
    normalize(dialect, a) == normalize(dialect, b)
}

/// Whether the identifier `id`, as typed, names the stored name `stored`.
///
/// `PostgreSQL`: an unquoted name is folded to ASCII lower case and compared exactly; a quoted name is
/// compared exactly. `MySQL` and `SQLite`: case-insensitive.
///
/// [`find_stored_by`] applies the same rule to many candidates without allocating per candidate.
#[cfg(test)]
pub(crate) fn ident_eq(dialect: Dialect, id: &Ident, stored: &str) -> bool {
    match flavor(dialect) {
        Flavor::Postgres if id.quoted => id.value == stored,
        Flavor::Postgres => pg_fold(&id.value) == stored,
        _ => eq_fold(&id.value, stored),
    }
}

/// Whether the identifier `id`, as typed, names exactly the stored name `stored`, with no
/// lenient fallback: `PostgreSQL` folds an unquoted name to ASCII lower case and compares exactly,
/// a quoted name is compared exactly; `MySQL` and `SQLite` compare case-insensitively.
pub(crate) fn stored_eq_strict(dialect: Dialect, id: &Ident, stored: &str) -> bool {
    match flavor(dialect) {
        Flavor::Postgres if id.quoted => id.value == stored,
        Flavor::Postgres => {
            id.value.len() == stored.len()
                && id
                    .value
                    .bytes()
                    .zip(stored.bytes())
                    .all(|(a, b)| a.to_ascii_lowercase() == b)
        }
        _ => eq_fold(&id.value, stored),
    }
}

/// A hash of the case-folded name; equal for names that [`eq_fold`] treats as equal, so it keys
/// buckets in which the strict or lenient comparison then decides.
pub(crate) fn fold_hash(name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in name.chars() {
        h ^= u64::from(u32::from(fold(c)));
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Finds the stored item for a typed identifier: a strict match first, a case-insensitive match
/// as a lenient fallback. `key` returns the stored name of an item.
pub(crate) fn find_stored_by<T>(
    dialect: Dialect,
    id: &Ident,
    items: impl Iterator<Item = T>,
    key: impl Fn(&T) -> &str,
) -> Option<T> {
    let folded = if flavor(dialect) == Flavor::Postgres && !id.quoted {
        Some(pg_fold(&id.value))
    } else {
        None
    };
    let mut loose = None;
    for item in items {
        let name = key(&item);
        let strict = match (&folded, flavor(dialect)) {
            (Some(f), _) => name == f,
            (None, Flavor::Postgres) => name == id.value,
            (None, _) => eq_fold(name, &id.value),
        };
        if strict {
            return Some(item);
        }
        if loose.is_none() && eq_fold(name, &id.value) {
            loose = Some(item);
        }
    }
    loose
}

/// [`find_stored_by`] over plain names.
pub(crate) fn find_stored<'a>(
    dialect: Dialect,
    id: &Ident,
    names: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    find_stored_by(dialect, id, names, |n| n)
}

/// Whether `name` must be quoted to be used as an identifier.
pub(crate) fn needs_quotes(dialect: Dialect, name: &str) -> bool {
    let fl = flavor(dialect);
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| {
        c == '_'
            || if fl == Flavor::Postgres {
                c.is_ascii_lowercase()
            } else {
                c.is_ascii_alphabetic()
            }
    });
    let rest_ok = chars.all(|c| {
        c == '_'
            || c == '$'
            || c.is_ascii_digit()
            || if fl == Flavor::Postgres {
                c.is_ascii_lowercase()
            } else {
                c.is_ascii_alphabetic()
            }
    });
    !(first_ok && rest_ok) || is_reserved(dialect, name)
}

/// Quotes `name` when needed (always when `force`). `None` when the name cannot be quoted (empty
/// or containing NUL): the caller drops the candidate rather than insert it raw.
pub(crate) fn quote(dialect: Dialect, name: &str, force: bool) -> Option<String> {
    if force || needs_quotes(dialect, name) {
        sqler_sql::quote_identifier(dialect, name).ok()
    } else {
        Some(name.to_owned())
    }
}

/// Whether `word` is reserved in the dialect (case-insensitive).
pub(crate) fn is_reserved(dialect: Dialect, word: &str) -> bool {
    let extra: &[&str] = match flavor(dialect) {
        Flavor::Postgres => PG_RESERVED,
        Flavor::MySql => MYSQL_RESERVED,
        Flavor::Sqlite => SQLITE_RESERVED,
    };
    COMMON_RESERVED
        .iter()
        .chain(extra)
        .any(|r| r.eq_ignore_ascii_case(word))
}

const COMMON_RESERVED: &[&str] = &[
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "BETWEEN",
    "BY",
    "CASE",
    "CHECK",
    "COLLATE",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "FALSE",
    "FOREIGN",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IN",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NATURAL",
    "NOT",
    "NULL",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "PRIMARY",
    "REFERENCES",
    "RIGHT",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TO",
    "TRUE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
    "WITH",
];

const PG_RESERVED: &[&str] = &[
    "ANALYSE",
    "ANALYZE",
    "ANY",
    "ARRAY",
    "COLUMN",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "FETCH",
    "FOR",
    "GRANT",
    "LATERAL",
    "OFFSET",
    "ONLY",
    "RETURNING",
    "SOME",
    "SYMMETRIC",
    "TRAILING",
    "USER",
    "VARIADIC",
    "WINDOW",
];

const MYSQL_RESERVED: &[&str] = &[
    "ADD",
    "CONDITION",
    "DATABASE",
    "DATABASES",
    "DESCRIBE",
    "DIV",
    "EXPLAIN",
    "FETCH",
    "FOR",
    "GRANT",
    "IGNORE",
    "INDEX",
    "KEY",
    "KEYS",
    "LOCK",
    "MOD",
    "OFFSET",
    "OPTION",
    "RANGE",
    "RANK",
    "REGEXP",
    "RENAME",
    "REPLACE",
    "RLIKE",
    "SCHEMA",
    "SHOW",
    "UNSIGNED",
    "USE",
    "WINDOW",
];

const SQLITE_RESERVED: &[&str] = &[
    "ADD",
    "AUTOINCREMENT",
    "COLUMN",
    "COMMIT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DATABASE",
    "FILTER",
    "GLOB",
    "INDEX",
    "OFFSET",
    "OVER",
    "RENAME",
    "REPLACE",
    "ROLLBACK",
    "TRANSACTION",
    "WINDOW",
];

const COMMON_KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FILTER",
    "FIRST",
    "FOLLOWING",
    "FOREIGN",
    "FROM",
    "FULL",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LAST",
    "LATERAL",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NATURAL",
    "NEXT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "ONLY",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRECEDING",
    "PRIMARY",
    "RANGE",
    "RECURSIVE",
    "REFERENCES",
    "RENAME",
    "RETURNING",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SELECT",
    "SET",
    "TABLE",
    "TEMP",
    "TEMPORARY",
    "THEN",
    "TO",
    "TRUE",
    "TRUNCATE",
    "UNBOUNDED",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
];

const PG_KEYWORDS: &[&str] = &[
    "ANALYZE",
    "ARRAY",
    "BIGSERIAL",
    "CONCURRENTLY",
    "CONFLICT",
    "COPY",
    "DECLARE",
    "DO",
    "EXTENSION",
    "FUNCTION",
    "ILIKE",
    "INHERITS",
    "JSONB",
    "LANGUAGE",
    "MATERIALIZED",
    "NOTHING",
    "NULLS",
    "PERFORM",
    "PROCEDURE",
    "RAISE",
    "SCHEMA",
    "SEQUENCE",
    "SERIAL",
    "SIMILAR",
    "TABLESPACE",
    "TRIGGER",
    "TYPE",
    "UNLOGGED",
    "VACUUM",
];

const MYSQL_KEYWORDS: &[&str] = &[
    "AUTO_INCREMENT",
    "CHARSET",
    "DATABASE",
    "DELIMITER",
    "DESCRIBE",
    "DUPLICATE",
    "ENGINE",
    "FUNCTION",
    "IGNORE",
    "LOCK",
    "PROCEDURE",
    "REGEXP",
    "REPLACE",
    "RLIKE",
    "SHOW",
    "STRAIGHT_JOIN",
    "TABLES",
    "TRIGGER",
    "UNLOCK",
    "UNSIGNED",
    "USE",
];

const SQLITE_KEYWORDS: &[&str] = &[
    "ANALYZE",
    "ATTACH",
    "AUTOINCREMENT",
    "CONFLICT",
    "DETACH",
    "DO",
    "GLOB",
    "INDEXED",
    "NOTHING",
    "PLAN",
    "PRAGMA",
    "QUERY",
    "REINDEX",
    "REPLACE",
    "ROWID",
    "STRICT",
    "TRIGGER",
    "VACUUM",
    "VIRTUAL",
    "WITHOUT",
];

/// Every keyword of the dialect, upper case.
pub(crate) fn keywords(dialect: Dialect) -> impl Iterator<Item = &'static str> {
    let extra: &'static [&'static str] = match flavor(dialect) {
        Flavor::Postgres => PG_KEYWORDS,
        Flavor::MySql => MYSQL_KEYWORDS,
        Flavor::Sqlite => SQLITE_KEYWORDS,
    };
    COMMON_KEYWORDS.iter().chain(extra).copied()
}

/// Keywords that can start a statement.
pub(crate) fn statement_keywords(dialect: Dialect) -> &'static [&'static str] {
    match flavor(dialect) {
        Flavor::Postgres => &[
            "SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "TRUNCATE",
            "EXPLAIN", "BEGIN", "COMMIT", "ROLLBACK", "VALUES", "SET", "SHOW", "GRANT", "REVOKE",
            "ANALYZE", "VACUUM", "COPY", "TABLE",
        ],
        Flavor::MySql => &[
            "SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "REPLACE", "CREATE", "ALTER", "DROP",
            "TRUNCATE", "EXPLAIN", "DESCRIBE", "SHOW", "USE", "SET", "BEGIN", "COMMIT", "ROLLBACK",
            "VALUES", "GRANT", "REVOKE", "LOCK", "UNLOCK", "TABLE",
        ],
        Flavor::Sqlite => &[
            "SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "REPLACE", "CREATE", "ALTER", "DROP",
            "EXPLAIN", "PRAGMA", "BEGIN", "COMMIT", "ROLLBACK", "VALUES", "ATTACH", "DETACH",
            "VACUUM", "REINDEX", "ANALYZE",
        ],
    }
}

/// Keywords that can follow a complete relation reference, most useful first.
pub(crate) const TAIL_KEYWORDS: &[&str] = &[
    "JOIN",
    "LEFT JOIN",
    "INNER JOIN",
    "RIGHT JOIN",
    "FULL JOIN",
    "CROSS JOIN",
    "ON",
    "USING",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "AS",
    "UNION",
    "UNION ALL",
    "INTERSECT",
    "EXCEPT",
    "OFFSET",
    "FETCH",
    "FOR",
    "WINDOW",
    "SET",
    "VALUES",
    "SELECT",
    "RETURNING",
    "INTO",
    "FROM",
    "LEFT OUTER JOIN",
    "NATURAL JOIN",
];

/// Keywords that are valid where a relation is expected.
pub(crate) const RELATION_KEYWORDS: &[&str] = &["LATERAL", "ONLY", "SELECT", "VALUES"];

const COMMON_FUNCS: &[Func] = &[
    f("count", "count(expr) -> integer"),
    f("sum", "sum(expr) -> number"),
    f("avg", "avg(expr) -> number"),
    f("min", "min(expr) -> same type"),
    f("max", "max(expr) -> same type"),
    f("coalesce", "coalesce(a, b, ...) -> first non-null"),
    f("nullif", "nullif(a, b) -> null if a = b"),
    f("abs", "abs(x) -> number"),
    f("round", "round(x [, digits]) -> number"),
    f("ceil", "ceil(x) -> number"),
    f("floor", "floor(x) -> number"),
    f("power", "power(x, y) -> number"),
    f("sqrt", "sqrt(x) -> number"),
    f("exp", "exp(x) -> number"),
    f("ln", "ln(x) -> number"),
    f("sin", "sin(x) -> number"),
    f("cos", "cos(x) -> number"),
    f("sign", "sign(x) -> integer"),
    f("lower", "lower(text) -> text"),
    f("upper", "upper(text) -> text"),
    f("trim", "trim(text) -> text"),
    f("ltrim", "ltrim(text [, chars]) -> text"),
    f("rtrim", "rtrim(text [, chars]) -> text"),
    f("replace", "replace(text, from, to) -> text"),
    f("substr", "substr(text, start [, len]) -> text"),
    f("row_number", "row_number() over (...) -> integer"),
    f("rank", "rank() over (...) -> integer"),
    f("dense_rank", "dense_rank() over (...) -> integer"),
    f("lag", "lag(expr [, offset [, default]]) over (...)"),
    f("lead", "lead(expr [, offset [, default]]) over (...)"),
    f("first_value", "first_value(expr) over (...)"),
    f("last_value", "last_value(expr) over (...)"),
    f("ntile", "ntile(n) over (...) -> integer"),
];

const PG_FUNCS: &[Func] = &[
    f("array_agg", "array_agg(expr) -> array"),
    f("string_agg", "string_agg(expr, delimiter) -> text"),
    f("bool_and", "bool_and(expr) -> boolean"),
    f("bool_or", "bool_or(expr) -> boolean"),
    f("greatest", "greatest(a, b, ...) -> same type"),
    f("least", "least(a, b, ...) -> same type"),
    f("length", "length(text) -> integer"),
    f("char_length", "char_length(text) -> integer"),
    f("concat", "concat(a, b, ...) -> text"),
    f("concat_ws", "concat_ws(sep, a, b, ...) -> text"),
    f("left", "left(text, n) -> text"),
    f("right", "right(text, n) -> text"),
    f("lpad", "lpad(text, len [, fill]) -> text"),
    f("rpad", "rpad(text, len [, fill]) -> text"),
    f("position", "position(sub in text) -> integer"),
    f("split_part", "split_part(text, delimiter, n) -> text"),
    f(
        "regexp_replace",
        "regexp_replace(text, pattern, repl [, flags])",
    ),
    f("regexp_match", "regexp_match(text, pattern) -> text[]"),
    f("starts_with", "starts_with(text, prefix) -> boolean"),
    f("initcap", "initcap(text) -> text"),
    f("md5", "md5(text) -> text"),
    f("to_char", "to_char(value, format) -> text"),
    f("to_date", "to_date(text, format) -> date"),
    f("to_timestamp", "to_timestamp(text, format) -> timestamptz"),
    f("to_number", "to_number(text, format) -> numeric"),
    f0("now", "now() -> timestamptz"),
    f0("current_timestamp", "current_timestamp -> timestamptz"),
    f0("clock_timestamp", "clock_timestamp() -> timestamptz"),
    f("date_trunc", "date_trunc(field, source) -> timestamp"),
    f("date_part", "date_part(field, source) -> double"),
    f("extract", "extract(field from source) -> numeric"),
    f("age", "age(timestamp [, timestamp]) -> interval"),
    f("make_date", "make_date(year, month, day) -> date"),
    f(
        "generate_series",
        "generate_series(start, stop [, step]) -> setof",
    ),
    f("unnest", "unnest(array) -> setof"),
    f("array_length", "array_length(array, dim) -> integer"),
    f("array_append", "array_append(array, elem) -> array"),
    f(
        "jsonb_build_object",
        "jsonb_build_object(k, v, ...) -> jsonb",
    ),
    f("jsonb_build_array", "jsonb_build_array(...) -> jsonb"),
    f("jsonb_agg", "jsonb_agg(expr) -> jsonb"),
    f(
        "jsonb_array_elements",
        "jsonb_array_elements(jsonb) -> setof jsonb",
    ),
    f("jsonb_each", "jsonb_each(jsonb) -> setof record"),
    f(
        "jsonb_extract_path",
        "jsonb_extract_path(jsonb, path, ...) -> jsonb",
    ),
    f("jsonb_set", "jsonb_set(jsonb, path, value) -> jsonb"),
    f("json_agg", "json_agg(expr) -> json"),
    f("to_jsonb", "to_jsonb(value) -> jsonb"),
    f("random", "random() -> double"),
    f("mod", "mod(a, b) -> number"),
    f("trunc", "trunc(x [, digits]) -> number"),
    f("gen_random_uuid", "gen_random_uuid() -> uuid"),
    f("pg_typeof", "pg_typeof(value) -> regtype"),
    f("current_setting", "current_setting(name) -> text"),
    f0("current_database", "current_database() -> name"),
    f0("current_schema", "current_schema() -> name"),
    f(
        "percentile_cont",
        "percentile_cont(fraction) within group (...)",
    ),
    f("stddev", "stddev(expr) -> number"),
    f("variance", "variance(expr) -> number"),
];

const MYSQL_FUNCS: &[Func] = &[
    f("group_concat", "group_concat(expr [separator s]) -> text"),
    f("ifnull", "ifnull(a, b) -> a if not null"),
    f("if", "if(cond, then, else) -> value"),
    f("greatest", "greatest(a, b, ...) -> same type"),
    f("least", "least(a, b, ...) -> same type"),
    f("length", "length(text) -> bytes"),
    f("char_length", "char_length(text) -> characters"),
    f("concat", "concat(a, b, ...) -> text"),
    f("concat_ws", "concat_ws(sep, a, b, ...) -> text"),
    f("left", "left(text, n) -> text"),
    f("right", "right(text, n) -> text"),
    f("lpad", "lpad(text, len, fill) -> text"),
    f("rpad", "rpad(text, len, fill) -> text"),
    f("locate", "locate(sub, text [, pos]) -> integer"),
    f("instr", "instr(text, sub) -> integer"),
    f("substring", "substring(text, pos [, len]) -> text"),
    f(
        "substring_index",
        "substring_index(text, delim, count) -> text",
    ),
    f("reverse", "reverse(text) -> text"),
    f("repeat", "repeat(text, n) -> text"),
    f("format", "format(x, decimals) -> text"),
    f("regexp_replace", "regexp_replace(text, pattern, repl)"),
    f("regexp_like", "regexp_like(text, pattern) -> boolean"),
    f("md5", "md5(text) -> text"),
    f("sha2", "sha2(text, bits) -> text"),
    f0("now", "now() -> datetime"),
    f0("curdate", "curdate() -> date"),
    f0("curtime", "curtime() -> time"),
    f0("current_timestamp", "current_timestamp -> datetime"),
    f("date", "date(expr) -> date"),
    f("date_add", "date_add(date, interval n unit) -> date"),
    f("date_sub", "date_sub(date, interval n unit) -> date"),
    f("date_format", "date_format(date, format) -> text"),
    f("str_to_date", "str_to_date(text, format) -> datetime"),
    f("datediff", "datediff(a, b) -> integer"),
    f("timestampdiff", "timestampdiff(unit, a, b) -> integer"),
    f("year", "year(date) -> integer"),
    f("month", "month(date) -> integer"),
    f("day", "day(date) -> integer"),
    f("hour", "hour(time) -> integer"),
    f("unix_timestamp", "unix_timestamp([date]) -> integer"),
    f("from_unixtime", "from_unixtime(ts [, format]) -> datetime"),
    f("json_object", "json_object(k, v, ...) -> json"),
    f("json_array", "json_array(...) -> json"),
    f("json_extract", "json_extract(doc, path, ...) -> json"),
    f("json_unquote", "json_unquote(json) -> text"),
    f("json_arrayagg", "json_arrayagg(expr) -> json"),
    f("json_objectagg", "json_objectagg(k, v) -> json"),
    f(
        "json_contains",
        "json_contains(doc, val [, path]) -> boolean",
    ),
    f0("rand", "rand() -> double"),
    f("mod", "mod(a, b) -> number"),
    f("truncate", "truncate(x, d) -> number"),
    f0("last_insert_id", "last_insert_id() -> integer"),
    f0("database", "database() -> text"),
    f0("uuid", "uuid() -> text"),
    f("cast", "cast(expr as type) -> value"),
    f("convert", "convert(expr, type) -> value"),
    f("std", "std(expr) -> number"),
    f("variance", "variance(expr) -> number"),
    f("bit_and", "bit_and(expr) -> integer"),
    f("find_in_set", "find_in_set(str, list) -> integer"),
];

const SQLITE_FUNCS: &[Func] = &[
    f("group_concat", "group_concat(expr [, sep]) -> text"),
    f("total", "total(expr) -> real"),
    f("ifnull", "ifnull(a, b) -> a if not null"),
    f("iif", "iif(cond, then, else) -> value"),
    f("length", "length(x) -> integer"),
    f("instr", "instr(text, sub) -> integer"),
    f("substring", "substring(text, start [, len]) -> text"),
    f("printf", "printf(format, ...) -> text"),
    f("format", "format(format, ...) -> text"),
    f("hex", "hex(blob) -> text"),
    f("unhex", "unhex(text) -> blob"),
    f("quote", "quote(x) -> text"),
    f("unicode", "unicode(text) -> integer"),
    f("char", "char(code, ...) -> text"),
    f("typeof", "typeof(x) -> text"),
    f("max", "max(a, b, ...) -> same type"),
    f("min", "min(a, b, ...) -> same type"),
    f("date", "date(time, modifier, ...) -> text"),
    f("time", "time(time, modifier, ...) -> text"),
    f("datetime", "datetime(time, modifier, ...) -> text"),
    f("julianday", "julianday(time, modifier, ...) -> real"),
    f("strftime", "strftime(format, time, modifier, ...) -> text"),
    f("unixepoch", "unixepoch(time, modifier, ...) -> integer"),
    f0("current_timestamp", "current_timestamp -> text"),
    f("json", "json(text) -> text"),
    f("json_extract", "json_extract(json, path, ...) -> value"),
    f("json_object", "json_object(k, v, ...) -> text"),
    f("json_array", "json_array(...) -> text"),
    f(
        "json_array_length",
        "json_array_length(json [, path]) -> integer",
    ),
    f("json_each", "json_each(json [, path]) -> table"),
    f("json_set", "json_set(json, path, value, ...) -> text"),
    f("json_group_array", "json_group_array(expr) -> text"),
    f("json_group_object", "json_group_object(k, v) -> text"),
    f0("random", "random() -> integer"),
    f("randomblob", "randomblob(n) -> blob"),
    f("zeroblob", "zeroblob(n) -> blob"),
    f0("last_insert_rowid", "last_insert_rowid() -> integer"),
    f0("changes", "changes() -> integer"),
    f0("total_changes", "total_changes() -> integer"),
    f("likelihood", "likelihood(x, p) -> x"),
    f("glob", "glob(pattern, text) -> boolean"),
    f("like", "like(pattern, text) -> boolean"),
    f0("sqlite_version", "sqlite_version() -> text"),
    f("round", "round(x [, digits]) -> real"),
    f("nullif", "nullif(a, b) -> null if a = b"),
];

/// Words that are not columns although they are neither reserved nor in the keyword and function
/// tables: type names, date parts, window-frame words and special values.
const NON_COLUMN_WORDS: &[&str] = &[
    "int",
    "integer",
    "bigint",
    "smallint",
    "tinyint",
    "mediumint",
    "text",
    "varchar",
    "char",
    "character",
    "varying",
    "boolean",
    "bool",
    "numeric",
    "decimal",
    "float",
    "double",
    "precision",
    "real",
    "date",
    "time",
    "timestamp",
    "timestamptz",
    "datetime",
    "interval",
    "json",
    "jsonb",
    "uuid",
    "bytea",
    "blob",
    "signed",
    "unsigned",
    "binary",
    "zone",
    "year",
    "quarter",
    "month",
    "week",
    "day",
    "hour",
    "minute",
    "second",
    "microsecond",
    "millisecond",
    "epoch",
    "dow",
    "doy",
    "isodow",
    "isoyear",
    "century",
    "decade",
    "timezone",
    "current_date",
    "current_time",
    "current_timestamp",
    "current_user",
    "session_user",
    "localtime",
    "localtimestamp",
    "ilike",
    "similar",
    "nulls",
    "unbounded",
    "preceding",
    "following",
    "row",
    "rows",
    "range",
    "groups",
    "partition",
    "exclude",
    "ties",
    "others",
    "within",
    "ordinality",
    "both",
    "leading",
    "trailing",
    "unknown",
    "glob",
    "regexp",
    "rlike",
    "div",
    "mod",
    // MySQL select modifiers.
    "distinctrow",
    "high_priority",
    "low_priority",
    "straight_join",
    "sql_small_result",
    "sql_big_result",
    "sql_buffer_result",
    "sql_cache",
    "sql_no_cache",
    "sql_calc_found_rows",
    // `GROUP BY GROUPING SETS (...)`, `CUBE`, `ROLLUP`.
    "grouping",
    "sets",
    "cube",
    "rollup",
    "share",
    "mode",
];

/// Whether `word` can be something other than a column: a reserved word, a keyword, a function
/// name, or a type name or date part. Used to keep unknown-column diagnostics conservative.
pub(crate) fn is_non_column_word(dialect: Dialect, word: &str) -> bool {
    is_reserved(dialect, word)
        || keywords(dialect).any(|k| k.eq_ignore_ascii_case(word))
        || functions(dialect).any(|f| f.name.eq_ignore_ascii_case(word))
        || NON_COLUMN_WORDS
            .iter()
            .any(|k| k.eq_ignore_ascii_case(word))
}

/// Whether `name` is an implicit column every row has without it being declared (`ctid`,
/// `rowid`). MySQL has only the hidden `_rowid` of `InnoDB`.
pub(crate) fn is_system_column(dialect: Dialect, name: &str) -> bool {
    let list: &[&str] = match flavor(dialect) {
        Flavor::Postgres => &["ctid", "xmin", "xmax", "cmin", "cmax", "tableoid", "oid"],
        Flavor::MySql => &["_rowid"],
        Flavor::Sqlite => &["rowid", "_rowid_", "oid"],
    };
    list.iter().any(|c| c.eq_ignore_ascii_case(name))
}

/// Whether `name` is a schema the database provides itself, so a source that omits it is not
/// evidence that it does not exist.
pub(crate) fn is_system_schema(dialect: Dialect, name: &str) -> bool {
    let list: &[&str] = match flavor(dialect) {
        Flavor::Postgres => &["pg_catalog", "information_schema", "pg_toast"],
        Flavor::MySql => &["information_schema", "mysql", "performance_schema", "sys"],
        Flavor::Sqlite => &["main", "temp"],
    };
    list.iter().any(|s| s.eq_ignore_ascii_case(name))
}

/// Whether the unqualified `name` is a system relation (`pg_class`, `sqlite_master`, `dual`).
pub(crate) fn is_system_relation(dialect: Dialect, name: &str) -> bool {
    let starts = |prefix: &str| {
        name.get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
    };
    match flavor(dialect) {
        Flavor::Postgres => starts("pg_"),
        Flavor::MySql => name.eq_ignore_ascii_case("dual"),
        Flavor::Sqlite => starts("sqlite_"),
    }
}

/// Built-in functions of the dialect.
pub(crate) fn functions(dialect: Dialect) -> impl Iterator<Item = &'static Func> {
    let own: &'static [Func] = match flavor(dialect) {
        Flavor::Postgres => PG_FUNCS,
        Flavor::MySql => MYSQL_FUNCS,
        Flavor::Sqlite => SQLITE_FUNCS,
    };
    // A dialect-specific entry replaces a common one with the same name.
    COMMON_FUNCS
        .iter()
        .filter(move |c| !own.iter().any(|o| o.name == c.name))
        .chain(own)
}

/// The built-in snippets.
pub(crate) const SNIPPETS: &[Snippet] = &[
    Snippet {
        trigger: "sel",
        detail: "SELECT ... FROM ... WHERE",
        body: "SELECT ${1:*}\nFROM ${2:table}\nWHERE ${3:condition}$0",
    },
    Snippet {
        trigger: "ins",
        detail: "INSERT INTO ... VALUES",
        body: "INSERT INTO ${1:table} (${2:columns})\nVALUES (${3:values})$0",
    },
    Snippet {
        trigger: "upd",
        detail: "UPDATE ... SET ... WHERE",
        body: "UPDATE ${1:table}\nSET ${2:column} = ${3:value}\nWHERE ${4:condition}$0",
    },
    Snippet {
        trigger: "del",
        detail: "DELETE FROM ... WHERE",
        body: "DELETE FROM ${1:table}\nWHERE ${2:condition}$0",
    },
    Snippet {
        trigger: "cte",
        detail: "WITH ... AS (...) SELECT",
        body: "WITH ${1:name} AS (\n  ${2:SELECT 1}\n)\nSELECT *\nFROM ${1:name}$0",
    },
];

/// Applies the keyword case to `text`, leaving `${n:placeholder}` bodies and tab stops alone.
pub(crate) fn apply_case(text: &str, lowercase: bool) -> String {
    if !lowercase {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut prev = '\0';
    for c in text.chars() {
        if c == '{' && prev == '$' {
            depth += 1;
        } else if c == '}' && depth > 0 {
            depth -= 1;
        }
        out.push(if depth > 0 { c } else { c.to_ascii_lowercase() });
        prev = c;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str, quoted: bool) -> Ident {
        Ident {
            value: value.to_owned(),
            quoted,
            span: 0..value.len(),
        }
    }

    #[test]
    fn ident_eq_rules() {
        let pg = Dialect::Postgres;
        assert!(ident_eq(pg, &id("Users", false), "users"));
        assert!(!ident_eq(pg, &id("Users", true), "users"));
        assert!(ident_eq(pg, &id("Users", true), "Users"));
        assert!(!ident_eq(pg, &id("users", false), "Users"));
        assert!(ident_eq(Dialect::MySql, &id("USERS", false), "users"));
        assert!(ident_eq(Dialect::Sqlite, &id("Ünï", true), "ünï"));
    }

    #[test]
    fn find_stored_prefers_strict() {
        let names = ["Users", "users"];
        let got = find_stored(Dialect::Postgres, &id("USERS", false), names.into_iter());
        assert_eq!(got, Some("users"));
        let got = find_stored(Dialect::Postgres, &id("Users", true), names.into_iter());
        assert_eq!(got, Some("Users"));
        let got = find_stored(Dialect::Postgres, &id("uSeRs", true), ["Users"].into_iter());
        assert_eq!(got, Some("Users"), "lenient fallback");
        assert_eq!(
            find_stored(Dialect::MySql, &id("x", false), ["y"].into_iter()),
            None
        );
    }

    #[test]
    fn text_names_follow_the_dialect() {
        let pg = |v: &str, q| text_name(Dialect::Postgres, &id(v, q)).into_owned();
        assert_eq!(pg("USERS", false), "users");
        // PostgreSQL folds ASCII only: a non-ASCII capital keeps its case.
        assert_eq!(pg("ÉCOLE", false), "École");
        assert_eq!(pg("Ünï_ID", false), "Ünï_id");
        assert_eq!(pg("Users", true), "Users");
        assert_eq!(pg("users", true), "users");
        for d in [Dialect::MySql, Dialect::Sqlite] {
            assert_eq!(text_name(d, &id("USERS", false)), "USERS");
        }
    }

    #[test]
    fn postgres_folds_ascii_only() {
        let upper = id("ÉCOLE", false);
        assert_eq!(normalize(Dialect::Postgres, &upper), "École");
        assert!(!ident_eq(Dialect::Postgres, &upper, "école"));
        assert!(ident_eq(Dialect::Postgres, &upper, "École"));
        let names = ["école", "École"];
        assert_eq!(
            find_stored(Dialect::Postgres, &upper, names.into_iter()),
            Some("École")
        );
        assert_eq!(
            find_stored(Dialect::MySql, &upper, names.into_iter()),
            Some("école")
        );
    }

    #[test]
    fn quoting_rules() {
        assert!(!needs_quotes(Dialect::Postgres, "user_id"));
        assert!(needs_quotes(Dialect::Postgres, "UserId"));
        assert!(needs_quotes(Dialect::Postgres, "order"));
        assert!(needs_quotes(Dialect::Postgres, "my col"));
        assert!(needs_quotes(Dialect::Postgres, "1abc"));
        assert!(needs_quotes(Dialect::Postgres, ""));
        assert!(!needs_quotes(Dialect::MySql, "UserId"));
        assert!(needs_quotes(Dialect::MySql, "key"));
        assert!(!needs_quotes(Dialect::Postgres, "key"));
        assert_eq!(quote(Dialect::MySql, "my col", false).unwrap(), "`my col`");
        assert_eq!(quote(Dialect::Sqlite, "ab", true).unwrap(), "\"ab\"");
        assert_eq!(quote(Dialect::Postgres, "ab", false).unwrap(), "ab");
        assert_eq!(quote(Dialect::Postgres, "", false), None);
        assert_eq!(quote(Dialect::Postgres, "a\0b", false), None);
    }

    #[test]
    fn tables_are_populated() {
        for d in Dialect::ALL {
            assert!(functions(d).count() >= 60, "{d:?} functions");
            assert!(keywords(d).count() >= 100, "{d:?} keywords");
            assert!(statement_keywords(d).contains(&"SELECT"));
        }
        assert_eq!(SNIPPETS.len(), 5);
    }

    #[test]
    fn case_application() {
        assert_eq!(
            apply_case("SELECT ${1:Table} X", true),
            "select ${1:Table} x"
        );
        assert_eq!(apply_case("SELECT", false), "SELECT");
    }

    #[test]
    fn strict_stored_match() {
        let pg = Dialect::Postgres;
        assert!(stored_eq_strict(pg, &id("USERS", false), "users"));
        assert!(!stored_eq_strict(pg, &id("users", false), "Users"));
        assert!(!stored_eq_strict(pg, &id("Users", true), "users"));
        assert!(stored_eq_strict(pg, &id("Users", true), "Users"));
        assert!(!stored_eq_strict(pg, &id("ÉCOLE", false), "école"));
        assert!(stored_eq_strict(pg, &id("ÉCOLE", false), "École"));
        for d in [Dialect::MySql, Dialect::Sqlite] {
            assert!(stored_eq_strict(d, &id("USERS", false), "users"));
            assert!(stored_eq_strict(d, &id("Users", true), "users"));
            assert!(!stored_eq_strict(d, &id("user", true), "users"));
        }
        assert_eq!(fold_hash("Users"), fold_hash("uSERS"));
        assert_ne!(fold_hash("users"), fold_hash("user"));
    }

    #[test]
    fn non_column_words_and_system_names() {
        let pg = Dialect::Postgres;
        for w in ["SELECT", "int", "YEAR", "count", "Zone", "ilike"] {
            assert!(is_non_column_word(pg, w), "{w}");
        }
        assert!(!is_non_column_word(pg, "customer_id"));
        assert!(is_system_schema(pg, "PG_CATALOG"));
        assert!(!is_system_schema(pg, "public"));
        assert!(is_system_schema(Dialect::Sqlite, "main"));
        assert!(is_system_schema(Dialect::MySql, "mysql"));
        assert!(is_system_relation(pg, "pg_class"));
        assert!(!is_system_relation(pg, "users"));
        assert!(is_system_relation(Dialect::Sqlite, "sqlite_master"));
        assert!(is_system_relation(Dialect::MySql, "DUAL"));
        assert!(!is_system_relation(Dialect::MySql, "pg_class"));
    }
}
