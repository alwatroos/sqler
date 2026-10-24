// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Dialects, identifier quoting, qualified names and SQL literals.

use std::fmt::Write as _;

use sqler_core::{DbObject, ObjectKind, ObjectLevel, ObjectPath, Value};

use crate::DdlError;

/// A SQL dialect the generator can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Dialect {
    /// `PostgreSQL`.
    Postgres,
    /// `MySQL` and `MariaDB`.
    MySql,
    /// `SQLite`.
    Sqlite,
    /// Microsoft SQL Server T-SQL.
    TSql,
}

impl Dialect {
    /// Every dialect.
    pub const ALL: [Dialect; 4] = [Self::Postgres, Self::MySql, Self::Sqlite, Self::TSql];

    /// The dialects that are also built-in driver ids.
    const BUILT_IN: [Dialect; 3] = [Self::Postgres, Self::MySql, Self::Sqlite];

    /// The name of this dialect: `postgres`, `mysql`, `sqlite` or `tsql`. For the first three it
    /// is also the driver id; `tsql` is only a dialect name (a plugin manifest declares it).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::Sqlite => "sqlite",
            Self::TSql => "tsql",
        }
    }

    /// Parses a built-in driver id (`postgres`, `mysql`, `sqlite`; exact match). Never returns
    /// [`Dialect::TSql`]: `tsql` is not a driver id, and a plugin driver reaches its dialect
    /// through [`Dialect::from_name`].
    #[must_use]
    pub fn from_driver_id(id: &str) -> Option<Self> {
        Self::BUILT_IN.into_iter().find(|d| d.as_str() == id)
    }

    /// Parses a dialect name as a plugin manifest declares it (exact, case-sensitive match on
    /// [`Dialect::as_str`]). An unknown name is `None`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == name)
    }

    fn quote_chars(self) -> (char, char) {
        match self {
            Self::Postgres | Self::Sqlite => ('"', '"'),
            Self::MySql => ('`', '`'),
            Self::TSql => ('[', ']'),
        }
    }
}

/// Quotes one identifier.
///
/// The name is always quoted and the dialect's quote character is doubled, so reserved words,
/// mixed case, spaces and embedded quotes need no special handling. Nothing else is changed.
///
/// # Errors
///
/// [`DdlError::InvalidIdentifier`] if the name is empty or contains NUL.
pub fn quote_identifier(dialect: Dialect, name: &str) -> Result<String, DdlError> {
    if name.is_empty() || name.contains('\0') {
        return Err(DdlError::InvalidIdentifier {
            name: name.to_owned(),
        });
    }
    let (open, close) = dialect.quote_chars();
    let mut out = String::with_capacity(name.len() + 2);
    out.push(open);
    for c in name.chars() {
        // Only the closing char is doubled: `[a]]b]` on T-SQL.
        if c == close {
            out.push(close);
        }
        out.push(c);
    }
    out.push(close);
    Ok(out)
}

/// Appends `value` as a SQL literal of `dialect` to `out`.
///
/// Pure and infallible. Every `'` in text is doubled, and text with a backslash or NUL is written
/// in a form that no `sql_mode` or `standard_conforming_strings` setting can misread (`E'..'` on
/// `PostgreSQL`, a hex string with a character-set introducer on `MySQL`, a hex `CAST` on
/// `SQLite`). Floats that SQL has no literal for are `NaN` and the infinities: `PostgreSQL` gets
/// casts, `SQLite` gets `9e999` (which it reads as infinity), `MySQL` and T-SQL get `NULL`. T-SQL text is
/// `N'..'`; text with a NUL or a backslash before a line break is a `CAST` of its UTF-16LE bytes.
///
/// # Examples
///
/// ```
/// use sqler_core::Value;
/// use sqler_sql::{Dialect, sql_literal};
///
/// assert_eq!(sql_literal(Dialect::Postgres, &Value::Text("it's".into())), "'it''s'");
/// assert_eq!(sql_literal(Dialect::Sqlite, &Value::Bool(true)), "1");
/// ```
pub fn push_literal(out: &mut String, dialect: Dialect, value: &Value) {
    match value {
        Value::Null => out.push_str("NULL"),
        Value::Bool(b) => out.push_str(match (dialect, b) {
            (Dialect::Sqlite | Dialect::TSql, true) => "1",
            (Dialect::Sqlite | Dialect::TSql, false) => "0",
            (Dialect::Postgres | Dialect::MySql, true) => "TRUE",
            (Dialect::Postgres | Dialect::MySql, false) => "FALSE",
        }),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::Float(x) => push_float(out, dialect, *x),
        Value::Decimal(d) => out.push_str(d.as_str()),
        Value::Text(t) | Value::Json(t) => push_string(out, dialect, t),
        Value::Bytes(bytes) => {
            let (open, close) = match dialect {
                Dialect::Postgres => ("decode('", "', 'hex')"),
                Dialect::MySql | Dialect::Sqlite => ("X'", "'"),
                Dialect::TSql => ("0x", ""),
            };
            out.push_str(open);
            push_hex(out, bytes);
            out.push_str(close);
        }
        Value::Date(_) => push_temporal(out, dialect, "DATE", value),
        Value::Time(_) => push_temporal(out, dialect, "TIME", value),
        Value::Timestamp(_) => push_temporal(out, dialect, "TIMESTAMP", value),
        Value::TimestampTz(_) => push_temporal(out, dialect, "TIMESTAMPTZ", value),
        other => push_string(out, dialect, &other.to_string()),
    }
}

/// [`push_literal`] into a new string.
#[must_use]
pub fn sql_literal(dialect: Dialect, value: &Value) -> String {
    let mut out = String::new();
    push_literal(&mut out, dialect, value);
    out
}

/// `text` as a string literal of `dialect` (the literal of a [`Value::Text`]).
#[must_use]
pub fn string_literal(dialect: Dialect, text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    push_string(&mut out, dialect, text);
    out
}

fn push_hex(out: &mut String, bytes: &[u8]) {
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
}

fn push_float(out: &mut String, dialect: Dialect, x: f64) {
    if x.is_finite() {
        if x == 0.0 && x.is_sign_negative() {
            // Checked on real servers (tests/db_*_float.rs): PostgreSQL types `-0.0e0` as numeric,
            // which has no negative zero, so it needs the quoted cast; MySQL 8.4 keeps the sign
            // of the double literal `-0.0e0`; SQLite keeps it in the expression but a REAL column
            // stores +0.0; MariaDB 11.8 stores +0.0 for every literal form.
            out.push_str(match dialect {
                Dialect::Postgres => "'-0'::float8",
                Dialect::MySql | Dialect::Sqlite | Dialect::TSql => "-0.0e0",
            });
        } else {
            let _ = write!(out, "{x:?}");
        }
        return;
    }
    out.push_str(match (dialect, x.is_nan(), x.is_sign_negative()) {
        (Dialect::Postgres, true, _) => "'NaN'::float8",
        (Dialect::Postgres, false, false) => "'Infinity'::float8",
        (Dialect::Postgres, false, true) => "'-Infinity'::float8",
        (Dialect::Sqlite, false, false) => "9e999",
        (Dialect::Sqlite, false, true) => "-9e999",
        (Dialect::Sqlite, true, _) | (Dialect::MySql | Dialect::TSql, _, _) => "NULL",
    });
}

fn push_temporal(out: &mut String, dialect: Dialect, keyword: &str, value: &Value) {
    match dialect {
        Dialect::Postgres => {
            out.push_str(keyword);
            out.push(' ');
        }
        Dialect::MySql | Dialect::Sqlite => {}
        Dialect::TSql => {
            // `YYYY-MM-DDThh:mm:ss` is the form that `datetime2` and `datetimeoffset` read the
            // same under every `SET DATEFORMAT` and language.
            if matches!(value, Value::Timestamp(_) | Value::TimestampTz(_)) {
                push_plain(out, &value.to_string().replacen(' ', "T", 1));
                return;
            }
        }
    }
    push_plain(out, &value.to_string());
}

/// `'text'` with every `'` doubled. Only for text without backslash and NUL, or in dialects where
/// those are harmless.
fn push_plain(out: &mut String, text: &str) {
    out.push('\'');
    for c in text.chars() {
        if c == '\'' {
            out.push('\'');
        }
        out.push(c);
    }
    out.push('\'');
}

fn push_string(out: &mut String, dialect: Dialect, text: &str) {
    let special = text.contains(['\\', '\0']);
    match dialect {
        Dialect::Postgres if special => {
            out.push_str("E'");
            for c in text.chars() {
                match c {
                    '\'' => out.push_str("''"),
                    '\\' => out.push_str("\\\\"),
                    '\0' => out.push_str("\\x00"),
                    c => out.push(c),
                }
            }
            out.push('\'');
        }
        Dialect::MySql if special => {
            out.push_str("_utf8mb4 X'");
            push_hex(out, text.as_bytes());
            out.push('\'');
        }
        Dialect::Sqlite if text.contains('\0') => {
            out.push_str("CAST(X'");
            push_hex(out, text.as_bytes());
            out.push_str("' AS TEXT)");
        }
        Dialect::TSql if tsql_needs_cast(text) => {
            // The UTF-16LE code units as a binary literal, read back as the same text.
            out.push_str("CAST(0x");
            for unit in text.encode_utf16() {
                let [lo, hi] = unit.to_le_bytes();
                let _ = write!(out, "{lo:02x}{hi:02x}");
            }
            out.push_str(" AS NVARCHAR(MAX))");
        }
        Dialect::TSql => {
            out.push('N');
            push_plain(out, text);
        }
        // Plain `'..'` is exact here: no backslash or NUL (PostgreSQL, MySQL), no NUL (SQLite).
        // Every dialect is named so that a new one must decide its escaping.
        Dialect::Postgres | Dialect::MySql | Dialect::Sqlite => push_plain(out, text),
    }
}

/// T-SQL text that a plain `N'..'` cannot carry: a NUL, or a backslash right before a line
/// break (the line continuation that removes both).
fn tsql_needs_cast(text: &str) -> bool {
    text.contains('\0') || text.contains("\\\r") || text.contains("\\\n")
}

/// The qualifier of a schema-level object: the schema, or on `MySQL` the schema or database.
fn qualifier(dialect: Dialect, path: &ObjectPath) -> Option<&str> {
    match dialect {
        Dialect::MySql => path.schema.as_deref().or(path.database.as_deref()),
        _ => path.schema.as_deref(),
    }
}

/// `qualifier.name`, or just `name` without a qualifier.
fn qualify(dialect: Dialect, path: &ObjectPath, name: &str) -> Result<String, DdlError> {
    match qualifier(dialect, path) {
        Some(q) => Ok(format!(
            "{}.{}",
            quote_identifier(dialect, q)?,
            quote_identifier(dialect, name)?
        )),
        None => quote_identifier(dialect, name),
    }
}

/// The quoted table an index or trigger belongs to, for `ON` clauses.
pub(crate) fn table_ref(
    dialect: Dialect,
    kind: ObjectKind,
    path: &ObjectPath,
) -> Result<String, DdlError> {
    let table = path
        .table
        .as_deref()
        .ok_or(DdlError::MissingTable { kind })?;
    qualify(dialect, path, table)
}

/// The fully qualified, fully quoted name of an object (the "copy name").
///
/// # Errors
///
/// [`DdlError::InvalidIdentifier`] if any part is empty or contains NUL.
pub fn qualified_name(dialect: Dialect, object: &DbObject) -> Result<String, DdlError> {
    match object.kind.level() {
        ObjectLevel::Schema => qualify(dialect, &object.parent, &object.name),
        _ => quote_identifier(dialect, &object.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(d: Dialect, n: &str) -> String {
        quote_identifier(d, n).unwrap()
    }

    #[test]
    fn dialect_ids() {
        for d in Dialect::ALL {
            assert_eq!(Dialect::from_name(d.as_str()), Some(d));
            // `tsql` is a dialect name, never a built-in driver id.
            let id = Dialect::from_driver_id(d.as_str());
            assert_eq!(id, (d != Dialect::TSql).then_some(d), "{d:?}");
        }
        assert_eq!(Dialect::from_driver_id("tsql"), None);
        assert_eq!(Dialect::from_driver_id("sqlserver"), None);
        for bad in [
            "TSql",
            "TSQL",
            "Postgres",
            "mssql",
            "sqlserver",
            " tsql",
            "",
        ] {
            assert_eq!(Dialect::from_name(bad), None, "{bad:?}");
        }
        assert_eq!(Dialect::from_driver_id("Postgres"), None);
        assert_eq!(Dialect::from_driver_id("mssql"), None);
        assert_eq!(Dialect::from_driver_id(""), None);
    }

    #[test]
    fn quoting_cases() {
        for d in [Dialect::Postgres, Dialect::Sqlite] {
            assert_eq!(q(d, "a\"b"), "\"a\"\"b\"");
            assert_eq!(q(d, "a`b"), "\"a`b\"");
            assert_eq!(q(d, "select"), "\"select\"");
            assert_eq!(q(d, "Mixed Case"), "\"Mixed Case\"");
            assert_eq!(q(d, "with space"), "\"with space\"");
            assert_eq!(q(d, "\""), "\"\"\"\"");
        }
        let d = Dialect::MySql;
        assert_eq!(q(d, "a`b"), "`a``b`");
        assert_eq!(q(d, "a\"b"), "`a\"b`");
        assert_eq!(q(d, "select"), "`select`");
        assert_eq!(q(d, "Mixed Case"), "`Mixed Case`");
        assert_eq!(q(d, "with space"), "`with space`");
        assert_eq!(q(d, "é 名"), "`é 名`");
        let d = Dialect::TSql;
        assert_eq!(q(d, "a]b"), "[a]]b]");
        assert_eq!(q(d, "a[b"), "[a[b]");
        assert_eq!(q(d, "a\"b`c"), "[a\"b`c]");
        assert_eq!(q(d, "select"), "[select]");
        assert_eq!(q(d, "Mixed Case"), "[Mixed Case]");
        assert_eq!(q(d, "]"), "[]]]");
        assert_eq!(q(d, "é 名"), "[é 名]");
    }

    #[test]
    fn invalid_identifiers() {
        for d in Dialect::ALL {
            for bad in ["", "a\0b", "\0"] {
                assert_eq!(
                    quote_identifier(d, bad),
                    Err(DdlError::InvalidIdentifier { name: bad.into() })
                );
            }
        }
    }

    #[test]
    fn qualification() {
        let with_schema = ObjectPath::root().in_database("d").in_schema("s");
        let db_only = ObjectPath::root().in_database("d");
        let t = |p: &ObjectPath| DbObject::new(ObjectKind::Table, p.clone(), "t");
        assert_eq!(
            qualified_name(Dialect::Postgres, &t(&with_schema)).unwrap(),
            "\"s\".\"t\""
        );
        assert_eq!(
            qualified_name(Dialect::Postgres, &t(&db_only)).unwrap(),
            "\"t\""
        );
        assert_eq!(
            qualified_name(Dialect::Sqlite, &t(&with_schema)).unwrap(),
            "\"s\".\"t\""
        );
        assert_eq!(
            qualified_name(Dialect::MySql, &t(&with_schema)).unwrap(),
            "`s`.`t`"
        );
        assert_eq!(
            qualified_name(Dialect::MySql, &t(&db_only)).unwrap(),
            "`d`.`t`"
        );
        assert_eq!(
            qualified_name(Dialect::MySql, &t(&ObjectPath::root())).unwrap(),
            "`t`"
        );
        // T-SQL: the schema is the qualifier, the database is not.
        assert_eq!(
            qualified_name(Dialect::TSql, &t(&with_schema)).unwrap(),
            "[s].[t]"
        );
        assert_eq!(qualified_name(Dialect::TSql, &t(&db_only)).unwrap(), "[t]");
    }

    #[test]
    fn bare_kinds_ignore_the_parent() {
        let parent = ObjectPath::root().in_database("d").in_schema("s");
        for kind in [
            ObjectKind::Database,
            ObjectKind::Role,
            ObjectKind::Schema,
            ObjectKind::Extension,
        ] {
            let o = DbObject::new(kind, parent.clone(), "n x");
            assert_eq!(qualified_name(Dialect::Postgres, &o).unwrap(), "\"n x\"");
            assert_eq!(qualified_name(Dialect::MySql, &o).unwrap(), "`n x`");
        }
    }

    #[test]
    fn invalid_parts_are_rejected() {
        let bad_schema = ObjectPath::root().in_schema("");
        let o = DbObject::new(ObjectKind::Table, bad_schema, "t");
        assert!(qualified_name(Dialect::Postgres, &o).is_err());
        let o = DbObject::new(ObjectKind::Table, ObjectPath::root(), "t\0");
        assert!(qualified_name(Dialect::Postgres, &o).is_err());
    }

    #[test]
    fn table_refs() {
        let p = ObjectPath::root().in_schema("s").in_table("t");
        assert_eq!(
            table_ref(Dialect::Postgres, ObjectKind::Trigger, &p).unwrap(),
            "\"s\".\"t\""
        );
        let p = ObjectPath::root().in_database("d").in_table("t");
        assert_eq!(
            table_ref(Dialect::MySql, ObjectKind::Index, &p).unwrap(),
            "`d`.`t`"
        );
        assert_eq!(
            table_ref(Dialect::MySql, ObjectKind::Index, &ObjectPath::root()),
            Err(DdlError::MissingTable {
                kind: ObjectKind::Index
            })
        );
    }

    mod literals {
        use sqler_core::{Date, Time, Timestamp, TimestampTz};

        use super::*;
        use crate::lexer::{TokenKind, tokenize};

        const PG: Dialect = Dialect::Postgres;
        const MY: Dialect = Dialect::MySql;
        const LITE: Dialect = Dialect::Sqlite;
        const TS: Dialect = Dialect::TSql;

        fn lit(d: Dialect, v: &Value) -> String {
            sql_literal(d, v)
        }

        fn text(s: &str) -> Value {
            Value::Text(s.into())
        }

        fn ts() -> Timestamp {
            Timestamp::new(
                Date::new(2024, 1, 2).unwrap(),
                Time::new(3, 4, 5, 0).unwrap(),
            )
        }

        #[test]
        fn simple_values() {
            for d in Dialect::ALL {
                assert_eq!(lit(d, &Value::Null), "NULL");
                assert_eq!(lit(d, &Value::Int(i64::MIN)), "-9223372036854775808");
                assert_eq!(lit(d, &Value::Decimal("-1.50".parse().unwrap())), "-1.50");
                assert_eq!(lit(d, &Value::Float(1.0)), "1.0");
                assert_eq!(lit(d, &Value::Float(1e300)), "1e300");
            }
            assert_eq!(lit(PG, &Value::Bool(true)), "TRUE");
            assert_eq!(lit(MY, &Value::Bool(false)), "FALSE");
            assert_eq!(lit(LITE, &Value::Bool(true)), "1");
            assert_eq!(lit(LITE, &Value::Bool(false)), "0");
            assert_eq!(lit(TS, &Value::Bool(true)), "1");
            assert_eq!(lit(TS, &Value::Bool(false)), "0");
        }

        #[test]
        fn negative_zero_literal_text() {
            assert_eq!(lit(Dialect::Postgres, &Value::Float(-0.0)), "'-0'::float8");
            assert_eq!(lit(Dialect::MySql, &Value::Float(-0.0)), "-0.0e0");
            assert_eq!(lit(Dialect::Sqlite, &Value::Float(-0.0)), "-0.0e0");
            assert_eq!(lit(Dialect::TSql, &Value::Float(-0.0)), "-0.0e0");
            for d in Dialect::ALL {
                assert_eq!(lit(d, &Value::Float(0.0)), "0.0", "{d:?}");
            }
        }

        #[test]
        fn non_finite_floats() {
            let (nan, inf, ninf) = (f64::NAN, f64::INFINITY, f64::NEG_INFINITY);
            assert_eq!(lit(PG, &Value::Float(nan)), "'NaN'::float8");
            assert_eq!(lit(PG, &Value::Float(inf)), "'Infinity'::float8");
            assert_eq!(lit(PG, &Value::Float(ninf)), "'-Infinity'::float8");
            for v in [nan, inf, ninf] {
                assert_eq!(lit(MY, &Value::Float(v)), "NULL");
                assert_eq!(lit(TS, &Value::Float(v)), "NULL");
            }
            assert_eq!(lit(LITE, &Value::Float(nan)), "NULL");
            assert_eq!(lit(LITE, &Value::Float(inf)), "9e999");
            assert_eq!(lit(LITE, &Value::Float(ninf)), "-9e999");
        }

        #[test]
        fn text_forms() {
            for v in [text("it's"), Value::Json("{\"a\":\"it's\"}".into())] {
                let want = if matches!(v, Value::Text(_)) {
                    "'it''s'"
                } else {
                    "'{\"a\":\"it''s\"}'"
                };
                for d in Dialect::ALL {
                    let want = if d == Dialect::TSql {
                        format!("N{want}")
                    } else {
                        want.to_owned()
                    };
                    assert_eq!(lit(d, &v), want);
                }
            }
            let ts = Dialect::TSql;
            assert_eq!(lit(ts, &text("")), "N''");
            assert_eq!(lit(ts, &text("a\\b")), "N'a\\b'");
            assert_eq!(lit(ts, &text("é名")), "N'é名'");
            // A NUL, or a backslash right before a line break, becomes the UTF-16LE bytes.
            assert_eq!(
                lit(ts, &text("a\0b")),
                "CAST(0x610000006200 AS NVARCHAR(MAX))"
            );
            assert_eq!(
                lit(ts, &text("a\\\nb")),
                "CAST(0x61005c000a006200 AS NVARCHAR(MAX))"
            );
            assert_eq!(lit(ts, &text("\\\r")), "CAST(0x5c000d00 AS NVARCHAR(MAX))");
            // Outside the BMP: a surrogate pair.
            assert_eq!(
                lit(ts, &text("\0\u{1F600}")),
                "CAST(0x00003dd800de AS NVARCHAR(MAX))"
            );
            assert_eq!(lit(PG, &text("")), "''");
            assert_eq!(lit(PG, &text("a\\b'\0")), "E'a\\\\b''\\x00'");
            assert_eq!(lit(MY, &text("a\\")), "_utf8mb4 X'615c'");
            assert_eq!(lit(MY, &text("a\0")), "_utf8mb4 X'6100'");
            assert_eq!(lit(LITE, &text("a\\b")), "'a\\b'");
            assert_eq!(lit(LITE, &text("a\0")), "CAST(X'6100' AS TEXT)");
            assert_eq!(lit(PG, &text("é名")), "'é名'");
            assert_eq!(string_literal(MY, "x'y"), "'x''y'");
        }

        #[test]
        fn bytes() {
            let b = Value::Bytes(vec![0, 0xab, 0xff]);
            assert_eq!(lit(PG, &b), "decode('00abff', 'hex')");
            assert_eq!(lit(MY, &b), "X'00abff'");
            assert_eq!(lit(LITE, &b), "X'00abff'");
            assert_eq!(lit(PG, &Value::Bytes(vec![])), "decode('', 'hex')");
            assert_eq!(lit(MY, &Value::Bytes(vec![])), "X''");
            assert_eq!(lit(TS, &b), "0x00abff");
            assert_eq!(lit(TS, &Value::Bytes(vec![])), "0x");
        }

        #[test]
        fn temporal() {
            let date = Value::Date(Date::new(2024, 1, 2).unwrap());
            let time = Value::Time(Time::new(3, 4, 5, 0).unwrap());
            let stamp = Value::Timestamp(ts());
            let tz = Value::TimestampTz(TimestampTz::new(ts(), 3600).unwrap());
            assert_eq!(lit(PG, &date), "DATE '2024-01-02'");
            assert_eq!(lit(PG, &time), "TIME '03:04:05'");
            assert_eq!(lit(PG, &stamp), "TIMESTAMP '2024-01-02 03:04:05'");
            assert_eq!(lit(PG, &tz), "TIMESTAMPTZ '2024-01-02 03:04:05+01:00'");
            for d in [MY, LITE] {
                assert_eq!(lit(d, &date), "'2024-01-02'");
                assert_eq!(lit(d, &time), "'03:04:05'");
                assert_eq!(lit(d, &stamp), "'2024-01-02 03:04:05'");
                assert_eq!(lit(d, &tz), "'2024-01-02 03:04:05+01:00'");
            }
            assert_eq!(lit(TS, &date), "'2024-01-02'");
            assert_eq!(lit(TS, &time), "'03:04:05'");
            assert_eq!(lit(TS, &stamp), "'2024-01-02T03:04:05'");
            assert_eq!(lit(TS, &tz), "'2024-01-02T03:04:05+01:00'");
            let far = Value::Date(Date::new(12345, 1, 2).unwrap());
            assert_eq!(lit(PG, &far), "DATE '+12345-01-02'");
        }

        /// Significant (non-whitespace) token kinds and texts.
        fn sig(d: Dialect, s: &str) -> Vec<(TokenKind, String)> {
            let tokens = tokenize(d, s);
            assert!(tokens.iter().all(crate::lexer::Token::is_terminated), "{s}");
            tokens
                .iter()
                .filter(|t| t.kind != TokenKind::Whitespace)
                .map(|t| (t.kind, t.text(s).to_owned()))
                .collect()
        }

        fn is_str(t: &(TokenKind, String)) -> bool {
            t.0 == TokenKind::Str { terminated: true }
        }

        #[test]
        fn text_relexes_into_the_expected_tokens() {
            let nasty = [
                "",
                "'",
                "''",
                "\\",
                "\\'",
                "a\0b",
                "x'; DROP TABLE t; --",
                "$$",
                "/*",
                "\"",
                "é\\名'",
                "\\\n",
                "a\\\r\nb'; drop table t; --",
                "\\ \n",
            ];
            for d in Dialect::ALL {
                for t in nasty {
                    let out = string_literal(d, t);
                    let toks = sig(d, &out);
                    let hex_form = (d == MY && t.contains(['\\', '\0']))
                        || (d == LITE && t.contains('\0'))
                        || (d == TS && tsql_needs_cast(t));
                    if d == TS && hex_form {
                        let texts: Vec<_> = toks.iter().map(|t| t.1.as_str()).collect();
                        assert_eq!(toks.len(), 9, "{out}");
                        assert_eq!(texts[..2], ["CAST", "("]);
                        assert_eq!(toks[2].0, TokenKind::Number);
                        assert_eq!(texts[3..], ["AS", "NVARCHAR", "(", "MAX", ")", ")"]);
                    } else if d == MY && hex_form {
                        assert_eq!(toks.len(), 2, "{out}");
                        assert_eq!(toks[0], (TokenKind::Word, "_utf8mb4".to_owned()));
                        assert!(is_str(&toks[1]), "{out}");
                    } else if d == LITE && hex_form {
                        let texts: Vec<_> = toks.iter().map(|t| t.1.as_str()).collect();
                        assert_eq!(toks.len(), 6, "{out}");
                        assert_eq!(texts[..2], ["CAST", "("]);
                        assert!(is_str(&toks[2]));
                        assert_eq!(texts[3..], ["AS", "TEXT", ")"]);
                    } else {
                        assert_eq!(toks.len(), 1, "{out}");
                        assert!(is_str(&toks[0]), "{out}");
                    }
                }
            }
        }

        #[test]
        fn bytes_relex_into_the_expected_tokens() {
            let b = Value::Bytes(vec![1, 2, 255]);
            let toks = sig(PG, &lit(PG, &b));
            let texts: Vec<_> = toks.iter().map(|t| t.1.as_str()).collect();
            assert_eq!(texts, ["decode", "(", "'0102ff'", ",", "'hex'", ")"]);
            for d in [MY, LITE] {
                let toks = sig(d, &lit(d, &b));
                assert_eq!(toks.len(), 1);
                assert!(is_str(&toks[0]));
            }
            let toks = sig(TS, &lit(TS, &b));
            assert_eq!(toks, vec![(TokenKind::Number, "0x0102ff".to_owned())]);
        }
    }
}
