// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Bound `UPDATE`, `INSERT` and `DELETE` for the staged edits of the results grid, and the
//! analysis that proves a `SELECT` reads one plain table.
//!
//! The generators are pure. Identifiers are quoted with [`quote_identifier`]; every value goes
//! into [`Query::params`] and is referred to by the dialect's native positional marker, so no
//! value text is ever written into the SQL, and no string literal is emitted. The only text
//! taken from the catalog and written unquoted is a column's native type on `PostgreSQL`, and
//! only after a character whitelist (see [`Dialect::Postgres`] casts below).
//!
//! `PostgreSQL` binds decimals, dates, times, timestamps, JSON and text as `text`, and `NULL`
//! as `int8`, so the generator casts those parameters to the column's type.

use sqler_core::{ColumnInfo, DataType, Query, Table, Value};

use crate::lexer::{Token, TokenKind, tokenize};
use crate::{DdlError, Dialect, StatementClass, classify_each, qualified_name, quote_identifier};

/// A catalog column name and the value to bind for it.
pub type ColumnValue<'a> = (&'a str, &'a Value);

/// Why a statement could not be generated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DmlError {
    /// The table or a column name cannot be quoted.
    #[error(transparent)]
    Identifier(#[from] DdlError),
    /// The column is not a column of the table.
    #[error("unknown column {column:?}")]
    UnknownColumn {
        /// The name that was given.
        column: String,
    },
    /// The column is listed twice in one list.
    #[error("column {column:?} is listed twice")]
    DuplicateColumn {
        /// The repeated name.
        column: String,
    },
    /// An `UPDATE` without a column to set.
    #[error("nothing to set")]
    EmptySet,
    /// An `UPDATE` or `DELETE` without a filter; an unfiltered write is never generated.
    #[error("refusing to write without a filter")]
    EmptyFilter,
    /// An `INSERT` without a value.
    #[error("nothing to insert")]
    EmptyInsert,
    /// A value cannot be written to the column without the server changing it silently.
    #[error("column {column:?} cannot be written: {reason}")]
    UnwritableType {
        /// The column.
        column: String,
        /// Why, see [`unwritable_reason`].
        reason: &'static str,
    },
}

/// A transaction control statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TxStep {
    /// Starts the transaction.
    Begin,
    /// Commits it.
    Commit,
    /// Rolls it back.
    Rollback,
}

/// The SQL of a transaction step: `START TRANSACTION` (`MySQL` begin) or `BEGIN`, `COMMIT`,
/// `ROLLBACK`; on T-SQL `BEGIN TRANSACTION`, `COMMIT TRANSACTION` and `ROLLBACK TRANSACTION`.
#[must_use]
pub fn transaction_sql(dialect: Dialect, step: TxStep) -> &'static str {
    match (dialect, step) {
        (Dialect::TSql, TxStep::Begin) => "BEGIN TRANSACTION",
        (Dialect::TSql, TxStep::Commit) => "COMMIT TRANSACTION",
        (Dialect::TSql, TxStep::Rollback) => "ROLLBACK TRANSACTION",
        (Dialect::MySql, TxStep::Begin) => "START TRANSACTION",
        (_, TxStep::Begin) => "BEGIN",
        (_, TxStep::Commit) => "COMMIT",
        (_, TxStep::Rollback) => "ROLLBACK",
    }
}

/// The marker of the `k`-th (1-based) parameter in the SQL the generators write: `$k` on
/// `PostgreSQL`, `?k` on `SQLite`, `?` on `MySQL`, `@P{k}` on T-SQL. The single source of the
/// marker format; the apply preview calls it instead of keeping a copy.
#[must_use]
pub fn param_marker(dialect: Dialect, k: usize) -> String {
    match dialect {
        Dialect::Postgres => format!("${k}"),
        Dialect::Sqlite => format!("?{k}"),
        Dialect::MySql => "?".to_owned(),
        Dialect::TSql => format!("@P{k}"),
    }
}

/// Whether a catalog type spelling may be written into a `CAST`: 1 to 128 ASCII bytes of
/// letters, digits, `_`, space, `,`, `.`, `(`, `)`, `[` and `]`, not blank, with balanced `()` and
/// `[]`. No quote, backslash, `$`, `?`, `;`, comment or control character is allowed, so the
/// spelling can never change the lexing mode of the placeholder scanner (quoted or mixed-case
/// types fall back to the logical type).
fn is_safe_type_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && !name.trim().is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'_' | b' ' | b',' | b'.' | b'(' | b')' | b'[' | b']')
        })
        && balanced(name)
}

/// Whether `(` `)` and `[` `]` nest properly (a lone `[` would swallow the markers after it).
fn balanced(name: &str) -> bool {
    let (mut round, mut square) = (0usize, 0usize);
    for b in name.bytes() {
        match b {
            b'(' => round += 1,
            b'[' => square += 1,
            b')' => match round.checked_sub(1) {
                Some(r) => round = r,
                None => return false,
            },
            b']' => match square.checked_sub(1) {
                Some(r) => square = r,
                None => return false,
            },
            _ => {}
        }
    }
    round == 0 && square == 0
}

/// Removes every parenthesised group (type modifiers such as `(10,2)`), nested ones included.
fn strip_modifiers(name: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Splits trailing array brackets off a type: `("int4", "[]")`; `None` if a `[` is anywhere but
/// in a clean suffix of `[]` / `[n]` groups.
fn split_array(name: &str) -> Option<(&str, String)> {
    let Some(at) = name.find('[') else {
        return Some((name, String::new()));
    };
    let (base, rest) = name.split_at(at);
    if !rest
        .bytes()
        .all(|b| matches!(b, b'[' | b']') || b.is_ascii_digit())
        || !balanced(rest)
    {
        return None;
    }
    Some((base, "[]".repeat(rest.matches('[').count())))
}

/// The type to write in a cast for a catalog type spelling, or `None` if the spelling is not safe
/// to write.
///
/// An explicit cast to `character varying(5)`, `character(3)`, `bit(4)` silently truncates or
/// pads, where plain assignment to the column raises an error for a value that does not fit. The
/// cast therefore names the unconstrained type and the server's assignment check against the
/// column's own modifier still applies. `character(n)` becomes `bpchar` and `bit(n)` becomes
/// `bit varying`, because `character` and `bit` alone mean length one. `numeric`, `time` and
/// `timestamp` modifiers are dropped too (rounding is the same either way).
///
/// An `interval` spelling keeps its fields and precision as written: `PostgreSQL` reads an
/// unmarked quantity (`'1'`) by the fields of the type it is cast to, so `interval year` and
/// `interval` give different values, and the modifier does not change how a value is truncated.
fn cast_type(native: &str) -> Option<String> {
    if native.contains('"') {
        return quoted_type(native);
    }
    if !is_safe_type_name(native) {
        return None;
    }
    let (written, array) = split_array(native)?;
    let written = written.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower_written = written.to_ascii_lowercase();
    let base = if is_interval(&lower_written) {
        written
    } else {
        let stripped = strip_modifiers(&written);
        let base = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        match base.to_ascii_lowercase().as_str() {
            "character" => "bpchar".to_owned(),
            "bit" => "bit varying".to_owned(),
            _ => base,
        }
    };
    let out = format!("{base}{array}");
    (!base.is_empty() && is_safe_type_name(&out)).then_some(out)
}

/// Built-in `PostgreSQL` type spellings (as `format_type` prints them, plus common aliases) whose
/// cast the server applies as it would to a literal. A spelling outside this list on a column that
/// is not `Other` is a domain or a user type over a string type, see [`pg_type`].
const BUILTIN_PG_TYPES: &[&str] = &[
    "boolean",
    "bool",
    "smallint",
    "integer",
    "bigint",
    "int2",
    "int4",
    "int8",
    "oid",
    "real",
    "double precision",
    "float4",
    "float8",
    "numeric",
    "decimal",
    "text",
    "character varying",
    "varchar",
    "character",
    "bpchar",
    "name",
    "xml",
    "uuid",
    "inet",
    "cidr",
    "macaddr",
    "macaddr8",
    "unknown",
    "bytea",
    "money",
    "date",
    "time",
    "time without time zone",
    "time with time zone",
    "timetz",
    "timestamp",
    "timestamp without time zone",
    "timestamp with time zone",
    "timestamptz",
    "json",
    "jsonb",
    "jsonpath",
    "bit",
    "bit varying",
    "varbit",
    "point",
    "line",
    "lseg",
    "box",
    "path",
    "polygon",
    "circle",
    "tsvector",
    "tsquery",
    "pg_lsn",
    "int4range",
    "int8range",
    "numrange",
    "tsrange",
    "tstzrange",
    "daterange",
];

/// Whether a cast type produced by [`cast_type`] names a built-in type (arrays of one included).
fn is_builtin_pg_type(cast: &str) -> bool {
    let lower = cast.to_ascii_lowercase();
    let element = lower.split('[').next().unwrap_or("").trim();
    is_interval(element) || BUILTIN_PG_TYPES.contains(&element)
}

/// Whether a lowercase spelling is an `interval`, with or without precision or fields.
fn is_interval(lower: &str) -> bool {
    lower
        .strip_prefix("interval")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '(']))
}

/// How many trailing `[]` / `[n]` dimensions a type spelling ends with.
fn array_dimensions(native: &str) -> usize {
    let mut rest = native.trim_end();
    let mut dims = 0;
    while let Some(head) = rest.strip_suffix(']') {
        let Some(open) = head.rfind('[') else { break };
        if !head[open + 1..].bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        dims += 1;
        rest = head[..open].trim_end();
    }
    dims
}

/// A type written as `ident[.ident]` with double-quoted parts (an enum or domain whose name needs
/// quoting, as `format_type` prints it), re-quoted part by part. Anything else with a quote is
/// refused. The result contains only identifiers produced by [`quote_identifier`].
fn quoted_type(native: &str) -> Option<String> {
    let mut chars = native.chars().peekable();
    let mut parts: Vec<String> = Vec::new();
    loop {
        let mut part = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next()? {
                    '"' if chars.peek() == Some(&'"') => {
                        chars.next();
                        part.push('"');
                    }
                    '"' => break,
                    c => part.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_ascii_lowercase() || c == '_' || (!part.is_empty() && c.is_ascii_digit()) {
                    part.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
        }
        // A single quote, a control character or a non-ASCII letter is refused (the logical type
        // is used instead): the SQL stays free of anything that could be read as a string literal.
        if part.is_empty() || !part.is_ascii() || part.chars().any(|c| c == '\'' || c.is_control())
        {
            return None;
        }
        parts.push(part);
        if chars.peek() == Some(&'.') {
            chars.next();
        } else {
            break;
        }
    }
    let rest: String = chars.collect();
    let (empty, array) = split_array(&rest)?;
    if !empty.is_empty() || parts.len() > 2 {
        return None;
    }
    // `"char"` is the one-byte internal type: an explicit cast truncates to one byte.
    if parts.len() == 1 && parts[0] == "char" {
        return None;
    }
    let quoted: Option<Vec<String>> = parts
        .iter()
        .map(|p| quote_identifier(Dialect::Postgres, p).ok())
        .collect();
    Some(format!("{}{array}", quoted?.join(".")))
}

/// Why a value cannot be written to this column safely, or `None` if it can.
///
/// Only `PostgreSQL` has such columns, and on T-SQL the `timestamp` (`rowversion`) type, which the
/// server fills itself and refuses to write.
///
/// - `"char"` (the one-byte internal type): assigning text keeps the first byte, so a longer value
///   is cut with no error, and an explicit cast does the same.
/// - `interval` with fields (`interval year`, `interval day to second(3)`): a bare number such as
///   `1` is read by the fields of the column only when the server parses a literal. Sent as a text
///   parameter it is read as seconds and then masked by the fields, so `1` becomes `00:00:00`.
///
/// Both are refused for `UPDATE` and `INSERT`; the grid marks such a result read-only.
#[must_use]
pub fn unwritable_reason(dialect: Dialect, column: &ColumnInfo) -> Option<&'static str> {
    if dialect == Dialect::TSql {
        let native = column.native_type.as_deref()?.trim();
        return (native.eq_ignore_ascii_case("timestamp")
            || native.eq_ignore_ascii_case("rowversion"))
        .then_some("a rowversion column is filled by the server and cannot be written");
    }
    if dialect != Dialect::Postgres {
        return None;
    }
    let native = column.native_type.as_deref()?;
    let mut element = native.trim();
    while let Some(head) = element.strip_suffix(']') {
        match head.rfind('[') {
            Some(open) => element = head[..open].trim_end(),
            None => break,
        }
    }
    if element == "\"char\"" {
        return Some("the one-byte type \"char\" keeps only the first byte of a longer value");
    }
    let lower = element.to_ascii_lowercase();
    // The same strict test as the cast code: an enum `intervalkind` or a domain `intervals` is
    // not an interval.
    let fields = is_interval(&lower)
        && lower["interval".len()..]
            .trim_start()
            .starts_with(|c: char| c.is_ascii_alphabetic());
    fields.then_some(
        "an interval with fields reads a bare number differently when it is sent as text",
    )
}

/// The `PostgreSQL` type to cast parameters of this column to.
///
/// The column's own type name is used for an `Other` column (enums, extension types) and for a
/// built-in name. For any other column the name is a domain (or an array of a user type) whose
/// base type is a string, number or date type; casting to it explicitly would apply the base
/// type's *explicit* cast rules, and a domain over `varchar(5)` would truncate silently. The
/// parameter is cast to the logical type (`text`, `text[]`, `numeric`, ...) instead, and the
/// server's assignment rules apply on the column: a value that does not fit is an error and the
/// transaction rolls back. The price is that a domain over a non-string base (`uuid`, `inet`) and
/// an array of enums fail with a rollback too. A domain over `bit(n)` is `Other` and cannot be
/// told from an enum; it keeps the documented gap.
fn pg_type(column: &ColumnInfo) -> Option<String> {
    let native = column.native_type.as_deref();
    if let Some(t) = native.and_then(cast_type)
        && (column.data_type == DataType::Other || is_builtin_pg_type(&t))
    {
        return Some(t);
    }
    let name = match column.data_type {
        DataType::Text => "text",
        DataType::Decimal => "numeric",
        DataType::Date => "date",
        DataType::Time => "time",
        DataType::Timestamp => "timestamp",
        DataType::TimestampTz => "timestamptz",
        DataType::Json => "jsonb",
        DataType::Integer => "bigint",
        DataType::Float => "double precision",
        DataType::Boolean => "boolean",
        DataType::Binary => "bytea",
        _ => return None,
    };
    let dims = if column.data_type == DataType::Text {
        native.map_or(0, array_dimensions)
    } else {
        0
    };
    Some(format!("{name}{}", "[]".repeat(dims)))
}

/// A generated statement and, for each entry of `query.params`, the catalog column it is bound
/// for. The columns come from the generator itself, so a preview can label every value without
/// re-deriving the generator's parameter order.
///
/// `Debug` prints counts only: the bound values are user data.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct BoundDml {
    /// The SQL and its parameters.
    pub query: Query,
    /// The column of `query.params[i]`, same length and order.
    pub columns: Vec<String>,
}

impl std::fmt::Debug for BoundDml {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundDml")
            .field("sql_len", &self.query.sql.len())
            .field("params", &self.query.params.len())
            .field("columns", &self.columns.len())
            .finish()
    }
}

struct Builder<'a> {
    dialect: Dialect,
    table: &'a Table,
    params: Vec<Value>,
    bound: Vec<String>,
}

impl<'a> Builder<'a> {
    fn new(dialect: Dialect, table: &'a Table) -> Self {
        Self {
            dialect,
            table,
            params: Vec::new(),
            bound: Vec::new(),
        }
    }

    /// Checks that every name is a column of the table and none repeats.
    fn columns(&self, list: &[ColumnValue<'_>]) -> Result<Vec<&'a ColumnInfo>, DmlError> {
        let mut out: Vec<&ColumnInfo> = Vec::with_capacity(list.len());
        for (name, _) in list {
            let column = self
                .table
                .columns
                .iter()
                .find(|c| c.name == *name)
                .ok_or_else(|| DmlError::UnknownColumn {
                    column: (*name).to_owned(),
                })?;
            if out.iter().any(|c| c.name == column.name) {
                return Err(DmlError::DuplicateColumn {
                    column: (*name).to_owned(),
                });
            }
            out.push(column);
        }
        Ok(out)
    }

    /// Binds a value and returns the marker, wrapped in the cast `PostgreSQL` needs.
    fn bind(&mut self, column: &ColumnInfo, value: &Value) -> String {
        let m = self.push(column, value);
        if self.dialect != Dialect::Postgres {
            return m;
        }
        match value {
            Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Bytes(_) => m,
            Value::Null => match pg_type(column) {
                Some(t) => format!("CAST(CAST({m} AS text) AS {t})"),
                None => m,
            },
            _ => match pg_type(column) {
                Some(t) => format!("CAST({m} AS {t})"),
                None => m,
            },
        }
    }

    /// Records a parameter and returns its marker.
    fn push(&mut self, column: &ColumnInfo, value: &Value) -> String {
        self.params.push(value.clone());
        self.bound.push(column.name.clone());
        param_marker(self.dialect, self.params.len())
    }

    fn quote(&self, name: &str) -> Result<String, DmlError> {
        Ok(quote_identifier(self.dialect, name)?)
    }

    fn target(&self) -> Result<String, DmlError> {
        Ok(qualified_name(self.dialect, &self.table.object)?)
    }

    /// Refuses a column a value cannot be written to without a silent change.
    fn writable(&self, columns: &[&ColumnInfo]) -> Result<(), DmlError> {
        for column in columns {
            if let Some(reason) = unwritable_reason(self.dialect, column) {
                return Err(DmlError::UnwritableType {
                    column: column.name.clone(),
                    reason,
                });
            }
        }
        Ok(())
    }

    /// `c = p AND …`; a `Null` value is `c IS NULL` and binds nothing.
    fn where_clause(&mut self, filter: &[ColumnValue<'_>]) -> Result<String, DmlError> {
        let columns = self.columns(filter)?;
        let mut parts = Vec::with_capacity(filter.len());
        for (column, (_, value)) in columns.into_iter().zip(filter) {
            let name = self.quote(&column.name)?;
            if matches!(value, Value::Null) {
                parts.push(format!("{name} IS NULL"));
            } else if self.dialect == Dialect::Postgres && column.data_type == DataType::Json {
                let m = self.push(column, value);
                parts.push(format!("CAST({name} AS text) = {m}"));
            } else if self.dialect == Dialect::MySql && column.data_type == DataType::Json {
                // A text parameter compares as a JSON string, never equal to a JSON document.
                let m = self.push(column, value);
                parts.push(format!("{name} = CAST({m} AS JSON)"));
            } else {
                let p = self.bind(column, value);
                parts.push(format!("{name} = {p}"));
            }
        }
        Ok(parts.join(" AND "))
    }

    fn finish(self, sql: String) -> BoundDml {
        BoundDml {
            query: Query::new(sql).with_params(self.params),
            columns: self.bound,
        }
    }
}

/// `UPDATE <table> SET <c> = <p>, … WHERE <c> = <p> AND …`.
///
/// Markers are numbered in order of appearance and `params` follow the same order. A `Null`
/// in `set` is bound; a `Null` in `filter` becomes `IS NULL`.
///
/// # Errors
///
/// [`DmlError::EmptySet`], [`DmlError::EmptyFilter`], [`DmlError::UnknownColumn`],
/// [`DmlError::DuplicateColumn`] and [`DmlError::Identifier`].
pub fn update(
    dialect: Dialect,
    table: &Table,
    set: &[ColumnValue<'_>],
    filter: &[ColumnValue<'_>],
) -> Result<Query, DmlError> {
    update_bound(dialect, table, set, filter).map(|b| b.query)
}

/// [`update`] with the column of every parameter.
///
/// # Errors
///
/// As [`update`].
pub fn update_bound(
    dialect: Dialect,
    table: &Table,
    set: &[ColumnValue<'_>],
    filter: &[ColumnValue<'_>],
) -> Result<BoundDml, DmlError> {
    if set.is_empty() {
        return Err(DmlError::EmptySet);
    }
    if filter.is_empty() {
        return Err(DmlError::EmptyFilter);
    }
    let mut b = Builder::new(dialect, table);
    let columns = b.columns(set)?;
    b.writable(&columns)?;
    let mut sql = format!("UPDATE {} SET ", b.target()?);
    let mut assignments = Vec::with_capacity(set.len());
    for (column, (_, value)) in columns.into_iter().zip(set) {
        let name = b.quote(&column.name)?;
        let p = b.bind(column, value);
        assignments.push(format!("{name} = {p}"));
    }
    sql.push_str(&assignments.join(", "));
    sql.push_str(" WHERE ");
    sql.push_str(&b.where_clause(filter)?);
    Ok(b.finish(sql))
}

/// `INSERT INTO <table> (<c>, …) VALUES (<p>, …)`.
///
/// # Errors
///
/// [`DmlError::EmptyInsert`], [`DmlError::UnknownColumn`], [`DmlError::DuplicateColumn`] and
/// [`DmlError::Identifier`].
pub fn insert(
    dialect: Dialect,
    table: &Table,
    values: &[ColumnValue<'_>],
) -> Result<Query, DmlError> {
    insert_bound(dialect, table, values).map(|b| b.query)
}

/// [`insert`] with the column of every parameter.
///
/// # Errors
///
/// As [`insert`].
pub fn insert_bound(
    dialect: Dialect,
    table: &Table,
    values: &[ColumnValue<'_>],
) -> Result<BoundDml, DmlError> {
    if values.is_empty() {
        return Err(DmlError::EmptyInsert);
    }
    let mut b = Builder::new(dialect, table);
    let columns = b.columns(values)?;
    b.writable(&columns)?;
    let mut names = Vec::with_capacity(values.len());
    let mut markers = Vec::with_capacity(values.len());
    for (column, (_, value)) in columns.into_iter().zip(values) {
        names.push(b.quote(&column.name)?);
        markers.push(b.bind(column, value));
    }
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        b.target()?,
        names.join(", "),
        markers.join(", ")
    );
    Ok(b.finish(sql))
}

/// `DELETE FROM <table> WHERE <c> = <p> AND …`.
///
/// # Errors
///
/// [`DmlError::EmptyFilter`], [`DmlError::UnknownColumn`], [`DmlError::DuplicateColumn`] and
/// [`DmlError::Identifier`].
pub fn delete(
    dialect: Dialect,
    table: &Table,
    filter: &[ColumnValue<'_>],
) -> Result<Query, DmlError> {
    delete_bound(dialect, table, filter).map(|b| b.query)
}

/// [`delete`] with the column of every parameter.
///
/// # Errors
///
/// As [`delete`].
pub fn delete_bound(
    dialect: Dialect,
    table: &Table,
    filter: &[ColumnValue<'_>],
) -> Result<BoundDml, DmlError> {
    if filter.is_empty() {
        return Err(DmlError::EmptyFilter);
    }
    let mut b = Builder::new(dialect, table);
    let mut sql = format!("DELETE FROM {} WHERE ", b.target()?);
    sql.push_str(&b.where_clause(filter)?);
    Ok(b.finish(sql))
}

/// An identifier as written in SQL text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SqlIdent {
    /// The name without quotes; doubled quote characters are undone.
    pub text: String,
    /// Whether it was written quoted.
    pub quoted: bool,
}

impl SqlIdent {
    /// Whether this identifier names the catalog object `name`.
    ///
    /// A quoted identifier matches exactly. An unquoted one is folded to lower case on
    /// `PostgreSQL` (ASCII letters only, as the server's UTF-8 folding does) and compared ASCII
    /// case-insensitively on `MySQL` and `SQLite`.
    #[must_use]
    pub fn matches(&self, dialect: Dialect, name: &str) -> bool {
        if self.quoted {
            return self.text == name;
        }
        match dialect {
            Dialect::Postgres => self.text.to_ascii_lowercase() == name,
            Dialect::MySql | Dialect::Sqlite | Dialect::TSql => {
                self.text.eq_ignore_ascii_case(name)
            }
        }
    }
}

/// What a single-table `SELECT` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectList {
    /// `*`.
    Star,
    /// Plain column names, in order.
    Columns(Vec<SqlIdent>),
}

/// The one table a plain `SELECT` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceTable {
    /// The schema (or database) qualifier, if written.
    pub schema: Option<SqlIdent>,
    /// The table name.
    pub name: SqlIdent,
    /// The select list.
    pub columns: SelectList,
}

/// Whether a result column name is the catalog column `table_column`: exact on `PostgreSQL`,
/// ASCII case-insensitive on `MySQL` and `SQLite`.
#[must_use]
pub fn same_column(dialect: Dialect, result_name: &str, table_column: &str) -> bool {
    match dialect {
        Dialect::Postgres => result_name == table_column,
        Dialect::MySql | Dialect::Sqlite | Dialect::TSql => {
            result_name.eq_ignore_ascii_case(table_column)
        }
    }
}

/// Words that are never an unquoted column or table name in the shapes accepted here.
const NOT_A_NAME: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "DISTINCT",
    "DISTINCTROW",
    "ALL",
    "AS",
    "ON",
    "USING",
    "ONLY",
    "LATERAL",
    "TABLE",
    "NULL",
    "TRUE",
    "FALSE",
    "CASE",
    "ORDER",
    "GROUP",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "HAVING",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "WINDOW",
    "INTO",
    "JOIN",
    "NATURAL",
    "CROSS",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "OUTER",
    "STRAIGHT_JOIN",
    "TOP",
    // Niladic functions and pseudo-columns: written without parentheses they are values, not
    // columns, so `SELECT user, ... FROM t` shows the session user under a column called `user`.
    // A quoted identifier of the same spelling is a real column and stays accepted.
    "CURRENT_CATALOG",
    "CURRENT_DATE",
    "CURRENT_PATH",
    "CURRENT_ROLE",
    "CURRENT_SCHEMA",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "DEFAULT",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "SESSION_USER",
    "SYSDATE",
    "SYSTEM_USER",
    "USER",
    "UTC_DATE",
    "UTC_TIME",
    "UTC_TIMESTAMP",
];

/// Whether a [`NOT_A_NAME`] word is a niladic function or pseudo-column rather than a keyword.
fn is_niladic(word: &str) -> bool {
    NOT_A_NAME
        .iter()
        .skip_while(|w| **w != "TOP")
        .skip(1)
        .any(|w| *w == word)
}

/// Words that end the table part and may start the tail.
const TAIL_START: &[&str] = &["WHERE", "ORDER", "LIMIT", "OFFSET", "FETCH"];

/// Words that must not appear at depth 0 after the table.
const TAIL_FORBIDDEN: &[&str] = &[
    "JOIN",
    "GROUP",
    "HAVING",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "WINDOW",
    "INTO",
];

/// `after_dot` is true for the part after a `.`: there a function-like word is a plain name.
fn ident_of(dialect: Dialect, tok: &Token, src: &str, after_dot: bool) -> Option<SqlIdent> {
    let text = tok.text(src);
    match tok.kind {
        TokenKind::Word => {
            let first = text.chars().next()?;
            if !(first == '_' || first.is_alphabetic())
                || text.contains('.')
                || NOT_A_NAME
                    .iter()
                    .any(|w| text.eq_ignore_ascii_case(w) && !(after_dot && is_niladic(w)))
            {
                return None;
            }
            Some(SqlIdent {
                text: text.to_owned(),
                quoted: false,
            })
        }
        TokenKind::QuotedIdent { terminated: true } => {
            let (open, close) = match text.chars().next()? {
                '"' if dialect != Dialect::MySql => ('"', '"'),
                '`' if matches!(dialect, Dialect::MySql | Dialect::Sqlite) => ('`', '`'),
                '[' if matches!(dialect, Dialect::Sqlite | Dialect::TSql) => ('[', ']'),
                _ => return None,
            };
            let inner = text.strip_prefix(open)?.strip_suffix(close)?;
            if inner.is_empty() {
                return None;
            }
            // `SQLite` brackets do not double; every other quote doubles its closing char.
            let text = if open == '[' && dialect == Dialect::Sqlite {
                inner.to_owned()
            } else {
                let double: String = [close, close].iter().collect();
                inner.replace(&double, &close.to_string())
            };
            Some(SqlIdent { text, quoted: true })
        }
        _ => None,
    }
}

fn is_punct(tok: &Token, src: &str, p: &str) -> bool {
    tok.kind == TokenKind::Punct && tok.text(src) == p
}

/// Proves that `sql` is a plain `SELECT <*|columns> FROM <table> [WHERE|ORDER|LIMIT|OFFSET|FETCH …]`
/// and returns its source table; `None` for anything else (joins, grouping, aliases, `DISTINCT`,
/// expressions, subqueries in `FROM`, `WITH`, several statements, executable comments, `INTO`,
/// unterminated text).
///
/// The output of [`select_star`](crate::select_star) is accepted. Pure and linear in the text.
#[must_use]
pub fn single_table_source(dialect: Dialect, sql: &str) -> Option<SourceTable> {
    let statements = classify_each(dialect, sql);
    if statements.len() != 1 || statements.first()?.1 != StatementClass::Read {
        return None;
    }
    let all = tokenize(dialect, sql);
    if all
        .iter()
        .any(|t| !t.is_terminated() || matches!(t.kind, TokenKind::ExecComment { .. }))
    {
        return None;
    }
    let mut toks: Vec<Token> = all.into_iter().filter(|t| !t.is_trivia()).collect();
    if toks.last().is_some_and(|t| is_punct(t, sql, ";")) {
        toks.pop();
    }
    let mut it = toks.iter().peekable();

    if !it.next()?.is_keyword(sql, "SELECT") {
        return None;
    }
    let mut first = it.next()?;
    if dialect == Dialect::TSql && first.is_keyword(sql, "TOP") {
        // `TOP (n)` or `TOP n`, with a plain number: no percent, no ties, no expression.
        let mut count = it.next()?;
        if is_punct(count, sql, "(") {
            count = it.next()?;
            if count.kind != TokenKind::Number || !is_punct(it.next()?, sql, ")") {
                return None;
            }
        } else if count.kind != TokenKind::Number {
            return None;
        }
        first = it.next()?;
    }
    let columns = if first.kind == TokenKind::Op && first.text(sql) == "*" {
        SelectList::Star
    } else {
        let mut list = vec![ident_of(dialect, first, sql, false)?];
        while it.peek().is_some_and(|t| is_punct(t, sql, ",")) {
            it.next();
            list.push(ident_of(dialect, it.next()?, sql, false)?);
        }
        SelectList::Columns(list)
    };
    if !it.next()?.is_keyword(sql, "FROM") {
        return None;
    }
    let mut name = ident_of(dialect, it.next()?, sql, false)?;
    let mut schema = None;
    if it.peek().is_some_and(|t| is_punct(t, sql, ".")) {
        it.next();
        schema = Some(name);
        name = ident_of(dialect, it.next()?, sql, true)?;
    }
    if let Some(next) = it.peek()
        && !(next.kind == TokenKind::Word && TAIL_START.iter().any(|w| next.is_keyword(sql, w)))
    {
        return None;
    }
    let mut depth = 0usize;
    for tok in it {
        if is_punct(tok, sql, "(") {
            depth += 1;
        } else if is_punct(tok, sql, ")") {
            depth = depth.saturating_sub(1);
        } else if depth == 0
            && tok.kind == TokenKind::Word
            && TAIL_FORBIDDEN.iter().any(|w| tok.is_keyword(sql, w))
        {
            return None;
        }
    }
    Some(SourceTable {
        schema,
        name,
        columns,
    })
}

#[cfg(test)]
mod tests {
    use sqler_core::{
        ColumnInfo, DataType, Date, DbObject, Decimal, ObjectKind, ObjectPath, Table, Time,
        Timestamp, TimestampTz,
    };

    use super::*;
    use crate::select_star;

    const PG: Dialect = Dialect::Postgres;
    const MY: Dialect = Dialect::MySql;
    const LITE: Dialect = Dialect::Sqlite;
    const TS: Dialect = Dialect::TSql;

    fn table() -> Table {
        let object = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("public"),
            "t",
        );
        Table::new(object).with_columns(vec![
            ColumnInfo::new("id", DataType::Integer),
            ColumnInfo::new("name", DataType::Text),
            ColumnInfo::new("price", DataType::Decimal).with_native_type("numeric(10,2)"),
            ColumnInfo::new("doc", DataType::Json),
            ColumnInfo::new("blob", DataType::Binary),
            ColumnInfo::new("day", DataType::Date),
            ColumnInfo::new("at", DataType::Timestamp),
            ColumnInfo::new("atz", DataType::TimestampTz),
            ColumnInfo::new("tm", DataType::Time),
            ColumnInfo::new("odd", DataType::Text).with_native_type("x'; DROP TABLE t; --"),
            ColumnInfo::new("flag", DataType::Boolean),
            ColumnInfo::new("weird", DataType::Other),
        ])
    }

    fn sql(q: &Result<Query, DmlError>) -> &str {
        &q.as_ref().unwrap().sql
    }

    #[test]
    fn bound_dml_debug_never_shows_values() {
        let t = table();
        let name = Value::Text("sekrit-name".into());
        let id = Value::Int(424_242);
        for d in [PG, MY, LITE] {
            let set = [("name", &name)];
            let filter = [("id", &id)];
            let b = update_bound(d, &t, &set, &filter).unwrap();
            let shown = format!("{b:?}");
            assert!(!shown.contains("sekrit"), "{shown}");
            assert!(!shown.contains("424242"), "{shown}");
            assert!(!shown.contains("UPDATE"), "{shown}");
            assert!(shown.contains("params: 2"), "{shown}");
        }
    }

    #[test]
    fn update_per_dialect() {
        let t = table();
        let name = Value::Text("o'brien".into());
        let id = Value::Int(7);
        let set = [("name", &name)];
        let filter = [("id", &id)];
        let q = update(PG, &t, &set, &filter);
        assert_eq!(
            sql(&q),
            r#"UPDATE "public"."t" SET "name" = CAST($1 AS text) WHERE "id" = $2"#
        );
        assert_eq!(q.unwrap().params, vec![name.clone(), id.clone()]);
        let q = update(LITE, &t, &set, &filter);
        assert_eq!(
            sql(&q),
            r#"UPDATE "public"."t" SET "name" = ?1 WHERE "id" = ?2"#
        );
        let q = update(MY, &t, &set, &filter);
        assert_eq!(sql(&q), "UPDATE `public`.`t` SET `name` = ? WHERE `id` = ?");
    }

    #[test]
    fn insert_and_delete_shapes() {
        let t = table();
        let id = Value::Int(1);
        let flag = Value::Bool(true);
        let q = insert(PG, &t, &[("id", &id), ("flag", &flag)]);
        assert_eq!(
            sql(&q),
            r#"INSERT INTO "public"."t" ("id", "flag") VALUES ($1, $2)"#
        );
        let q = insert(MY, &t, &[("id", &id)]);
        assert_eq!(sql(&q), "INSERT INTO `public`.`t` (`id`) VALUES (?)");
        let q = delete(LITE, &t, &[("id", &id), ("flag", &flag)]);
        assert_eq!(
            sql(&q),
            r#"DELETE FROM "public"."t" WHERE "id" = ?1 AND "flag" = ?2"#
        );
    }

    #[test]
    fn null_set_is_bound_null_filter_is_is_null() {
        let t = table();
        let null = Value::Null;
        let id = Value::Int(1);
        let q = update(
            LITE,
            &t,
            &[("name", &null)],
            &[("id", &id), ("flag", &null)],
        );
        assert_eq!(
            sql(&q),
            r#"UPDATE "public"."t" SET "name" = ?1 WHERE "id" = ?2 AND "flag" IS NULL"#
        );
        assert_eq!(q.unwrap().params, vec![Value::Null, Value::Int(1)]);
        let q = delete(MY, &t, &[("name", &null)]);
        assert_eq!(sql(&q), "DELETE FROM `public`.`t` WHERE `name` IS NULL");
        assert_eq!(q.unwrap().params.len(), 0);
    }

    #[test]
    fn postgres_casts() {
        let t = table();
        let dec: Decimal = "1.50".parse().unwrap();
        let date = Date::new(2024, 2, 29).unwrap();
        let time = Time::new(1, 2, 3, 0).unwrap();
        let ts = Timestamp::new(date, time);
        let tz = TimestampTz::new(ts, 3600).unwrap();
        let cases: Vec<(&str, Value, &str)> = vec![
            ("price", Value::Decimal(dec), "CAST($1 AS numeric)"),
            ("day", Value::Date(date), "CAST($1 AS date)"),
            ("tm", Value::Time(time), "CAST($1 AS time)"),
            ("at", Value::Timestamp(ts), "CAST($1 AS timestamp)"),
            ("atz", Value::TimestampTz(tz), "CAST($1 AS timestamptz)"),
            ("doc", Value::Json("{}".into()), "CAST($1 AS jsonb)"),
            ("name", Value::Text("x".into()), "CAST($1 AS text)"),
            ("blob", Value::Bytes(vec![0, 1]), "$1"),
            ("id", Value::Int(1), "$1"),
            ("flag", Value::Bool(false), "$1"),
            ("price", Value::Float(1.5), "$1"),
            ("id", Value::Null, "CAST(CAST($1 AS text) AS bigint)"),
            ("price", Value::Null, "CAST(CAST($1 AS text) AS numeric)"),
            ("weird", Value::Null, "$1"),
            ("weird", Value::Text("x".into()), "$1"),
            // An unsafe native type falls back to the logical type.
            ("odd", Value::Text("x".into()), "CAST($1 AS text)"),
        ];
        for (col, value, expected) in cases {
            let q = insert(PG, &t, &[(col, &value)]);
            assert_eq!(
                sql(&q),
                format!(r#"INSERT INTO "public"."t" ("{col}") VALUES ({expected})"#)
            );
            assert_eq!(q.unwrap().params, vec![value]);
        }
    }

    fn pg_cast(native: &str) -> Option<String> {
        cast_type(native)
    }

    #[test]
    fn casts_carry_no_length_precision_or_field_modifier() {
        // An explicit cast to a constrained type truncates silently; the column's own modifier is
        // then checked by the server's assignment rules, which raise an error.
        for (native, expected) in [
            ("character varying(5)", "character varying"),
            ("character varying", "character varying"),
            ("character(3)", "bpchar"),
            ("character", "bpchar"),
            ("bpchar", "bpchar"),
            ("bit(4)", "bit varying"),
            ("bit varying(4)", "bit varying"),
            ("numeric(10,2)", "numeric"),
            ("numeric", "numeric"),
            ("timestamp(3) with time zone", "timestamp with time zone"),
            (
                "timestamp(3) without time zone",
                "timestamp without time zone",
            ),
            ("time(0) with time zone", "time with time zone"),
            ("interval(3)", "interval(3)"),
            ("interval", "interval"),
            ("interval day to second(3)", "interval day to second(3)"),
            ("interval year", "interval year"),
            ("interval year to month", "interval year to month"),
            ("interval(3)[]", "interval(3)[]"),
            ("character varying(5)[]", "character varying[]"),
            ("character(3)[][]", "bpchar[][]"),
            ("numeric(10,2)[3]", "numeric[]"),
            ("bit(4)[]", "bit varying[]"),
            ("int4", "int4"),
            ("public.my_type", "public.my_type"),
        ] {
            assert_eq!(pg_cast(native).as_deref(), Some(expected), "{native}");
        }
        assert_eq!(pg_cast("x[a]"), None);
        assert_eq!(pg_cast("(5)"), None);
        let t = table();
        let mut t = t;
        t.columns
            .push(ColumnInfo::new("v", DataType::Text).with_native_type("character varying(5)"));
        t.columns
            .push(ColumnInfo::new("c", DataType::Text).with_native_type("character(3)"));
        t.columns
            .push(ColumnInfo::new("b", DataType::Text).with_native_type("bit(4)"));
        let text = Value::Text("1234567".into());
        let q = insert(PG, &t, &[("v", &text), ("c", &text), ("b", &text)]);
        assert_eq!(
            sql(&q),
            r#"INSERT INTO "public"."t" ("v", "c", "b") VALUES (CAST($1 AS character varying), CAST($2 AS bpchar), CAST($3 AS bit varying))"#
        );
        let q = insert(PG, &t, &[("v", &Value::Null)]);
        assert_eq!(
            sql(&q),
            r#"INSERT INTO "public"."t" ("v") VALUES (CAST(CAST($1 AS text) AS character varying))"#
        );
        let q = delete(PG, &t, &[("v", &text)]);
        assert_eq!(
            sql(&q),
            r#"DELETE FROM "public"."t" WHERE "v" = CAST($1 AS character varying)"#
        );
    }

    #[test]
    fn domains_over_strings_are_cast_to_the_logical_type() {
        // A `Text` or `Decimal` column whose type name is not a built-in is a domain (or an array
        // of a user type): an explicit cast to a domain over varchar(5) would truncate silently.
        let pg = |data_type, native: &str| {
            pg_type(&ColumnInfo::new("c", data_type).with_native_type(native))
        };
        for (data_type, native, expected) in [
            (DataType::Text, "d5", "text"),
            (DataType::Text, "public.d5", "text"),
            (DataType::Text, r#""Odd Dom""#, "text"),
            (DataType::Text, r#"public."Odd Dom""#, "text"),
            (DataType::Text, "d5[]", "text[]"),
            (DataType::Text, r#""Odd Dom"[][]"#, "text[][]"),
            (DataType::Text, r#""Mixed Enum"[]"#, "text[]"),
            (DataType::Text, "x'; DROP TABLE t; --", "text"),
            (DataType::Decimal, "money_d", "numeric"),
            (DataType::Integer, "small_d", "bigint"),
            (DataType::Timestamp, "ts_d", "timestamp"),
            // Built-in spellings keep their own type.
            (DataType::Text, "character varying(5)", "character varying"),
            (DataType::Text, "uuid", "uuid"),
            (DataType::Text, "inet", "inet"),
            (DataType::Text, "integer[]", "integer[]"),
            (
                DataType::Text,
                "character varying(5)[]",
                "character varying[]",
            ),
            (DataType::Text, "interval year", "interval year"),
            (DataType::Text, "time with time zone", "time with time zone"),
            (DataType::Decimal, "numeric(10,2)", "numeric"),
            // `Other` is an enum or extension type and keeps its own name.
            (DataType::Other, "d_enum", "d_enum"),
            (DataType::Other, r#""Mixed Enum""#, r#""Mixed Enum""#),
            (DataType::Other, r#""Mixed Enum"[]"#, r#""Mixed Enum"[]"#),
        ] {
            assert_eq!(pg(data_type, native).as_deref(), Some(expected), "{native}");
        }
        let t = {
            let mut t = table();
            t.columns
                .push(ColumnInfo::new("d", DataType::Text).with_native_type("d5"));
            t
        };
        let long = Value::Text("1234567".into());
        let q = update(PG, &t, &[("d", &long)], &[("id", &Value::Int(1))]);
        assert_eq!(
            sql(&q),
            r#"UPDATE "public"."t" SET "d" = CAST($1 AS text) WHERE "id" = $2"#
        );
    }

    #[test]
    fn columns_that_would_change_the_value_are_refused() {
        let col = |native: &str| ColumnInfo::new("c", DataType::Text).with_native_type(native);
        for native in [
            r#""char""#,
            r#""char"[]"#,
            "interval year",
            "interval day to second(3)",
            "INTERVAL HOUR TO MINUTE",
            "interval year[]",
        ] {
            assert!(
                unwritable_reason(Dialect::Postgres, &col(native)).is_some(),
                "{native}"
            );
        }
        for native in [
            "interval",
            "interval(3)",
            "interval(3)[]",
            "intervalkind",
            "intervals",
            "interval_x",
            "intervalkind[]",
            "character(3)",
            "text",
            "d5",
            r#""Mixed Enum""#,
            "integer[]",
        ] {
            assert!(
                unwritable_reason(Dialect::Postgres, &col(native)).is_none(),
                "{native}"
            );
        }
        // Other dialects have no such spelling; a column without a native type is fine.
        assert!(unwritable_reason(Dialect::MySql, &col("interval year")).is_none());
        let plain = ColumnInfo::new("c", DataType::Text);
        assert!(unwritable_reason(Dialect::Postgres, &plain).is_none());

        let mut t = table();
        t.columns.push(col(r#""char""#));
        let error = update(
            PG,
            &t,
            &[("c", &Value::Text("xyz".into()))],
            &[("id", &Value::Int(1))],
        )
        .unwrap_err();
        assert!(matches!(error, DmlError::UnwritableType { .. }), "{error}");
        let error = insert(PG, &t, &[("c", &Value::Text("xyz".into()))]).unwrap_err();
        assert!(matches!(error, DmlError::UnwritableType { .. }));
        // Reading it as a filter value is not a write.
        assert!(delete(PG, &t, &[("c", &Value::Text("x".into()))]).is_ok());
    }

    #[test]
    fn quoted_type_names_are_requoted() {
        assert_eq!(
            pg_cast(r#""Mixed Enum""#).as_deref(),
            Some(r#""Mixed Enum""#)
        );
        assert_eq!(
            pg_cast(r#"public."Mixed Enum""#).as_deref(),
            Some(r#""public"."Mixed Enum""#)
        );
        assert_eq!(
            pg_cast(r#""Odd ""Schema"""."T"[]"#).as_deref(),
            Some(r#""Odd ""Schema"""."T"[]"#)
        );
        assert_eq!(pg_cast(r#""a$1?b c""#).as_deref(), Some(r#""a$1?b c""#));
        assert_eq!(pg_cast("\"caf\u{e9}\""), None);
        for bad in [
            r#""char""#,
            r#""it's""#,
            r#""""#,
            r#""unterminated"#,
            r#""a"(5)"#,
            r#""a" "b""#,
            r#""a"."b"."c""#,
            r#"a"b"#,
            "\"a\0b\"",
        ] {
            assert_eq!(pg_cast(bad), None, "{bad}");
        }
        let object = DbObject::new(ObjectKind::Table, ObjectPath::root().in_schema("s"), "t");
        let t = Table::new(object).with_columns(vec![
            ColumnInfo::new("a", DataType::Other).with_native_type(r#""Mixed Enum""#),
            ColumnInfo::new("b", DataType::Other).with_native_type(r#"x."a$1?b c""#),
        ]);
        let v = Value::Text("c'd".into());
        let q = insert(PG, &t, &[("a", &v), ("b", &v)]).unwrap();
        assert_eq!(
            q.sql,
            r#"INSERT INTO "s"."t" ("a", "b") VALUES (CAST($1 AS "Mixed Enum"), CAST($2 AS "x"."a$1?b c"))"#
        );
        assert_eq!(crate::placeholders(PG, &q.sql).len(), q.params.len());
    }

    #[test]
    fn mysql_json_filter_casts_the_parameter() {
        let t = table();
        let doc = Value::Json("{\"a\": 1}".into());
        let q = delete(MY, &t, &[("doc", &doc)]);
        assert_eq!(
            sql(&q),
            "DELETE FROM `public`.`t` WHERE `doc` = CAST(? AS JSON)"
        );
        assert_eq!(q.unwrap().params, vec![doc.clone()]);
        // SET stays a plain parameter; SQLite compares the text.
        let q = update(MY, &t, &[("doc", &doc)], &[("id", &Value::Int(1))]);
        assert_eq!(sql(&q), "UPDATE `public`.`t` SET `doc` = ? WHERE `id` = ?");
    }

    #[test]
    fn bound_columns_follow_the_parameters() {
        let t = table();
        let (name, null, doc) = (
            Value::Text("n".into()),
            Value::Null,
            Value::Json("{}".into()),
        );
        let id = Value::Int(1);
        for d in Dialect::ALL {
            let b = update_bound(
                d,
                &t,
                &[("name", &name), ("flag", &null)],
                &[("id", &id), ("blob", &null), ("doc", &doc), ("day", &null)],
            )
            .unwrap();
            assert_eq!(b.columns, ["name", "flag", "id", "doc"], "{d:?}");
            assert_eq!(b.columns.len(), b.query.params.len());
            let found = crate::placeholders(d, &b.query.sql);
            assert_eq!(found.len(), b.query.params.len(), "{}", b.query.sql);
            for (i, p) in found.iter().enumerate() {
                assert_eq!(
                    &b.query.sql[p.range.clone()],
                    param_marker(d, i + 1),
                    "{}",
                    b.query.sql
                );
            }
            let b = insert_bound(d, &t, &[("id", &id), ("name", &null)]).unwrap();
            assert_eq!(b.columns, ["id", "name"]);
            let b = delete_bound(d, &t, &[("name", &null), ("id", &id)]).unwrap();
            assert_eq!(b.columns, ["id"]);
        }
        assert_eq!(param_marker(PG, 3), "$3");
        assert_eq!(param_marker(LITE, 3), "?3");
        assert_eq!(param_marker(MY, 3), "?");
    }

    #[test]
    fn postgres_json_filter_compares_as_text() {
        let t = table();
        let doc = Value::Json("{\"a\": 1}".into());
        let q = delete(PG, &t, &[("doc", &doc)]);
        assert_eq!(
            sql(&q),
            r#"DELETE FROM "public"."t" WHERE CAST("doc" AS text) = $1"#
        );
        let q = delete(LITE, &t, &[("doc", &doc)]);
        assert_eq!(sql(&q), r#"DELETE FROM "public"."t" WHERE "doc" = ?1"#);
        let q = delete(PG, &t, &[("name", &Value::Text("n".into()))]);
        assert_eq!(
            sql(&q),
            r#"DELETE FROM "public"."t" WHERE "name" = CAST($1 AS text)"#
        );
    }

    #[test]
    fn other_dialects_never_cast() {
        let t = table();
        let v = Value::Decimal("2".parse().unwrap());
        for d in [MY, LITE] {
            let q = insert(d, &t, &[("price", &v)]).unwrap();
            assert!(!q.sql.contains("CAST"), "{}", q.sql);
        }
    }

    #[test]
    fn whitelisted_types_agree_with_placeholder_scan() {
        let object = DbObject::new(ObjectKind::Table, ObjectPath::root().in_schema("s"), "t");
        for native in [
            "numeric(10,2)",
            "character varying(20)[]",
            "public.my_type",
            "int[][]",
            "a ] b",
            "[a]",
            "x(y[z])",
            "double precision",
        ] {
            let t = Table::new(object.clone()).with_columns(vec![
                ColumnInfo::new("a", DataType::Text).with_native_type(native),
                ColumnInfo::new("b", DataType::Text).with_native_type(native),
            ]);
            let (a, b) = (Value::Text("x".into()), Value::Text("y".into()));
            let q = insert(PG, &t, &[("a", &a), ("b", &b)]).unwrap();
            let found = crate::placeholders(PG, &q.sql);
            assert_eq!(found.len(), q.params.len(), "{native}: {}", q.sql);
        }
    }

    #[test]
    fn safe_type_names() {
        assert!(is_safe_type_name("numeric(10,2)"));
        assert!(is_safe_type_name("character varying(20)[]"));
        assert!(!is_safe_type_name(r#""MyEnum""#));
        assert!(!is_safe_type_name(r#"["]""#));
        assert!(is_safe_type_name("public.my_type"));
        assert!(!is_safe_type_name(""));
        assert!(!is_safe_type_name("   "));
        assert!(!is_safe_type_name(r#""broken"#));
        assert!(!is_safe_type_name("a'b"));
        assert!(!is_safe_type_name("["));
        assert!(!is_safe_type_name("a)"));
        assert!(!is_safe_type_name("(a"));
        assert!(!is_safe_type_name("a]["));
        assert!(!is_safe_type_name("a;b"));
        assert!(!is_safe_type_name("a--b"));
        assert!(!is_safe_type_name("naïve"));
        assert!(!is_safe_type_name(&"a".repeat(129)));
        assert!(is_safe_type_name(&"a".repeat(128)));
    }

    #[test]
    fn identifiers_are_quoted() {
        let object = DbObject::new(ObjectKind::Table, ObjectPath::root(), "we\"ird");
        let t = Table::new(object).with_columns(vec![ColumnInfo::new("c`\"x", DataType::Integer)]);
        let v = Value::Int(1);
        let q = insert(PG, &t, &[("c`\"x", &v)]);
        assert_eq!(sql(&q), r#"INSERT INTO "we""ird" ("c`""x") VALUES ($1)"#);
        let q = insert(MY, &t, &[("c`\"x", &v)]);
        assert_eq!(sql(&q), "INSERT INTO `we\"ird` (`c``\"x`) VALUES (?)");
    }

    #[test]
    fn errors() {
        let t = table();
        let v = Value::Int(1);
        assert_eq!(
            update(PG, &t, &[("nope", &v)], &[("id", &v)]),
            Err(DmlError::UnknownColumn {
                column: "nope".into()
            })
        );
        assert_eq!(
            update(PG, &t, &[("id", &v)], &[("nope", &v)]),
            Err(DmlError::UnknownColumn {
                column: "nope".into()
            })
        );
        assert_eq!(
            update(PG, &t, &[("id", &v), ("id", &v)], &[("id", &v)]),
            Err(DmlError::DuplicateColumn {
                column: "id".into()
            })
        );
        assert_eq!(
            delete(PG, &t, &[("id", &v), ("id", &v)]),
            Err(DmlError::DuplicateColumn {
                column: "id".into()
            })
        );
        assert_eq!(
            insert(PG, &t, &[("id", &v), ("id", &v)]),
            Err(DmlError::DuplicateColumn {
                column: "id".into()
            })
        );
        assert_eq!(update(PG, &t, &[], &[("id", &v)]), Err(DmlError::EmptySet));
        assert_eq!(
            update(PG, &t, &[("id", &v)], &[]),
            Err(DmlError::EmptyFilter)
        );
        assert_eq!(delete(PG, &t, &[]), Err(DmlError::EmptyFilter));
        assert_eq!(insert(PG, &t, &[]), Err(DmlError::EmptyInsert));
        // Names that cannot be quoted.
        let bad = Table::new(DbObject::new(ObjectKind::Table, ObjectPath::root(), ""))
            .with_columns(vec![ColumnInfo::new("a\0", DataType::Integer)]);
        assert!(matches!(
            delete(PG, &bad, &[("a\0", &v)]),
            Err(DmlError::Identifier(_))
        ));
        let bad = Table::new(DbObject::new(ObjectKind::Table, ObjectPath::root(), "t"))
            .with_columns(vec![ColumnInfo::new("a\0", DataType::Integer)]);
        assert!(matches!(
            delete(PG, &bad, &[("a\0", &v)]),
            Err(DmlError::Identifier(_))
        ));
    }

    #[test]
    fn transaction_statements() {
        assert_eq!(transaction_sql(MY, TxStep::Begin), "START TRANSACTION");
        assert_eq!(transaction_sql(PG, TxStep::Begin), "BEGIN");
        assert_eq!(transaction_sql(LITE, TxStep::Begin), "BEGIN");
        for d in [PG, MY, LITE] {
            assert_eq!(transaction_sql(d, TxStep::Commit), "COMMIT");
            assert_eq!(transaction_sql(d, TxStep::Rollback), "ROLLBACK");
        }
        assert_eq!(transaction_sql(TS, TxStep::Begin), "BEGIN TRANSACTION");
        assert_eq!(transaction_sql(TS, TxStep::Commit), "COMMIT TRANSACTION");
        assert_eq!(
            transaction_sql(TS, TxStep::Rollback),
            "ROLLBACK TRANSACTION"
        );
        // Every generated step is itself classified as transaction control.
        for step in [TxStep::Begin, TxStep::Commit, TxStep::Rollback] {
            assert_eq!(
                crate::classify(TS, transaction_sql(TS, step)),
                StatementClass::Other
            );
        }
    }

    #[test]
    fn param_markers() {
        assert_eq!(param_marker(PG, 3), "$3");
        assert_eq!(param_marker(MY, 3), "?");
        assert_eq!(param_marker(LITE, 3), "?3");
        assert_eq!(param_marker(TS, 3), "@P3");
    }

    #[test]
    fn tsql_dml_and_rowversion() {
        let table = Table::new(DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("dbo"),
            "t",
        ))
        .with_columns(vec![
            ColumnInfo::new("id", DataType::Integer),
            ColumnInfo::new("a]", DataType::Text),
        ]);
        let v = Value::Int(1);
        let t = Value::Text("x".into());
        let q = update(TS, &table, &[("a]", &t)], &[("id", &v)]).unwrap();
        assert_eq!(q.sql, "UPDATE [dbo].[t] SET [a]]] = @P1 WHERE [id] = @P2");
        let q = insert(TS, &table, &[("id", &v), ("a]", &t)]).unwrap();
        assert_eq!(
            q.sql,
            "INSERT INTO [dbo].[t] ([id], [a]]]) VALUES (@P1, @P2)"
        );
        let q = delete(TS, &table, &[("id", &v)]).unwrap();
        assert_eq!(q.sql, "DELETE FROM [dbo].[t] WHERE [id] = @P1");
        for native in ["timestamp", "ROWVERSION", " rowversion "] {
            let c = ColumnInfo::new("rv", DataType::Binary).with_native_type(native);
            assert!(unwritable_reason(TS, &c).is_some(), "{native}");
        }
        for native in ["datetime2", "varbinary", "timestamp_x"] {
            let c = ColumnInfo::new("rv", DataType::Binary).with_native_type(native);
            assert!(unwritable_reason(TS, &c).is_none(), "{native}");
        }
        // The type spelling that is refused on T-SQL is an ordinary one elsewhere.
        let c = ColumnInfo::new("rv", DataType::Timestamp).with_native_type("timestamp");
        assert!(unwritable_reason(PG, &c).is_none());
        assert!(unwritable_reason(MY, &c).is_none());
        assert!(unwritable_reason(LITE, &c).is_none());
    }

    #[test]
    fn tsql_single_table_sources() {
        let s = src(TS, "select * from [dbo].[t]]x]").unwrap();
        assert_eq!(s.schema, Some(id("dbo", true)));
        assert_eq!(s.name, id("t]x", true));
        let s = src(TS, "SELECT TOP (100) *\nFROM [dbo].[t];\n").unwrap();
        assert_eq!(s.name, id("t", true));
        assert_eq!(s.columns, SelectList::Star);
        let s = src(TS, "select top 5 [a], \"b\" from t where a = 1").unwrap();
        assert_eq!(
            s.columns,
            SelectList::Columns(vec![id("a", true), id("b", true)])
        );
        assert!(src(TS, "select top (5) percent * from t").is_none());
        assert!(src(TS, "select top (5) with ties * from t").is_none());
        assert!(src(TS, "select top (x) * from t").is_none());
        assert!(src(TS, "select top (@n) * from t").is_none());
        assert!(src(TS, "select top (5 * from t").is_none());
        assert!(src(TS, "select top * from t").is_none());
        assert!(src(TS, "select * from t with (nolock)").is_none());
        // TOP is T-SQL syntax only.
        for d in [PG, MY, LITE] {
            assert!(src(d, "select top (5) * from t").is_none(), "{d:?}");
        }
        // A backtick name is no T-SQL identifier.
        assert!(src(TS, "select * from `t`").is_none());
        assert!(src(TS, "select * from \"t\"").is_some());
        assert!(src(PG, "select * from [t]").is_none());
        assert!(src(MY, "select * from [t]").is_none());
        assert!(src(LITE, "select * from [t]").is_some());
        // T-SQL names compare without regard to case.
        assert!(id("Users", false).matches(TS, "users"));
        assert!(same_column(TS, "ID", "id"));
    }

    fn src(d: Dialect, sql: &str) -> Option<SourceTable> {
        single_table_source(d, sql)
    }

    fn id(text: &str, quoted: bool) -> SqlIdent {
        SqlIdent {
            text: text.into(),
            quoted,
        }
    }

    #[test]
    fn accepts_plain_selects() {
        let s = src(PG, "select * from users").unwrap();
        assert_eq!(s.name, id("users", false));
        assert_eq!(s.schema, None);
        assert_eq!(s.columns, SelectList::Star);
        let s = src(
            PG,
            "SELECT a, \"B\" FROM public.t WHERE a > 1 ORDER BY a, b LIMIT 5;",
        )
        .unwrap();
        assert_eq!(s.schema, Some(id("public", false)));
        assert_eq!(
            s.columns,
            SelectList::Columns(vec![id("a", false), id("B", true)])
        );
        let s = src(
            MY,
            "SELECT `a``b` FROM `db`.`t` -- c\n WHERE x IN (SELECT 1 FROM u, v)",
        )
        .unwrap();
        assert_eq!(s.columns, SelectList::Columns(vec![id("a`b", true)]));
        let s = src(LITE, "select [a b], \"c\"\"d\" from [t t] limit 3 offset 1").unwrap();
        assert_eq!(
            s.columns,
            SelectList::Columns(vec![id("a b", true), id("c\"d", true)])
        );
        assert_eq!(s.name, id("t t", true));
        assert!(src(PG, "SELECT * FROM t FETCH FIRST 3 ROWS ONLY").is_some());
        assert!(src(PG, "SELECT * FROM t /* x */ ;  ").is_some());
    }

    #[test]
    fn accepts_select_star_output() {
        let object = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("s"),
            "we\"ird",
        );
        for d in Dialect::ALL {
            let sql = select_star(d, &object, 1000).unwrap();
            let s = src(d, &sql).unwrap_or_else(|| panic!("{sql}"));
            assert_eq!(s.columns, SelectList::Star);
            assert!(s.name.matches(d, "we\"ird") || d == MY, "{sql}");
        }
    }

    #[test]
    fn rejects_everything_else() {
        let cases = [
            "SELECT * FROM a JOIN b ON a.x = b.x",
            "SELECT * FROM a, b",
            "SELECT * FROM a NATURAL JOIN b",
            "SELECT x FROM a GROUP BY x",
            "SELECT * FROM a WHERE x = 1 GROUP BY x HAVING 1 = 1",
            "SELECT DISTINCT x FROM a",
            "SELECT * FROM a UNION SELECT * FROM b",
            "SELECT * FROM a WHERE x = 1 EXCEPT SELECT * FROM b",
            "SELECT count(*) FROM a",
            "SELECT x AS y FROM a",
            "SELECT x y FROM a",
            "SELECT a.x FROM a",
            "SELECT 1 FROM a",
            "SELECT 'lit' FROM a",
            "SELECT x + 1 FROM a",
            "SELECT * FROM a AS b",
            "SELECT * FROM a b",
            "SELECT * FROM (SELECT * FROM a) s",
            "WITH c AS (SELECT 1) SELECT * FROM c",
            "SELECT * FROM a; SELECT * FROM b",
            "SELECT * FROM a; DELETE FROM a",
            "SELECT * INTO b FROM a",
            "SELECT * FROM a LIMIT 1 INTO OUTFILE 'x'",
            "SELECT * FROM a WHERE x = 'unterminated",
            "SELECT * FROM \"a",
            "SELECT * FROM a /* open",
            "SELECT *, x FROM a",
            "SELECT * FROM",
            "SELECT * FROM a.b.c",
            "SELECT",
            "",
            "   ",
            "UPDATE a SET x = 1",
            "SELECT * FROM a FOR UPDATE",
            "SELECT * FROM a WINDOW w AS (PARTITION BY x)",
            "SELECT * FROM only a",
            "SELECT * FROM a TABLESAMPLE SYSTEM (1)",
            // Niladic functions and pseudo-columns are values, not columns.
            "SELECT user, name FROM t",
            "SELECT USER FROM t",
            "SELECT id, current_user FROM t",
            "SELECT session_user FROM t",
            "SELECT system_user FROM t",
            "SELECT current_role FROM t",
            "SELECT current_catalog FROM t",
            "SELECT current_schema FROM t",
            "SELECT current_date FROM t",
            "SELECT current_time FROM t",
            "SELECT CURRENT_TIMESTAMP, id FROM t",
            "SELECT localtime FROM t",
            "SELECT localtimestamp FROM t",
            "SELECT utc_date FROM t",
            "SELECT utc_time FROM t",
            "SELECT utc_timestamp FROM t",
            "SELECT sysdate FROM t",
            "SELECT default FROM t",
            "SELECT * FROM user",
            "SELECT * FROM current_user",
        ];
        for d in Dialect::ALL {
            for sql in cases {
                assert!(src(d, sql).is_none(), "{d:?}: {sql}");
            }
        }
        assert!(src(MY, "SELECT /*!40001 SQL_NO_CACHE */ * FROM a").is_none());
        assert!(src(MY, "SELECT SQL_CALC_FOUND_ROWS a FROM t").is_none());
        assert!(src(MY, "SELECT \"a\" FROM t").is_none());
        assert!(src(PG, "SELECT `a` FROM t").is_none());
        assert!(src(PG, "SELECT [a] FROM t").is_none());
        assert!(src(PG, "SELECT U&\"a\" FROM t").is_none());
        assert!(src(MY, "SELECT @v FROM t").is_none());
    }

    #[test]
    fn quoted_builtin_spellings_are_columns() {
        // A quoted identifier is a real column named like the function.
        let s = src(PG, r#"SELECT "user", "current_date" FROM t"#).unwrap();
        assert_eq!(
            s.columns,
            SelectList::Columns(vec![id("user", true), id("current_date", true)])
        );
        assert!(src(MY, "SELECT `user`, `utc_timestamp` FROM `user`").is_some());
        assert!(src(LITE, "SELECT [user], \"sysdate\" FROM \"default\"").is_some());
        // The qualified table part after a dot is a name, not a function.
        assert!(src(MY, "SELECT * FROM mysql.user").is_some());
        // The word as a prefix of a longer name is an ordinary column.
        assert!(src(PG, "SELECT user_id, users, current_user_x FROM t").is_some());
    }

    #[test]
    fn ident_matching() {
        assert!(id("Users", false).matches(PG, "users"));
        assert!(!id("Users", false).matches(PG, "Users"));
        assert!(id("Users", true).matches(PG, "Users"));
        assert!(!id("Users", true).matches(PG, "users"));
        assert!(id("Users", false).matches(MY, "uSERS"));
        assert!(id("Users", false).matches(LITE, "uSERS"));
        assert!(!id("Users", true).matches(LITE, "users"));
        // PostgreSQL folds ASCII letters only: unquoted É does not become é.
        assert!(!id("É", false).matches(PG, "é"));
        assert!(id("É", false).matches(PG, "É"));
        assert!(id("é", false).matches(PG, "é"));
        assert!(id("ÉA", false).matches(PG, "Éa"));
        assert!(same_column(PG, "a", "a"));
        assert!(!same_column(PG, "A", "a"));
        assert!(same_column(MY, "A", "a"));
        assert!(same_column(LITE, "A", "a"));
        assert!(!same_column(LITE, "b", "a"));
    }
}
