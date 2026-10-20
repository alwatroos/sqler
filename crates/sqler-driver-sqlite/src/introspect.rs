// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Introspect` for `SQLite`, from `sqlite_master` and the `pragma_*` table-valued functions.
//!
//! `SQLite` has no database level: attached databases are its schemas (`main`, `temp`, ...).

use sqler_driver_api::sqler_core::{
    ColumnInfo, DbObject, ForeignKey, Listing, ObjectKind, ObjectPath, PrimaryKey,
    ReferentialAction, Table,
};
use sqler_driver_api::{BoxFuture, DriverError, Introspect};
use sqlx::{AssertSqlSafe, Row as _};

use crate::convert::{declared_type, query_error};
use crate::session::SqliteSession;

const DEFAULT_SCHEMA: &str = "main";

fn schema_of(path: &ObjectPath) -> &str {
    path.schema.as_deref().unwrap_or(DEFAULT_SCHEMA)
}

/// Quotes `name` as an SQL identifier.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The `sqlite_master.type` value of a supported kind other than `Schema`.
fn master_type(kind: ObjectKind) -> Option<&'static str> {
    match kind {
        ObjectKind::Table => Some("table"),
        ObjectKind::View => Some("view"),
        ObjectKind::Index => Some("index"),
        ObjectKind::Trigger => Some("trigger"),
        _ => None,
    }
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

impl SqliteSession {
    async fn list_objects(
        &mut self,
        parent: &ObjectPath,
        kind: ObjectKind,
    ) -> Result<Listing, DriverError> {
        if kind == ObjectKind::Schema {
            let rows = sqlx::query("select name from pragma_database_list order by seq")
                .fetch_all(&mut self.conn)
                .await
                .map_err(query_error)?;
            let mut objects = Vec::with_capacity(rows.len());
            for row in rows {
                let name: String = row.try_get(0).map_err(query_error)?;
                objects.push(DbObject::new(ObjectKind::Schema, ObjectPath::root(), name));
            }
            return Ok(Listing::Objects(objects));
        }
        let Some(ty) = master_type(kind) else {
            return Ok(Listing::Unsupported);
        };
        let schema = schema_of(parent);
        let narrowed =
            parent.table.is_some() && matches!(kind, ObjectKind::Index | ObjectKind::Trigger);
        let text = format!(
            "select name, tbl_name from {}.sqlite_master where type = ?1 {}\
             and name not like 'sqlite\\_%' escape '\\' order by name",
            quote(schema),
            if narrowed { "and tbl_name = ?2 " } else { "" },
        );
        let mut query = sqlx::query(AssertSqlSafe(text)).bind(ty);
        if narrowed {
            query = query.bind(parent.table.clone().unwrap_or_default());
        }
        let rows = query.fetch_all(&mut self.conn).await.map_err(query_error)?;
        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row.try_get(0).map_err(query_error)?;
            let tbl: String = row.try_get(1).map_err(query_error)?;
            let mut path = ObjectPath::root().in_schema(schema);
            if matches!(kind, ObjectKind::Index | ObjectKind::Trigger) {
                path = path.in_table(tbl);
            }
            objects.push(DbObject::new(kind, path, name));
        }
        Ok(Listing::Objects(objects))
    }

    async fn load_table(&mut self, object: &DbObject) -> Result<Option<Table>, DriverError> {
        let Some(ty) = matches!(object.kind, ObjectKind::Table | ObjectKind::View)
            .then(|| master_type(object.kind))
            .flatten()
        else {
            return Ok(None);
        };
        let schema = schema_of(&object.parent).to_owned();
        let exists = sqlx::query(AssertSqlSafe(format!(
            "select 1 from {}.sqlite_master where type = ?1 and name = ?2",
            quote(&schema)
        )))
        .bind(ty)
        .bind(object.name.clone())
        .fetch_optional(&mut self.conn)
        .await
        .map_err(query_error)?;
        if exists.is_none() {
            return Ok(None);
        }

        let rows = sqlx::query(
            "select name, type, \"notnull\", dflt_value, pk from pragma_table_info(?1, ?2) \
             order by cid",
        )
        .bind(object.name.clone())
        .bind(schema.clone())
        .fetch_all(&mut self.conn)
        .await
        .map_err(query_error)?;
        let mut columns = Vec::with_capacity(rows.len());
        let mut pk: Vec<(i64, String)> = Vec::new();
        for row in rows {
            let name: String = row.try_get(0).map_err(query_error)?;
            let native: String = row.try_get(1).map_err(query_error)?;
            let notnull: i64 = row.try_get(2).map_err(query_error)?;
            let default: Option<String> = row.try_get(3).map_err(query_error)?;
            let pk_pos: i64 = row.try_get(4).map_err(query_error)?;
            let mut info =
                ColumnInfo::new(name.clone(), declared_type(&native)).with_nullable(notnull == 0);
            if !native.is_empty() {
                info = info.with_native_type(native);
            }
            if let Some(default) = default {
                info = info.with_default(default);
            }
            if pk_pos > 0 {
                pk.push((pk_pos, name));
            }
            columns.push(info);
        }
        pk.sort_by_key(|(pos, _)| *pos);

        let rows = sqlx::query(
            "select id, \"table\", \"from\", \"to\", on_update, on_delete \
             from pragma_foreign_key_list(?1, ?2) order by id, seq",
        )
        .bind(object.name.clone())
        .bind(schema.clone())
        .fetch_all(&mut self.conn)
        .await
        .map_err(query_error)?;
        let mut keys: Vec<(i64, ForeignKey)> = Vec::new();
        for row in rows {
            let id: i64 = row.try_get(0).map_err(query_error)?;
            let table: String = row.try_get(1).map_err(query_error)?;
            let from: String = row.try_get(2).map_err(query_error)?;
            let to: Option<String> = row.try_get(3).map_err(query_error)?;
            let on_update: String = row.try_get(4).map_err(query_error)?;
            let on_delete: String = row.try_get(5).map_err(query_error)?;
            if let Some((last, key)) = keys.last_mut()
                && *last == id
            {
                key.columns.push(from);
                key.referenced_columns.extend(to);
                continue;
            }
            let key = ForeignKey::new(vec![from], table, to.into_iter().collect())
                .with_referenced_schema(schema.clone())
                .with_on_update(action(&on_update))
                .with_on_delete(action(&on_delete));
            keys.push((id, key));
        }

        let mut table = Table::new(object.clone())
            .with_columns(columns)
            .with_foreign_keys(keys.into_iter().map(|(_, key)| key).collect());
        if !pk.is_empty() {
            table = table.with_primary_key(PrimaryKey::new(
                pk.into_iter().map(|(_, name)| name).collect(),
            ));
        }
        Ok(Some(table))
    }

    async fn load_definition(&mut self, object: &DbObject) -> Result<Option<String>, DriverError> {
        let Some(ty) = master_type(object.kind) else {
            return Ok(None);
        };
        let row = sqlx::query(AssertSqlSafe(format!(
            "select sql from {}.sqlite_master where type = ?1 and name = ?2",
            quote(schema_of(&object.parent))
        )))
        .bind(ty)
        .bind(object.name.clone())
        .fetch_optional(&mut self.conn)
        .await
        .map_err(query_error)?;
        match row {
            Some(row) => row.try_get::<Option<String>, _>(0).map_err(query_error),
            None => Ok(None),
        }
    }
}

impl Introspect for SqliteSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(
            kind,
            ObjectKind::Schema
                | ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::Index
                | ObjectKind::Trigger
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
