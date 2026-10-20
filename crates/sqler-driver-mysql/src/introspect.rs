// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Introspect` for `MySQL` and `MariaDB`, from `information_schema` and `SHOW CREATE`.
//!
//! `MySQL` has no database level below the server: its databases are the schemas (ADR-0010),
//! as attached databases are for `SQLite`.

use sqler_driver_api::sqler_core::{
    ColumnInfo, DbObject, ForeignKey, Listing, ObjectKind, ObjectPath, PrimaryKey,
    ReferentialAction, Table,
};
use sqler_driver_api::{BoxFuture, DriverError, Introspect};
use sqlx::mysql::MySqlRow;
use sqlx::{AssertSqlSafe, Row as _};

use crate::convert::{declared_type, mysql_code, query_error};
use crate::session::MySqlSession;

/// `ER_DBACCESS_DENIED_ERROR`.
const ER_DBACCESS_DENIED: u16 = 1044;
/// `ER_TABLEACCESS_DENIED_ERROR`.
const ER_TABLEACCESS_DENIED: u16 = 1142;
/// `ER_SPECIFIC_ACCESS_DENIED_ERROR`.
const ER_SPECIFIC_ACCESS_DENIED: u16 = 1227;
/// `ER_BAD_DB_ERROR`.
const ER_BAD_DB: u16 = 1049;
/// `ER_NO_SUCH_TABLE`.
const ER_NO_SUCH_TABLE: u16 = 1146;
/// `ER_SP_DOES_NOT_EXIST`.
const ER_SP_DOES_NOT_EXIST: u16 = 1305;
/// `ER_TRG_DOES_NOT_EXIST`.
const ER_TRG_DOES_NOT_EXIST: u16 = 1360;

/// Quotes `name` as an SQL identifier with backticks.
fn quote(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// A text cell. `information_schema` columns can be reported as binary, so read the bytes.
fn text(row: &MySqlRow, i: usize) -> Result<String, DriverError> {
    let bytes: Vec<u8> = row.try_get_unchecked(i).map_err(query_error)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A nullable text cell.
fn opt_text(row: &MySqlRow, i: usize) -> Result<Option<String>, DriverError> {
    let bytes: Option<Vec<u8>> = row.try_get_unchecked(i).map_err(query_error)?;
    Ok(bytes.map(|b| String::from_utf8_lossy(&b).into_owned()))
}

fn action(text: &str) -> ReferentialAction {
    match text.to_ascii_uppercase().as_str() {
        "RESTRICT" => ReferentialAction::Restrict,
        "CASCADE" => ReferentialAction::Cascade,
        "SET NULL" => ReferentialAction::SetNull,
        "SET DEFAULT" => ReferentialAction::SetDefault,
        _ => ReferentialAction::NoAction,
    }
}

/// The `SHOW CREATE` keyword and the result column that holds the definition.
fn show_create(kind: ObjectKind) -> Option<(&'static str, &'static str)> {
    match kind {
        ObjectKind::Schema => Some(("DATABASE", "Create Database")),
        ObjectKind::Table => Some(("TABLE", "Create Table")),
        ObjectKind::View => Some(("VIEW", "Create View")),
        ObjectKind::Function => Some(("FUNCTION", "Create Function")),
        ObjectKind::Procedure => Some(("PROCEDURE", "Create Procedure")),
        ObjectKind::Trigger => Some(("TRIGGER", "SQL Original Statement")),
        _ => None,
    }
}

/// Whether the error means the object does not exist (or cannot be seen).
fn is_missing(code: Option<u16>) -> bool {
    matches!(
        code,
        Some(ER_NO_SUCH_TABLE | ER_BAD_DB | ER_SP_DOES_NOT_EXIST | ER_TRG_DOES_NOT_EXIST)
    )
}

impl MySqlSession {
    /// The schema to look in: the path's own, else the session's current database.
    async fn schema_of(&mut self, path: &ObjectPath) -> Result<Option<String>, DriverError> {
        if let Some(schema) = &path.schema {
            return Ok(Some(schema.clone()));
        }
        let row = sqlx::query("select database()")
            .fetch_one(&mut self.conn)
            .await
            .map_err(query_error)?;
        opt_text(&row, 0)
    }

    async fn list_objects(
        &mut self,
        parent: &ObjectPath,
        kind: ObjectKind,
    ) -> Result<Listing, DriverError> {
        match kind {
            ObjectKind::Schema => return self.list_schemas().await,
            ObjectKind::Role => return self.list_roles().await,
            _ => {}
        }
        let Some(schema) = self.schema_of(parent).await? else {
            return Ok(Listing::Objects(Vec::new()));
        };
        let narrowed = parent
            .table
            .as_ref()
            .filter(|_| matches!(kind, ObjectKind::Trigger | ObjectKind::Index));
        let (sql, extra) = match kind {
            ObjectKind::Table => (
                "select TABLE_NAME from information_schema.TABLES \
                 where TABLE_SCHEMA = ? and TABLE_TYPE in ('BASE TABLE', 'SYSTEM VERSIONED') \
                 order by TABLE_NAME",
                None,
            ),
            ObjectKind::View => (
                "select TABLE_NAME from information_schema.TABLES \
                 where TABLE_SCHEMA = ? and TABLE_TYPE = 'VIEW' order by TABLE_NAME",
                None,
            ),
            ObjectKind::Function | ObjectKind::Procedure => (
                "select ROUTINE_NAME from information_schema.ROUTINES \
                 where ROUTINE_SCHEMA = ? and ROUTINE_TYPE = ? order by ROUTINE_NAME",
                Some(if kind == ObjectKind::Function {
                    "FUNCTION"
                } else {
                    "PROCEDURE"
                }),
            ),
            ObjectKind::Trigger => (
                if narrowed.is_some() {
                    "select TRIGGER_NAME, EVENT_OBJECT_TABLE from information_schema.TRIGGERS \
                     where TRIGGER_SCHEMA = ? and EVENT_OBJECT_TABLE = ? order by TRIGGER_NAME"
                } else {
                    "select TRIGGER_NAME, EVENT_OBJECT_TABLE from information_schema.TRIGGERS \
                     where TRIGGER_SCHEMA = ? order by TRIGGER_NAME"
                },
                None,
            ),
            ObjectKind::Index => (
                if narrowed.is_some() {
                    "select distinct INDEX_NAME, TABLE_NAME from information_schema.STATISTICS \
                     where TABLE_SCHEMA = ? and TABLE_NAME = ? order by INDEX_NAME, TABLE_NAME"
                } else {
                    "select distinct INDEX_NAME, TABLE_NAME from information_schema.STATISTICS \
                     where TABLE_SCHEMA = ? order by INDEX_NAME, TABLE_NAME"
                },
                None,
            ),
            _ => return Ok(Listing::Unsupported),
        };
        let mut query = sqlx::query(sql).bind(schema.clone());
        if let Some(extra) = extra {
            query = query.bind(extra);
        }
        if let Some(table) = narrowed {
            query = query.bind(table.clone());
        }
        let rows = query.fetch_all(&mut self.conn).await.map_err(query_error)?;
        let with_table = matches!(kind, ObjectKind::Trigger | ObjectKind::Index);
        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            let name = text(&row, 0)?;
            let mut path = ObjectPath::root().in_schema(schema.clone());
            if with_table {
                path = path.in_table(text(&row, 1)?);
            }
            objects.push(DbObject::new(kind, path, name));
        }
        Ok(Listing::Objects(objects))
    }

    async fn list_schemas(&mut self) -> Result<Listing, DriverError> {
        let rows =
            sqlx::query("select SCHEMA_NAME from information_schema.SCHEMATA order by SCHEMA_NAME")
                .fetch_all(&mut self.conn)
                .await
                .map_err(query_error)?;
        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            objects.push(DbObject::new(
                ObjectKind::Schema,
                ObjectPath::root(),
                text(&row, 0)?,
            ));
        }
        Ok(Listing::Objects(objects))
    }

    /// Accounts as `user@host`; without access to `mysql.user`, the current account only.
    async fn list_roles(&mut self) -> Result<Listing, DriverError> {
        let accounts = sqlx::query("select User, Host from mysql.user order by User, Host")
            .fetch_all(&mut self.conn)
            .await;
        let names = match accounts {
            Ok(rows) => {
                let mut names = Vec::with_capacity(rows.len());
                for row in rows {
                    names.push(format!("{}@{}", text(&row, 0)?, text(&row, 1)?));
                }
                names
            }
            Err(e)
                if matches!(
                    mysql_code(&e),
                    Some(ER_TABLEACCESS_DENIED | ER_DBACCESS_DENIED | ER_SPECIFIC_ACCESS_DENIED)
                ) =>
            {
                let row = sqlx::query("select current_user()")
                    .fetch_one(&mut self.conn)
                    .await
                    .map_err(query_error)?;
                vec![text(&row, 0)?]
            }
            Err(e) => return Err(query_error(e)),
        };
        Ok(Listing::Objects(
            names
                .into_iter()
                .map(|name| DbObject::new(ObjectKind::Role, ObjectPath::root(), name))
                .collect(),
        ))
    }

    async fn load_table(&mut self, object: &DbObject) -> Result<Option<Table>, DriverError> {
        if !matches!(object.kind, ObjectKind::Table | ObjectKind::View) {
            return Ok(None);
        }
        let Some(schema) = self.schema_of(&object.parent).await? else {
            return Ok(None);
        };

        let rows = sqlx::query(
            "select COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT \
             from information_schema.COLUMNS where TABLE_SCHEMA = ? and TABLE_NAME = ? \
             order by ORDINAL_POSITION",
        )
        .bind(schema.clone())
        .bind(object.name.clone())
        .fetch_all(&mut self.conn)
        .await
        .map_err(query_error)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let mut columns = Vec::with_capacity(rows.len());
        for row in &rows {
            let name = text(row, 0)?;
            let data_type = text(row, 1)?;
            let column_type = text(row, 2)?;
            let nullable = text(row, 3)?.eq_ignore_ascii_case("YES");
            let mut info = ColumnInfo::new(name, declared_type(&data_type, &column_type))
                .with_native_type(column_type)
                .with_nullable(nullable);
            if let Some(default) = opt_text(row, 4)? {
                info = info.with_default(default);
            }
            columns.push(info);
        }

        let rows = sqlx::query(
            "select COLUMN_NAME from information_schema.KEY_COLUMN_USAGE \
             where TABLE_SCHEMA = ? and TABLE_NAME = ? and CONSTRAINT_NAME = 'PRIMARY' \
             order by ORDINAL_POSITION",
        )
        .bind(schema.clone())
        .bind(object.name.clone())
        .fetch_all(&mut self.conn)
        .await
        .map_err(query_error)?;
        let mut pk = Vec::with_capacity(rows.len());
        for row in &rows {
            pk.push(text(row, 0)?);
        }

        let rows = sqlx::query(
            "select k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, \
             k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE \
             from information_schema.KEY_COLUMN_USAGE k \
             join information_schema.REFERENTIAL_CONSTRAINTS r \
               on r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA \
              and r.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
              and r.TABLE_NAME = k.TABLE_NAME \
             where k.TABLE_SCHEMA = ? and k.TABLE_NAME = ? and k.REFERENCED_TABLE_NAME is not null \
             order by k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
        )
        .bind(schema)
        .bind(object.name.clone())
        .fetch_all(&mut self.conn)
        .await
        .map_err(query_error)?;
        let mut keys: Vec<ForeignKey> = Vec::new();
        let mut last = String::new();
        for row in &rows {
            let name = text(row, 0)?;
            let column = text(row, 1)?;
            let referenced_column = opt_text(row, 4)?;
            if !keys.is_empty() && name == last {
                if let Some(key) = keys.last_mut() {
                    key.columns.push(column);
                    key.referenced_columns.extend(referenced_column);
                }
                continue;
            }
            let mut key = ForeignKey::new(
                vec![column],
                text(row, 3)?,
                referenced_column.into_iter().collect(),
            )
            .with_name(name.clone())
            .with_on_update(action(&text(row, 5)?))
            .with_on_delete(action(&text(row, 6)?));
            if let Some(schema) = opt_text(row, 2)? {
                key = key.with_referenced_schema(schema);
            }
            keys.push(key);
            last = name;
        }

        let mut table = Table::new(object.clone())
            .with_columns(columns)
            .with_foreign_keys(keys);
        if !pk.is_empty() {
            table = table.with_primary_key(PrimaryKey::new(pk));
        }
        Ok(Some(table))
    }

    async fn load_definition(&mut self, object: &DbObject) -> Result<Option<String>, DriverError> {
        let Some((keyword, column)) = show_create(object.kind) else {
            return Ok(None);
        };
        let target = if object.kind == ObjectKind::Schema {
            quote(&object.name)
        } else {
            let Some(schema) = self.schema_of(&object.parent).await? else {
                return Ok(None);
            };
            format!("{}.{}", quote(&schema), quote(&object.name))
        };
        // The text protocol: not every `SHOW CREATE` can be prepared on every server.
        let result = sqlx::raw_sql(AssertSqlSafe(format!("SHOW CREATE {keyword} {target}")))
            .fetch_all(&mut self.conn)
            .await;
        match result {
            Ok(rows) => match rows.first() {
                Some(row) => {
                    let bytes: Option<Vec<u8>> =
                        row.try_get_unchecked(column).map_err(query_error)?;
                    Ok(bytes.map(|b| String::from_utf8_lossy(&b).into_owned()))
                }
                None => Ok(None),
            },
            Err(e) if is_missing(mysql_code(&e)) => Ok(None),
            Err(e) => Err(query_error(e)),
        }
    }
}

impl Introspect for MySqlSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(
            kind,
            ObjectKind::Schema
                | ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::Function
                | ObjectKind::Procedure
                | ObjectKind::Trigger
                | ObjectKind::Index
                | ObjectKind::Role
        )
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        Box::pin(async move {
            if !self.supports(kind) {
                return Ok(Listing::Unsupported);
            }
            self.list_objects(parent, kind).await
        })
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        Box::pin(self.load_table(table))
    }

    fn definition<'a>(
        &'a mut self,
        object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(self.load_definition(object))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(quote("t"), "`t`");
        assert_eq!(quote("a`b"), "`a``b`");
        assert_eq!(quote("``"), "``````");
    }

    #[test]
    fn show_create_covers_definable_kinds() {
        assert_eq!(
            show_create(ObjectKind::Trigger).unwrap().1,
            "SQL Original Statement"
        );
        assert!(show_create(ObjectKind::Index).is_none());
        assert!(show_create(ObjectKind::Role).is_none());
    }

    #[test]
    fn referential_actions_parse() {
        assert_eq!(action("CASCADE"), ReferentialAction::Cascade);
        assert_eq!(action("set null"), ReferentialAction::SetNull);
        assert_eq!(action("SET DEFAULT"), ReferentialAction::SetDefault);
        assert_eq!(action("RESTRICT"), ReferentialAction::Restrict);
        assert_eq!(action("NO ACTION"), ReferentialAction::NoAction);
    }

    #[test]
    fn missing_object_codes() {
        for code in [1146, 1049, 1305, 1360] {
            assert!(is_missing(Some(code)));
        }
        assert!(!is_missing(Some(1064)));
        assert!(!is_missing(None));
    }
}
