// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Dialects, identifier quoting and qualified names.

use sqler_core::{DbObject, ObjectKind, ObjectLevel, ObjectPath};

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
}

impl Dialect {
    /// Every dialect.
    pub const ALL: [Dialect; 3] = [Self::Postgres, Self::MySql, Self::Sqlite];

    /// The driver id of this dialect: `postgres`, `mysql` or `sqlite`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::Sqlite => "sqlite",
        }
    }

    /// Parses a driver id produced by [`Dialect::as_str`] (exact match).
    #[must_use]
    pub fn from_driver_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == id)
    }

    fn quote_char(self) -> char {
        match self {
            Self::Postgres | Self::Sqlite => '"',
            Self::MySql => '`',
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
    let quote = dialect.quote_char();
    let mut out = String::with_capacity(name.len() + 2);
    out.push(quote);
    for c in name.chars() {
        if c == quote {
            out.push(quote);
        }
        out.push(c);
    }
    out.push(quote);
    Ok(out)
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
            assert_eq!(Dialect::from_driver_id(d.as_str()), Some(d));
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
}
