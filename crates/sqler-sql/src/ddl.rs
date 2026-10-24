// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The scripts of the object browser: `CREATE`, `SELECT *`, `INSERT` template and `DROP`.

use sqler_core::{
    ColumnInfo, DataType, DbObject, ForeignKey, ObjectKind, ReferentialAction, Table,
};

use crate::dialect::table_ref;
use crate::{DdlError, Dialect, qualified_name, quote_identifier};

/// The row limit the object browser uses for `SELECT *` unless the user changes it.
pub const DEFAULT_ROW_LIMIT: u32 = 1000;

/// A script the object browser can generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Script {
    /// `CREATE`.
    Create,
    /// `SELECT *` with a row limit.
    SelectStar,
    /// `INSERT` template with placeholders.
    InsertTemplate,
    /// `DROP`.
    Drop,
}

impl Script {
    /// Every script.
    pub const ALL: [Script; 4] = [
        Self::Create,
        Self::SelectStar,
        Self::InsertTemplate,
        Self::Drop,
    ];

    /// The kebab-case identifier: `create`, `select-star`, `insert-template` or `drop`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::SelectStar => "select-star",
            Self::InsertTemplate => "insert-template",
            Self::Drop => "drop",
        }
    }
}

/// Whether the dialect has this script for this kind of object.
#[must_use]
pub fn supports(dialect: Dialect, kind: ObjectKind, script: Script) -> bool {
    use ObjectKind as K;
    match script {
        Script::Create | Script::Drop => match dialect {
            Dialect::Postgres => ObjectKind::ALL.contains(&kind),
            Dialect::MySql => matches!(
                kind,
                K::Database
                    | K::Schema
                    | K::Table
                    | K::View
                    | K::Function
                    | K::Procedure
                    | K::Trigger
                    | K::Index
                    | K::Sequence
            ),
            Dialect::Sqlite => matches!(kind, K::Table | K::View | K::Trigger | K::Index),
            // T-SQL scripts come from the server's definitions, not from this generator.
            Dialect::TSql => false,
        },
        Script::SelectStar => {
            matches!(kind, K::Table | K::View)
                || (dialect == Dialect::Postgres && kind == K::MaterializedView)
        }
        Script::InsertTemplate => kind == K::Table,
    }
}

fn check(dialect: Dialect, kind: ObjectKind, script: Script) -> Result<(), DdlError> {
    if supports(dialect, kind, script) {
        Ok(())
    } else {
        Err(DdlError::Unsupported {
            dialect,
            kind,
            script,
        })
    }
}

/// The comma-separated, quoted column list.
fn column_list(dialect: Dialect, names: &[String]) -> Result<String, DdlError> {
    let quoted = names
        .iter()
        .map(|n| quote_identifier(dialect, n))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(quoted.join(", "))
}

/// The type to write for a column: the server's spelling, else a fallback by logical type.
#[allow(
    clippy::match_same_arms,
    reason = "one row per dialect and type keeps the map comparable to the documented table"
)]
fn column_type(dialect: Dialect, column: &ColumnInfo) -> Option<String> {
    if let Some(native) = column.native_type.as_deref()
        && !native.trim().is_empty()
    {
        return Some(native.to_owned());
    }
    let fallback = match (dialect, column.data_type) {
        (Dialect::Postgres, DataType::Boolean) => "BOOLEAN",
        (Dialect::Postgres, DataType::Integer) => "BIGINT",
        (Dialect::Postgres, DataType::Float) => "DOUBLE PRECISION",
        (Dialect::Postgres, DataType::Decimal) => "NUMERIC",
        (Dialect::Postgres, DataType::Text) => "TEXT",
        (Dialect::Postgres, DataType::Binary) => "BYTEA",
        (Dialect::Postgres, DataType::Date) => "DATE",
        (Dialect::Postgres, DataType::Time) => "TIME",
        (Dialect::Postgres, DataType::Timestamp) => "TIMESTAMP",
        (Dialect::Postgres, DataType::TimestampTz) => "TIMESTAMPTZ",
        (Dialect::Postgres, DataType::Json) => "JSONB",
        (Dialect::Postgres, _) => "TEXT",
        (Dialect::MySql, DataType::Boolean) => "BOOLEAN",
        (Dialect::MySql, DataType::Integer) => "BIGINT",
        (Dialect::MySql, DataType::Float) => "DOUBLE",
        (Dialect::MySql, DataType::Decimal) => "DECIMAL(65,30)",
        (Dialect::MySql, DataType::Text) => "LONGTEXT",
        (Dialect::MySql, DataType::Binary) => "LONGBLOB",
        (Dialect::MySql, DataType::Date) => "DATE",
        (Dialect::MySql, DataType::Time) => "TIME(6)",
        (Dialect::MySql, DataType::Timestamp) => "DATETIME(6)",
        (Dialect::MySql, DataType::TimestampTz) => "TIMESTAMP(6)",
        (Dialect::MySql, DataType::Json) => "JSON",
        (Dialect::MySql, _) => "LONGTEXT",
        (Dialect::Sqlite, DataType::Boolean) => "BOOLEAN",
        (Dialect::Sqlite, DataType::Integer) => "INTEGER",
        (Dialect::Sqlite, DataType::Float) => "REAL",
        (Dialect::Sqlite, DataType::Decimal) => "NUMERIC",
        (Dialect::Sqlite, DataType::Text) => "TEXT",
        (Dialect::Sqlite, DataType::Binary) => "BLOB",
        (Dialect::Sqlite, DataType::Date) => "DATE",
        (Dialect::Sqlite, DataType::Time) => "TIME",
        (Dialect::Sqlite, DataType::Timestamp) => "TIMESTAMP",
        (Dialect::Sqlite, DataType::TimestampTz) => "TIMESTAMPTZ",
        (Dialect::Sqlite, DataType::Json) => "JSON",
        // SQLite columns may have no type.
        (Dialect::Sqlite, _) => return None,
        (Dialect::TSql, DataType::Boolean) => "BIT",
        (Dialect::TSql, DataType::Integer) => "BIGINT",
        (Dialect::TSql, DataType::Float) => "FLOAT",
        (Dialect::TSql, DataType::Decimal) => "DECIMAL(38,10)",
        (Dialect::TSql, DataType::Binary) => "VARBINARY(MAX)",
        (Dialect::TSql, DataType::Date) => "DATE",
        (Dialect::TSql, DataType::Time) => "TIME(7)",
        (Dialect::TSql, DataType::Timestamp) => "DATETIME2(7)",
        (Dialect::TSql, DataType::TimestampTz) => "DATETIMEOFFSET(7)",
        (Dialect::TSql, _) => "NVARCHAR(MAX)",
    };
    Some(fallback.to_owned())
}

fn column_definition(dialect: Dialect, column: &ColumnInfo) -> Result<String, DdlError> {
    let mut out = quote_identifier(dialect, &column.name)?;
    if let Some(ty) = column_type(dialect, column) {
        out.push(' ');
        out.push_str(&ty);
    }
    if let Some(default) = column.default.as_deref() {
        out.push_str(" DEFAULT ");
        out.push_str(default);
    }
    if !column.nullable {
        out.push_str(" NOT NULL");
    }
    Ok(out)
}

fn action_text(action: ReferentialAction) -> Option<&'static str> {
    match action {
        ReferentialAction::Restrict => Some("RESTRICT"),
        ReferentialAction::Cascade => Some("CASCADE"),
        ReferentialAction::SetNull => Some("SET NULL"),
        ReferentialAction::SetDefault => Some("SET DEFAULT"),
        _ => None,
    }
}

fn foreign_key_definition(dialect: Dialect, fk: &ForeignKey) -> Result<String, DdlError> {
    let mut out = String::new();
    if let Some(name) = fk.name.as_deref() {
        out.push_str("CONSTRAINT ");
        out.push_str(&quote_identifier(dialect, name)?);
        out.push(' ');
    }
    out.push_str("FOREIGN KEY (");
    out.push_str(&column_list(dialect, &fk.columns)?);
    out.push_str(") REFERENCES ");
    // SQLite's foreign-key clause takes a bare table name, no schema prefix.
    if dialect != Dialect::Sqlite
        && let Some(schema) = fk.referenced_schema.as_deref()
    {
        out.push_str(&quote_identifier(dialect, schema)?);
        out.push('.');
    }
    out.push_str(&quote_identifier(dialect, &fk.referenced_table)?);
    if !fk.referenced_columns.is_empty() {
        out.push_str(" (");
        out.push_str(&column_list(dialect, &fk.referenced_columns)?);
        out.push(')');
    }
    if let Some(text) = action_text(fk.on_update) {
        out.push_str(" ON UPDATE ");
        out.push_str(text);
    }
    if let Some(text) = action_text(fk.on_delete) {
        out.push_str(" ON DELETE ");
        out.push_str(text);
    }
    Ok(out)
}

/// Trims a definition, drops trailing `;` and whitespace and ends it with `;\n`.
/// Returns `None` when nothing is left.
fn normalise(text: &str) -> Option<String> {
    let body = text
        .trim()
        .trim_end_matches(|c: char| c == ';' || c.is_whitespace());
    if body.is_empty() {
        None
    } else {
        Some(format!("{body};\n"))
    }
}

fn starts_with_create(text: &str) -> bool {
    text.trim_start()
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("CREATE"))
}

/// `CREATE TABLE` from the model: columns, defaults, nullability, primary and foreign keys.
///
/// `index_definitions` are the server's definition texts of the table's indexes; each is
/// normalised and appended as its own statement. `default` and `native_type` are written as the
/// server reports them, not escaped.
///
/// # Errors
///
/// [`DdlError::Unsupported`] if the object is not a table the dialect can create,
/// [`DdlError::EmptyTable`] for a table without columns on `MySQL` or `SQLite`, and
/// [`DdlError::InvalidIdentifier`] for a bad name.
pub fn create_table(
    dialect: Dialect,
    table: &Table,
    index_definitions: &[String],
) -> Result<String, DdlError> {
    let kind = table.object.kind;
    if kind != ObjectKind::Table {
        return Err(DdlError::Unsupported {
            dialect,
            kind,
            script: Script::Create,
        });
    }
    check(dialect, kind, Script::Create)?;
    let mut elements = Vec::new();
    for column in &table.columns {
        elements.push(column_definition(dialect, column)?);
    }
    if let Some(pk) = &table.primary_key {
        let mut e = String::new();
        if let Some(name) = pk.name.as_deref() {
            e.push_str("CONSTRAINT ");
            e.push_str(&quote_identifier(dialect, name)?);
            e.push(' ');
        }
        e.push_str("PRIMARY KEY (");
        e.push_str(&column_list(dialect, &pk.columns)?);
        e.push(')');
        elements.push(e);
    }
    for fk in &table.foreign_keys {
        elements.push(foreign_key_definition(dialect, fk)?);
    }
    let mut out = String::from("CREATE TABLE ");
    out.push_str(&qualified_name(dialect, &table.object)?);
    if elements.is_empty() {
        // Only PostgreSQL accepts a table without columns.
        if dialect != Dialect::Postgres {
            return Err(DdlError::EmptyTable { dialect });
        }
        out.push_str(" ();\n");
    } else {
        out.push_str(" (\n");
        let indented: Vec<String> = elements.iter().map(|e| format!("    {e}")).collect();
        out.push_str(&indented.join(",\n"));
        out.push_str("\n);\n");
    }
    for definition in index_definitions {
        if let Some(statement) = normalise(definition) {
            out.push('\n');
            out.push_str(&statement);
        }
    }
    Ok(out)
}

/// `CREATE` for any object, from the server's definition text.
///
/// `definition` is the text of `Introspect::definition`, written as the server reports it.
/// Views whose text is a bare query are wrapped in `CREATE VIEW ... AS`. Schemas, databases,
/// extensions and roles need no text. For a table with the model at hand use [`create_table`].
///
/// # Errors
///
/// [`DdlError::Unsupported`], [`DdlError::MissingDefinition`] or [`DdlError::InvalidIdentifier`].
pub fn create_script(
    dialect: Dialect,
    object: &DbObject,
    definition: Option<&str>,
) -> Result<String, DdlError> {
    let kind = object.kind;
    check(dialect, kind, Script::Create)?;
    let text = definition.and_then(normalise);
    match (kind, text) {
        (ObjectKind::View | ObjectKind::MaterializedView, Some(body)) => {
            if starts_with_create(&body) {
                Ok(body)
            } else {
                let keyword = if kind == ObjectKind::View {
                    "VIEW"
                } else {
                    "MATERIALIZED VIEW"
                };
                Ok(format!(
                    "CREATE {keyword} {} AS\n{body}",
                    qualified_name(dialect, object)?
                ))
            }
        }
        (_, Some(body)) => Ok(body),
        (kind, None) => {
            let keyword = match kind {
                ObjectKind::Schema if dialect == Dialect::MySql => "DATABASE",
                ObjectKind::Schema => "SCHEMA",
                ObjectKind::Database => "DATABASE",
                ObjectKind::Extension => "EXTENSION",
                ObjectKind::Role => "ROLE",
                _ => return Err(DdlError::MissingDefinition { kind }),
            };
            Ok(format!(
                "CREATE {keyword} {};\n",
                quote_identifier(dialect, &object.name)?
            ))
        }
    }
}

/// `SELECT * FROM <object> LIMIT <limit>`; on T-SQL `SELECT TOP (<limit>) * FROM <object>`.
///
/// # Errors
///
/// [`DdlError::Unsupported`] or [`DdlError::InvalidIdentifier`].
pub fn select_star(dialect: Dialect, object: &DbObject, limit: u32) -> Result<String, DdlError> {
    check(dialect, object.kind, Script::SelectStar)?;
    let name = qualified_name(dialect, object)?;
    Ok(match dialect {
        Dialect::TSql => format!("SELECT TOP ({limit}) *\nFROM {name};\n"),
        Dialect::Postgres | Dialect::MySql | Dialect::Sqlite => {
            format!("SELECT *\nFROM {name}\nLIMIT {limit};\n")
        }
    })
}

/// An `INSERT` with every column and the dialect's bind placeholders (`$1`, `?` or `@P1`).
///
/// # Errors
///
/// [`DdlError::Unsupported`] or [`DdlError::InvalidIdentifier`].
pub fn insert_template(dialect: Dialect, table: &Table) -> Result<String, DdlError> {
    check(dialect, table.object.kind, Script::InsertTemplate)?;
    let name = qualified_name(dialect, &table.object)?;
    if table.columns.is_empty() {
        return Ok(match dialect {
            Dialect::MySql => format!("INSERT INTO {name} () VALUES ();\n"),
            Dialect::Postgres | Dialect::Sqlite | Dialect::TSql => {
                format!("INSERT INTO {name} DEFAULT VALUES;\n")
            }
        });
    }
    let names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
    let placeholders: Vec<String> = (1..=names.len())
        .map(|i| match dialect {
            Dialect::Postgres => format!("${i}"),
            Dialect::TSql => crate::param_marker(dialect, i),
            Dialect::MySql | Dialect::Sqlite => "?".to_owned(),
        })
        .collect();
    Ok(format!(
        "INSERT INTO {name} ({})\nVALUES ({});\n",
        column_list(dialect, &names)?,
        placeholders.join(", ")
    ))
}

/// A plain `DROP` for the object. Never `IF EXISTS`, never `CASCADE`.
///
/// # Errors
///
/// [`DdlError::Unsupported`], [`DdlError::MissingTable`] (trigger on `PostgreSQL`, index on
/// `MySQL`, without `parent.table`) or [`DdlError::InvalidIdentifier`].
pub fn drop_script(dialect: Dialect, object: &DbObject) -> Result<String, DdlError> {
    let kind = object.kind;
    check(dialect, kind, Script::Drop)?;
    let on_table = (dialect == Dialect::Postgres && kind == ObjectKind::Trigger)
        || (dialect == Dialect::MySql && kind == ObjectKind::Index);
    if on_table {
        let keyword = if kind == ObjectKind::Trigger {
            "TRIGGER"
        } else {
            "INDEX"
        };
        return Ok(format!(
            "DROP {keyword} {} ON {};\n",
            quote_identifier(dialect, &object.name)?,
            table_ref(dialect, kind, &object.parent)?
        ));
    }
    if dialect == Dialect::MySql && matches!(kind, ObjectKind::Schema | ObjectKind::Database) {
        return Ok(format!(
            "DROP DATABASE {};\n",
            quote_identifier(dialect, &object.name)?
        ));
    }
    let keyword = match kind {
        ObjectKind::Database => "DATABASE",
        ObjectKind::Schema => "SCHEMA",
        ObjectKind::Table => "TABLE",
        ObjectKind::View => "VIEW",
        ObjectKind::MaterializedView => "MATERIALIZED VIEW",
        ObjectKind::Function => "FUNCTION",
        ObjectKind::Procedure => "PROCEDURE",
        ObjectKind::Trigger => "TRIGGER",
        ObjectKind::Sequence => "SEQUENCE",
        ObjectKind::Type => "TYPE",
        ObjectKind::Index => "INDEX",
        ObjectKind::Extension => "EXTENSION",
        ObjectKind::Role => "ROLE",
        _ => {
            return Err(DdlError::Unsupported {
                dialect,
                kind,
                script: Script::Drop,
            });
        }
    };
    Ok(format!(
        "DROP {keyword} {};\n",
        qualified_name(dialect, object)?
    ))
}

#[cfg(test)]
mod tests {
    use sqler_core::{ObjectPath, PrimaryKey};

    use super::*;

    fn obj(kind: ObjectKind, name: &str) -> DbObject {
        DbObject::new(kind, ObjectPath::root().in_schema("s"), name)
    }

    #[test]
    fn script_ids() {
        let ids: Vec<_> = Script::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(ids, ["create", "select-star", "insert-template", "drop"]);
    }

    #[test]
    fn support_matrix_counts() {
        let count = |d: Dialect, s: Script| {
            ObjectKind::ALL
                .into_iter()
                .filter(|k| supports(d, *k, s))
                .count()
        };
        for s in [Script::Create, Script::Drop] {
            assert_eq!(count(Dialect::Postgres, s), 13);
            assert_eq!(count(Dialect::MySql, s), 9);
            assert_eq!(count(Dialect::Sqlite, s), 4);
        }
        assert_eq!(count(Dialect::Postgres, Script::SelectStar), 3);
        assert_eq!(count(Dialect::MySql, Script::SelectStar), 2);
        assert_eq!(count(Dialect::Sqlite, Script::SelectStar), 2);
        for d in Dialect::ALL {
            assert_eq!(count(d, Script::InsertTemplate), 1);
            assert!(supports(d, ObjectKind::Table, Script::InsertTemplate));
        }
        assert!(!supports(Dialect::MySql, ObjectKind::Role, Script::Create));
        assert!(!supports(Dialect::Sqlite, ObjectKind::Schema, Script::Drop));
    }

    #[test]
    fn unsupported_everywhere_it_should_be() {
        for d in Dialect::ALL {
            for k in ObjectKind::ALL {
                let o = obj(k, "n");
                let t = Table::new(o.clone());
                assert_eq!(
                    create_script(d, &o, Some("CREATE X")).is_ok(),
                    supports(d, k, Script::Create),
                    "{d:?} {k:?}"
                );
                assert_eq!(
                    select_star(d, &o, 1).is_ok(),
                    supports(d, k, Script::SelectStar)
                );
                assert_eq!(
                    insert_template(d, &t).is_ok(),
                    supports(d, k, Script::InsertTemplate)
                );
                if !supports(d, k, Script::Drop) {
                    assert_eq!(
                        drop_script(d, &o),
                        Err(DdlError::Unsupported {
                            dialect: d,
                            kind: k,
                            script: Script::Drop
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn create_table_rejects_non_tables() {
        let t = Table::new(obj(ObjectKind::View, "v"));
        assert_eq!(
            create_table(Dialect::Postgres, &t, &[]),
            Err(DdlError::Unsupported {
                dialect: Dialect::Postgres,
                kind: ObjectKind::View,
                script: Script::Create
            })
        );
    }

    #[test]
    fn create_table_shape() {
        let t = Table::new(obj(ObjectKind::Table, "t"))
            .with_columns(vec![
                ColumnInfo::new("id", DataType::Integer).with_nullable(false),
                ColumnInfo::new("x", DataType::Other),
                ColumnInfo::new("y", DataType::Text).with_native_type("  "),
            ])
            .with_primary_key(PrimaryKey::new(vec!["id".into()]))
            .with_foreign_keys(vec![ForeignKey::new(vec!["x".into()], "p", vec![])]);
        let idx = vec!["CREATE INDEX i ON t (x);;  \n".to_owned(), "  ".to_owned()];
        assert_eq!(
            create_table(Dialect::Sqlite, &t, &idx).unwrap(),
            "CREATE TABLE \"s\".\"t\" (\n    \"id\" INTEGER NOT NULL,\n    \"x\",\n    \"y\" TEXT,\n    PRIMARY KEY (\"id\"),\n    FOREIGN KEY (\"x\") REFERENCES \"p\"\n);\n\nCREATE INDEX i ON t (x);\n"
        );
        assert!(
            create_table(Dialect::MySql, &t, &[])
                .unwrap()
                .contains("`x` LONGTEXT")
        );
    }

    #[test]
    fn invalid_column_name_is_rejected() {
        let t = Table::new(obj(ObjectKind::Table, "t"))
            .with_columns(vec![ColumnInfo::new("a\0", DataType::Text)]);
        for d in Dialect::ALL {
            // T-SQL has no create script: the kind check fails before the name is looked at.
            if d == Dialect::TSql {
                assert!(matches!(
                    create_table(d, &t, &[]),
                    Err(DdlError::Unsupported { .. })
                ));
            } else {
                assert!(matches!(
                    create_table(d, &t, &[]),
                    Err(DdlError::InvalidIdentifier { .. })
                ));
            }
            assert!(matches!(
                insert_template(d, &t),
                Err(DdlError::InvalidIdentifier { .. })
            ));
        }
    }

    #[test]
    fn tsql_scripts() {
        let d = Dialect::TSql;
        for kind in ObjectKind::ALL {
            assert!(!supports(d, kind, Script::Create), "{kind:?}");
            assert!(!supports(d, kind, Script::Drop), "{kind:?}");
        }
        assert!(supports(d, ObjectKind::Table, Script::SelectStar));
        assert!(supports(d, ObjectKind::View, Script::SelectStar));
        assert!(!supports(
            d,
            ObjectKind::MaterializedView,
            Script::SelectStar
        ));
        assert!(supports(d, ObjectKind::Table, Script::InsertTemplate));
        let t = obj(ObjectKind::Table, "t]x");
        assert_eq!(
            select_star(d, &t, 100).unwrap(),
            "SELECT TOP (100) *\nFROM [s].[t]]x];\n"
        );
        assert!(matches!(
            drop_script(d, &t),
            Err(DdlError::Unsupported { .. })
        ));
        assert!(matches!(
            create_script(d, &t, Some("create table t (a int)")),
            Err(DdlError::Unsupported { .. })
        ));
        let table = Table::new(t.clone()).with_columns(vec![
            ColumnInfo::new("a", DataType::Integer),
            ColumnInfo::new("b]", DataType::Text),
        ]);
        assert_eq!(
            insert_template(d, &table).unwrap(),
            "INSERT INTO [s].[t]]x] ([a], [b]]])\nVALUES (@P1, @P2);\n"
        );
        let empty = Table::new(t);
        assert_eq!(
            insert_template(d, &empty).unwrap(),
            "INSERT INTO [s].[t]]x] DEFAULT VALUES;\n"
        );
    }

    #[test]
    fn create_script_rules() {
        let d = Dialect::Postgres;
        let v = obj(ObjectKind::View, "v");
        assert_eq!(
            create_script(d, &v, Some(" SELECT 1 ;\n")).unwrap(),
            "CREATE VIEW \"s\".\"v\" AS\nSELECT 1;\n"
        );
        assert_eq!(
            create_script(d, &v, Some("create view v as select 1;")).unwrap(),
            "create view v as select 1;\n"
        );
        let m = obj(ObjectKind::MaterializedView, "m");
        assert_eq!(
            create_script(d, &m, Some("SELECT 1")).unwrap(),
            "CREATE MATERIALIZED VIEW \"s\".\"m\" AS\nSELECT 1;\n"
        );
        let f = obj(ObjectKind::Function, "f");
        assert_eq!(
            create_script(d, &f, None),
            Err(DdlError::MissingDefinition {
                kind: ObjectKind::Function
            })
        );
        assert_eq!(
            create_script(d, &f, Some(" ; ")),
            Err(DdlError::MissingDefinition {
                kind: ObjectKind::Function
            })
        );
        let schema = obj(ObjectKind::Schema, "a b");
        assert_eq!(
            create_script(d, &schema, None).unwrap(),
            "CREATE SCHEMA \"a b\";\n"
        );
        assert_eq!(
            create_script(Dialect::MySql, &schema, Some("")).unwrap(),
            "CREATE DATABASE `a b`;\n"
        );
        assert_eq!(
            create_script(d, &obj(ObjectKind::Role, "r"), None).unwrap(),
            "CREATE ROLE \"r\";\n"
        );
        assert_eq!(
            create_script(d, &obj(ObjectKind::Extension, "e"), None).unwrap(),
            "CREATE EXTENSION \"e\";\n"
        );
    }

    #[test]
    fn insert_templates() {
        let o = obj(ObjectKind::Table, "t");
        let t = Table::new(o).with_columns(vec![
            ColumnInfo::new("a", DataType::Text),
            ColumnInfo::new("b", DataType::Text),
        ]);
        assert_eq!(
            insert_template(Dialect::Postgres, &t).unwrap(),
            "INSERT INTO \"s\".\"t\" (\"a\", \"b\")\nVALUES ($1, $2);\n"
        );
        assert_eq!(
            insert_template(Dialect::MySql, &t).unwrap(),
            "INSERT INTO `s`.`t` (`a`, `b`)\nVALUES (?, ?);\n"
        );
        let empty = Table::new(obj(ObjectKind::Table, "t"));
        assert_eq!(
            insert_template(Dialect::Sqlite, &empty).unwrap(),
            "INSERT INTO \"s\".\"t\" DEFAULT VALUES;\n"
        );
        assert_eq!(
            insert_template(Dialect::MySql, &empty).unwrap(),
            "INSERT INTO `s`.`t` () VALUES ();\n"
        );
    }

    #[test]
    fn select_star_limit() {
        assert_eq!(
            select_star(
                Dialect::Sqlite,
                &obj(ObjectKind::View, "v"),
                DEFAULT_ROW_LIMIT
            )
            .unwrap(),
            "SELECT *\nFROM \"s\".\"v\"\nLIMIT 1000;\n"
        );
    }

    #[test]
    fn drop_forms() {
        let trig = DbObject::new(
            ObjectKind::Trigger,
            ObjectPath::root().in_schema("s").in_table("t"),
            "tr",
        );
        assert_eq!(
            drop_script(Dialect::Postgres, &trig).unwrap(),
            "DROP TRIGGER \"tr\" ON \"s\".\"t\";\n"
        );
        assert_eq!(
            drop_script(Dialect::Sqlite, &trig).unwrap(),
            "DROP TRIGGER \"s\".\"tr\";\n"
        );
        let idx = DbObject::new(
            ObjectKind::Index,
            ObjectPath::root().in_database("d").in_table("t"),
            "ix",
        );
        assert_eq!(
            drop_script(Dialect::MySql, &idx).unwrap(),
            "DROP INDEX `ix` ON `d`.`t`;\n"
        );
        assert_eq!(
            drop_script(Dialect::Postgres, &idx).unwrap(),
            "DROP INDEX \"ix\";\n"
        );
        for (d, k) in [
            (Dialect::Postgres, ObjectKind::Trigger),
            (Dialect::MySql, ObjectKind::Index),
        ] {
            assert_eq!(
                drop_script(d, &obj(k, "n")),
                Err(DdlError::MissingTable { kind: k })
            );
        }
        assert_eq!(
            drop_script(Dialect::MySql, &obj(ObjectKind::Schema, "x")).unwrap(),
            "DROP DATABASE `x`;\n"
        );
        assert_eq!(
            drop_script(Dialect::Postgres, &obj(ObjectKind::MaterializedView, "m")).unwrap(),
            "DROP MATERIALIZED VIEW \"s\".\"m\";\n"
        );
    }

    #[test]
    fn fk_actions() {
        let fk = ForeignKey::new(vec!["a".into()], "p", vec!["id".into()])
            .with_name("f")
            .with_referenced_schema("r")
            .with_on_update(ReferentialAction::SetNull)
            .with_on_delete(ReferentialAction::SetDefault);
        assert_eq!(
            foreign_key_definition(Dialect::Postgres, &fk).unwrap(),
            "CONSTRAINT \"f\" FOREIGN KEY (\"a\") REFERENCES \"r\".\"p\" (\"id\") ON UPDATE SET NULL ON DELETE SET DEFAULT"
        );
        assert_eq!(
            foreign_key_definition(Dialect::Sqlite, &fk).unwrap(),
            "CONSTRAINT \"f\" FOREIGN KEY (\"a\") REFERENCES \"p\" (\"id\") ON UPDATE SET NULL ON DELETE SET DEFAULT"
        );
        assert_eq!(
            foreign_key_definition(Dialect::MySql, &fk).unwrap(),
            "CONSTRAINT `f` FOREIGN KEY (`a`) REFERENCES `r`.`p` (`id`) ON UPDATE SET NULL ON DELETE SET DEFAULT"
        );
        assert_eq!(action_text(ReferentialAction::Restrict), Some("RESTRICT"));
        assert_eq!(action_text(ReferentialAction::NoAction), None);
    }

    #[test]
    fn table_without_columns() {
        let t = Table::new(obj(ObjectKind::Table, "t"));
        assert_eq!(
            create_table(Dialect::Postgres, &t, &[]).unwrap(),
            "CREATE TABLE \"s\".\"t\" ();\n"
        );
        for d in [Dialect::MySql, Dialect::Sqlite] {
            assert_eq!(
                create_table(d, &t, &[]),
                Err(DdlError::EmptyTable { dialect: d })
            );
        }
    }
}
