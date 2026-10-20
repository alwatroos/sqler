// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Introspect` for `PostgreSQL`, from `pg_catalog`.
//!
//! A session is bound to one database, so only that database can be browsed: a path naming
//! another database is `DriverError::Unsupported`. Functions and procedures are named
//! `proname(identity arguments)`, which keeps overloads apart.

use sqler_driver_api::sqler_core::{
    ColumnInfo, DbObject, ForeignKey, Listing, ObjectKind, ObjectPath, PrimaryKey,
    ReferentialAction, Table,
};
use sqler_driver_api::{BoxFuture, DriverError, Introspect};
use sqlx::Row as _;
use sqlx::postgres::types::Oid;

use crate::convert::{data_type, query_error};
use crate::session::PgSession;

fn err(e: sqlx::Error) -> DriverError {
    query_error(e, false)
}

/// The `list` query of a schema-level kind. It binds `$1` (schema) and, for index and trigger,
/// `$2` (table, may be null) and selects `(name, table)`.
fn schema_level_sql(kind: ObjectKind) -> Option<&'static str> {
    let relkind = |kinds: &'static str| match kinds {
        "r,p,f" => Some(
            "select c.relname::text, null::text from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = $1 and c.relkind in ('r', 'p', 'f') order by 1",
        ),
        "v" => Some(
            "select c.relname::text, null::text from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = $1 and c.relkind = 'v' order by 1",
        ),
        "m" => Some(
            "select c.relname::text, null::text from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = $1 and c.relkind = 'm' order by 1",
        ),
        "S" => Some(
            "select c.relname::text, null::text from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = $1 and c.relkind = 'S' order by 1",
        ),
        _ => None,
    };
    match kind {
        ObjectKind::Table => relkind("r,p,f"),
        ObjectKind::View => relkind("v"),
        ObjectKind::MaterializedView => relkind("m"),
        ObjectKind::Sequence => relkind("S"),
        ObjectKind::Index => Some(
            "select i.relname::text, t.relname::text from pg_index x \
             join pg_class i on i.oid = x.indexrelid \
             join pg_class t on t.oid = x.indrelid \
             join pg_namespace n on n.oid = i.relnamespace \
             where n.nspname = $1 and ($2::text is null or t.relname = $2) \
             order by 1, 2",
        ),
        ObjectKind::Trigger => Some(
            "select g.tgname::text, t.relname::text from pg_trigger g \
             join pg_class t on t.oid = g.tgrelid \
             join pg_namespace n on n.oid = t.relnamespace \
             where not g.tgisinternal and n.nspname = $1 \
             and ($2::text is null or t.relname = $2) order by 1, 2",
        ),
        ObjectKind::Function => Some(
            "select p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')', \
             null::text from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
             where n.nspname = $1 and p.prokind = 'f' order by 1",
        ),
        ObjectKind::Procedure => Some(
            "select p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')', \
             null::text from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
             where n.nspname = $1 and p.prokind = 'p' order by 1",
        ),
        ObjectKind::Type => Some(
            "select t.typname::text, null::text from pg_type t \
             join pg_namespace n on n.oid = t.typnamespace \
             left join pg_class c on c.oid = t.typrelid \
             where n.nspname = $1 and t.typtype in ('b', 'c', 'd', 'e', 'r') \
             and t.typcategory <> 'A' and (t.typtype <> 'c' or c.relkind = 'c') order by 1",
        ),
        _ => None,
    }
}

fn action(code: &str) -> ReferentialAction {
    match code {
        "r" => ReferentialAction::Restrict,
        "c" => ReferentialAction::Cascade,
        "n" => ReferentialAction::SetNull,
        "d" => ReferentialAction::SetDefault,
        _ => ReferentialAction::NoAction,
    }
}

/// The `relkind` letters of a kind `table()` accepts.
fn table_relkinds(kind: ObjectKind) -> Option<&'static [&'static str]> {
    match kind {
        ObjectKind::Table => Some(&["r", "p", "f"]),
        ObjectKind::View => Some(&["v"]),
        ObjectKind::MaterializedView => Some(&["m"]),
        _ => None,
    }
}

impl PgSession {
    async fn current_database(&mut self) -> Result<String, DriverError> {
        sqlx::query_scalar("select current_database()::text")
            .fetch_one(&mut self.conn)
            .await
            .map_err(err)
    }

    /// The session's database, or `Unsupported` if `wanted` names another one.
    async fn check_database(&mut self, wanted: Option<&str>) -> Result<String, DriverError> {
        let current = self.current_database().await?;
        match wanted {
            Some(other) if other != current => Err(DriverError::Unsupported(format!(
                "connected to database {current}; reconnect to browse {other}"
            ))),
            _ => Ok(current),
        }
    }

    async fn schema_or_current(&mut self, schema: Option<&str>) -> Result<String, DriverError> {
        if let Some(schema) = schema {
            return Ok(schema.to_owned());
        }
        let current: Option<String> = sqlx::query_scalar("select current_schema()::text")
            .fetch_one(&mut self.conn)
            .await
            .map_err(err)?;
        Ok(current.unwrap_or_else(|| "public".to_owned()))
    }

    async fn names(&mut self, sql: &'static str) -> Result<Vec<String>, DriverError> {
        sqlx::query_scalar(sql)
            .fetch_all(&mut self.conn)
            .await
            .map_err(err)
    }

    async fn list_objects(
        &mut self,
        parent: &ObjectPath,
        kind: ObjectKind,
    ) -> Result<Listing, DriverError> {
        let root = ObjectPath::root();
        let objects = match kind {
            ObjectKind::Database => self
                .names("select datname::text from pg_database where not datistemplate order by 1")
                .await?
                .into_iter()
                .map(|name| DbObject::new(kind, root.clone(), name))
                .collect(),
            ObjectKind::Role => self
                .names("select rolname::text from pg_roles where rolname !~ '^pg_' order by 1")
                .await?
                .into_iter()
                .map(|name| DbObject::new(kind, root.clone(), name))
                .collect(),
            ObjectKind::Schema | ObjectKind::Extension => {
                let db = self.check_database(parent.database.as_deref()).await?;
                let sql = if kind == ObjectKind::Schema {
                    "select nspname::text from pg_namespace \
                     where nspname !~ '^pg_toast' and nspname !~ '^pg_temp' \
                     order by (nspname = 'information_schema' or nspname ~ '^pg_'), nspname"
                } else {
                    "select extname::text from pg_extension order by 1"
                };
                self.names(sql)
                    .await?
                    .into_iter()
                    .map(|name| DbObject::new(kind, root.clone().in_database(&db), name))
                    .collect()
            }
            _ => {
                let Some(sql) = schema_level_sql(kind) else {
                    return Ok(Listing::Unsupported);
                };
                let db = self.check_database(parent.database.as_deref()).await?;
                let schema = self.schema_or_current(parent.schema.as_deref()).await?;
                let narrow = matches!(kind, ObjectKind::Index | ObjectKind::Trigger);
                let mut query = sqlx::query(sql).bind(schema.clone());
                if narrow {
                    query = query.bind(parent.table.clone());
                }
                let rows = query.fetch_all(&mut self.conn).await.map_err(err)?;
                let mut objects = Vec::with_capacity(rows.len());
                for row in rows {
                    let name: String = row.try_get(0).map_err(err)?;
                    let table: Option<String> = row.try_get(1).map_err(err)?;
                    let mut path = root.clone().in_database(&db).in_schema(&schema);
                    if narrow && let Some(table) = table {
                        path = path.in_table(table);
                    }
                    objects.push(DbObject::new(kind, path, name));
                }
                objects
            }
        };
        Ok(Listing::Objects(objects))
    }

    async fn load_columns(&mut self, oid: Oid) -> Result<Vec<ColumnInfo>, DriverError> {
        let rows = sqlx::query(
            "select a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull, \
             pg_get_expr(d.adbin, d.adrelid), b.oid::int8, (b.typcategory = 'A') \
             from pg_attribute a \
             join pg_type t on t.oid = a.atttypid \
             join pg_type b on b.oid = case when t.typtype = 'd' then t.typbasetype else t.oid end \
             left join pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum \
             where a.attrelid = $1 and a.attnum > 0 and not a.attisdropped order by a.attnum",
        )
        .bind(oid)
        .fetch_all(&mut self.conn)
        .await
        .map_err(err)?;
        let mut columns = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row.try_get(0).map_err(err)?;
            let native: String = row.try_get(1).map_err(err)?;
            let not_null: bool = row.try_get(2).map_err(err)?;
            let default: Option<String> = row.try_get(3).map_err(err)?;
            let base: i64 = row.try_get(4).map_err(err)?;
            let array: bool = row.try_get(5).map_err(err)?;
            let mut info = ColumnInfo::new(name, data_type(u32::try_from(base).ok(), array))
                .with_native_type(native)
                .with_nullable(!not_null);
            if let Some(default) = default {
                info = info.with_default(default);
            }
            columns.push(info);
        }
        Ok(columns)
    }

    async fn load_keys(
        &mut self,
        oid: Oid,
    ) -> Result<(Option<sqlx::postgres::PgRow>, Vec<ForeignKey>), DriverError> {
        let key_columns = |col: &str, rel: &str, list: &str| {
            format!(
                "array(select a.attname::text from unnest({list}) with ordinality k(attnum, ord) \
                 join pg_attribute a on a.attrelid = {rel} and a.attnum = k.attnum \
                 order by k.ord) as {col}"
            )
        };
        let pk_sql = format!(
            "select con.conname::text, {} from pg_constraint con \
             where con.conrelid = $1 and con.contype = 'p'",
            key_columns("cols", "con.conrelid", "con.conkey")
        );
        let pk_row = sqlx::query(sqlx::AssertSqlSafe(pk_sql))
            .bind(oid)
            .fetch_optional(&mut self.conn)
            .await
            .map_err(err)?;

        let fk_sql = format!(
            "select con.conname::text, {}, fn.nspname::text, fc.relname::text, {}, \
             con.confupdtype::text, con.confdeltype::text \
             from pg_constraint con \
             join pg_class fc on fc.oid = con.confrelid \
             join pg_namespace fn on fn.oid = fc.relnamespace \
             where con.conrelid = $1 and con.contype = 'f' order by con.conname",
            key_columns("cols", "con.conrelid", "con.conkey"),
            key_columns("refcols", "con.confrelid", "con.confkey"),
        );
        let fk_rows = sqlx::query(sqlx::AssertSqlSafe(fk_sql))
            .bind(oid)
            .fetch_all(&mut self.conn)
            .await
            .map_err(err)?;
        let mut keys = Vec::with_capacity(fk_rows.len());
        for row in fk_rows {
            let name: String = row.try_get(0).map_err(err)?;
            let cols: Vec<String> = row.try_get(1).map_err(err)?;
            let ref_schema: String = row.try_get(2).map_err(err)?;
            let ref_table: String = row.try_get(3).map_err(err)?;
            let ref_cols: Vec<String> = row.try_get(4).map_err(err)?;
            let on_update: String = row.try_get(5).map_err(err)?;
            let on_delete: String = row.try_get(6).map_err(err)?;
            keys.push(
                ForeignKey::new(cols, ref_table, ref_cols)
                    .with_name(name)
                    .with_referenced_schema(ref_schema)
                    .with_on_update(action(&on_update))
                    .with_on_delete(action(&on_delete)),
            );
        }
        Ok((pk_row, keys))
    }

    async fn load_table(&mut self, object: &DbObject) -> Result<Option<Table>, DriverError> {
        let Some(relkinds) = table_relkinds(object.kind) else {
            return Ok(None);
        };
        self.check_database(object.parent.database.as_deref())
            .await?;
        let schema = self
            .schema_or_current(object.parent.schema.as_deref())
            .await?;
        let found = sqlx::query(
            "select c.oid from pg_class c join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = $1 and c.relname = $2 and c.relkind::text = any($3)",
        )
        .bind(&schema)
        .bind(&object.name)
        .bind(relkinds.iter().map(|k| (*k).to_owned()).collect::<Vec<_>>())
        .fetch_optional(&mut self.conn)
        .await
        .map_err(err)?;
        let Some(found) = found else {
            return Ok(None);
        };
        let oid: Oid = found.try_get(0).map_err(err)?;

        let columns = self.load_columns(oid).await?;
        let (pk_row, keys) = self.load_keys(oid).await?;

        let mut table = Table::new(object.clone())
            .with_columns(columns)
            .with_foreign_keys(keys);
        if let Some(row) = pk_row {
            let name: String = row.try_get(0).map_err(err)?;
            let cols: Vec<String> = row.try_get(1).map_err(err)?;
            table = table.with_primary_key(PrimaryKey::new(cols).with_name(name));
        }
        Ok(Some(table))
    }

    async fn load_definition(&mut self, object: &DbObject) -> Result<Option<String>, DriverError> {
        let sql = match object.kind {
            ObjectKind::View | ObjectKind::MaterializedView => {
                "select pg_get_viewdef(c.oid, true) from pg_class c \
                 join pg_namespace n on n.oid = c.relnamespace \
                 where n.nspname = $1 and c.relname = $2 and c.relkind::text = $3"
            }
            ObjectKind::Function | ObjectKind::Procedure => {
                "select pg_get_functiondef(p.oid) from pg_proc p \
                 join pg_namespace n on n.oid = p.pronamespace \
                 where n.nspname = $1 \
                 and p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')' = $2 \
                 and p.prokind::text = $3"
            }
            ObjectKind::Trigger => {
                "select pg_get_triggerdef(g.oid, true) from pg_trigger g \
                 join pg_class t on t.oid = g.tgrelid \
                 join pg_namespace n on n.oid = t.relnamespace \
                 where not g.tgisinternal and n.nspname = $1 and g.tgname = $2 \
                 and ($3::text is null or t.relname = $3) order by t.relname limit 1"
            }
            ObjectKind::Index => {
                "select pg_get_indexdef(i.oid) from pg_class i \
                 join pg_namespace n on n.oid = i.relnamespace \
                 where n.nspname = $1 and i.relname = $2 and i.relkind in ('i', 'I')"
            }
            _ => return Ok(None),
        };
        self.check_database(object.parent.database.as_deref())
            .await?;
        let schema = self
            .schema_or_current(object.parent.schema.as_deref())
            .await?;
        let query = sqlx::query(sql).bind(schema).bind(&object.name);
        let query = match object.kind {
            ObjectKind::View => query.bind("v"),
            ObjectKind::MaterializedView => query.bind("m"),
            ObjectKind::Function => query.bind("f"),
            ObjectKind::Procedure => query.bind("p"),
            ObjectKind::Trigger => query.bind(object.parent.table.clone()),
            _ => query,
        };
        let row = query.fetch_optional(&mut self.conn).await.map_err(err)?;
        match row {
            Some(row) => row.try_get::<Option<String>, _>(0).map_err(err),
            None => Ok(None),
        }
    }
}

impl Introspect for PgSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(
            kind,
            ObjectKind::Database
                | ObjectKind::Schema
                | ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
                | ObjectKind::Function
                | ObjectKind::Procedure
                | ObjectKind::Trigger
                | ObjectKind::Sequence
                | ObjectKind::Type
                | ObjectKind::Index
                | ObjectKind::Extension
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
