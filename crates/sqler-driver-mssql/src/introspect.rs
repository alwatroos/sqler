// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Introspect` for SQL Server, from the `sys.*` catalog views.
//!
//! A session is bound to one database, so only that database can be browsed: a path naming
//! another database is `DriverError::Unsupported`. Materialised views and extensions do not exist
//! in SQL Server.

use sqler_driver_api::sqler_core::{
    ColumnInfo, DataType, DbObject, ForeignKey, Listing, ObjectKind, ObjectPath, PrimaryKey, Query,
    ReferentialAction, Table, Value,
};
use sqler_driver_api::{BoxFuture, DriverError, Introspect};

use crate::session::MssqlSession;

/// Object types of `sys.objects` that are functions.
const FUNCTION_TYPES: &str = "('FN', 'IF', 'TF', 'FS', 'FT')";
/// Object types of `sys.objects` that are procedures.
const PROCEDURE_TYPES: &str = "('P', 'PC', 'RF', 'X')";

fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn opt_text(v: &Value) -> Option<String> {
    (!v.is_null()).then(|| text(v))
}

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        _ => 0,
    }
}

fn t(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// The `list` query of a schema-level kind. It binds `@P1` (schema) and, for index and trigger,
/// `@P2` (table, may be null) and selects `(name, table)`.
fn schema_level_sql(kind: ObjectKind) -> Option<String> {
    let objects = |catalog: &str| {
        format!(
            "select o.name, null from sys.{catalog} o \
             join sys.schemas s on s.schema_id = o.schema_id where s.name = @P1 order by o.name"
        )
    };
    let by_type = |types: &str| {
        format!(
            "select o.name, null from sys.objects o \
             join sys.schemas s on s.schema_id = o.schema_id \
             where s.name = @P1 and o.type in {types} order by o.name"
        )
    };
    Some(match kind {
        ObjectKind::Table => objects("tables"),
        ObjectKind::View => objects("views"),
        ObjectKind::Sequence => objects("sequences"),
        ObjectKind::Function => by_type(FUNCTION_TYPES),
        ObjectKind::Procedure => by_type(PROCEDURE_TYPES),
        ObjectKind::Type => "select ty.name, null from sys.types ty \
             join sys.schemas s on s.schema_id = ty.schema_id \
             where s.name = @P1 and ty.is_user_defined = 1 order by ty.name"
            .to_owned(),
        ObjectKind::Index => "select i.name, o.name from sys.indexes i \
             join sys.objects o on o.object_id = i.object_id \
             join sys.schemas s on s.schema_id = o.schema_id \
             where s.name = @P1 and (@P2 is null or o.name = @P2) \
             and o.is_ms_shipped = 0 and o.type in ('U', 'V') and i.name is not null \
             order by i.name, o.name"
            .to_owned(),
        ObjectKind::Trigger => "select g.name, p.name from sys.triggers g \
             join sys.objects p on p.object_id = g.parent_id \
             join sys.schemas s on s.schema_id = p.schema_id \
             where g.parent_class = 1 and s.name = @P1 and (@P2 is null or p.name = @P2) \
             order by g.name, p.name"
            .to_owned(),
        _ => return None,
    })
}

fn action(code: i64) -> ReferentialAction {
    match code {
        1 => ReferentialAction::Cascade,
        2 => ReferentialAction::SetNull,
        3 => ReferentialAction::SetDefault,
        _ => ReferentialAction::NoAction,
    }
}

/// The logical type of a base type name of `sys.types`.
pub(crate) fn data_type(base: &str) -> DataType {
    match base {
        "bit" => DataType::Boolean,
        "tinyint" | "smallint" | "int" | "bigint" => DataType::Integer,
        "real" | "float" | "money" | "smallmoney" => DataType::Float,
        "decimal" | "numeric" => DataType::Decimal,
        "char" | "varchar" | "text" | "nchar" | "nvarchar" | "ntext" | "xml" | "sysname"
        | "uniqueidentifier" => DataType::Text,
        "binary" | "varbinary" | "image" | "timestamp" | "rowversion" => DataType::Binary,
        "date" => DataType::Date,
        "time" => DataType::Time,
        "datetime" | "datetime2" | "smalldatetime" => DataType::Timestamp,
        "datetimeoffset" => DataType::TimestampTz,
        _ => DataType::Other,
    }
}

/// The type as written in a column definition, e.g. `nvarchar(40)` or `decimal(10,2)`.
pub(crate) fn native_type(name: &str, max_length: i64, precision: i64, scale: i64) -> String {
    match name {
        "char" | "varchar" | "binary" | "varbinary" | "nchar" | "nvarchar" if max_length < 0 => {
            format!("{name}(max)")
        }
        "char" | "varchar" | "binary" | "varbinary" => format!("{name}({max_length})"),
        "nchar" | "nvarchar" => format!("{name}({})", max_length / 2),
        "decimal" | "numeric" => format!("{name}({precision},{scale})"),
        "time" | "datetime2" | "datetimeoffset" => format!("{name}({scale})"),
        _ => name.to_owned(),
    }
}

impl MssqlSession {
    async fn rows(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Vec<Value>>, DriverError> {
        let rs = self.fetch(&Query::new(sql).with_params(params)).await?;
        Ok(rs.rows().iter().map(|r| r.0.clone()).collect())
    }

    async fn scalar(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Option<Value>, DriverError> {
        Ok(self
            .rows(sql, params)
            .await?
            .into_iter()
            .next()
            .and_then(|row| row.into_iter().next()))
    }

    /// The session's database, or `Unsupported` if `wanted` names another one.
    async fn check_database(&mut self, wanted: Option<&str>) -> Result<String, DriverError> {
        let current = self
            .scalar("select db_name()", Vec::new())
            .await?
            .map(|v| text(&v))
            .unwrap_or_default();
        match wanted {
            Some(other) if other != current => Err(DriverError::Unsupported(format!(
                "connected to database {current}; reconnect to browse {other}"
            ))),
            _ => Ok(current),
        }
    }

    async fn schema_or_default(&mut self, schema: Option<&str>) -> Result<String, DriverError> {
        if let Some(schema) = schema {
            return Ok(schema.to_owned());
        }
        Ok(self
            .scalar("select schema_name()", Vec::new())
            .await?
            .and_then(|v| opt_text(&v))
            .unwrap_or_else(|| "dbo".to_owned()))
    }

    async fn names(&mut self, sql: &str) -> Result<Vec<String>, DriverError> {
        Ok(self
            .rows(sql, Vec::new())
            .await?
            .iter()
            .map(|row| text(&row[0]))
            .collect())
    }

    async fn list_objects(
        &mut self,
        parent: &ObjectPath,
        kind: ObjectKind,
    ) -> Result<Listing, DriverError> {
        let root = ObjectPath::root();
        let objects = match kind {
            ObjectKind::Database => self
                .names(
                    "select name from sys.databases \
                     where state = 0 and has_dbaccess(name) = 1 order by name",
                )
                .await?
                .into_iter()
                .map(|name| DbObject::new(kind, root.clone(), name))
                .collect(),
            ObjectKind::Role => self
                .names(
                    "select name from sys.server_principals \
                     where type in ('S', 'U', 'G', 'R') and name not like '##%' order by name",
                )
                .await?
                .into_iter()
                .map(|name| DbObject::new(kind, root.clone(), name))
                .collect(),
            ObjectKind::Schema => {
                let db = self.check_database(parent.database.as_deref()).await?;
                self.names(
                    "select name from sys.schemas \
                     order by case when name in ('sys', 'INFORMATION_SCHEMA', 'guest') \
                     or name like 'db[_]%' then 1 else 0 end, name",
                )
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
                let schema = self.schema_or_default(parent.schema.as_deref()).await?;
                let narrow = matches!(kind, ObjectKind::Index | ObjectKind::Trigger);
                let mut params = vec![t(&schema)];
                if narrow {
                    params.push(parent.table.as_deref().map_or(Value::Null, t));
                }
                let mut objects = Vec::new();
                for row in self.rows(&sql, params).await? {
                    let mut path = root.clone().in_database(&db).in_schema(&schema);
                    if narrow && let Some(table) = opt_text(&row[1]) {
                        path = path.in_table(table);
                    }
                    objects.push(DbObject::new(kind, path, text(&row[0])));
                }
                objects
            }
        };
        Ok(Listing::Objects(objects))
    }

    async fn load_columns(&mut self, object_id: i64) -> Result<Vec<ColumnInfo>, DriverError> {
        let rows = self
            .rows(
                "select c.name, ty.name, coalesce(base.name, ty.name), ty.is_user_defined, c.max_length, \
                 c.precision, c.scale, c.is_nullable, dc.definition \
                 from sys.columns c \
                 join sys.types ty on ty.user_type_id = c.user_type_id \
                 left join sys.types base on base.user_type_id = ty.system_type_id \
                 and base.system_type_id = base.user_type_id \
                 left join sys.default_constraints dc on dc.object_id = c.default_object_id \
                 where c.object_id = @P1 order by c.column_id",
                vec![Value::Int(object_id)],
            )
            .await?;
        let mut columns = Vec::with_capacity(rows.len());
        for row in rows {
            let base = text(&row[2]);
            let native = if matches!(row[3], Value::Bool(true)) {
                text(&row[1])
            } else {
                native_type(&base, int(&row[4]), int(&row[5]), int(&row[6]))
            };
            let mut info = ColumnInfo::new(text(&row[0]), data_type(&base))
                .with_native_type(native)
                .with_nullable(!matches!(row[7], Value::Bool(false)));
            if let Some(default) = opt_text(&row[8]) {
                info = info.with_default(default);
            }
            columns.push(info);
        }
        Ok(columns)
    }

    async fn load_keys(
        &mut self,
        object_id: i64,
    ) -> Result<(Option<PrimaryKey>, Vec<ForeignKey>), DriverError> {
        let pk_rows = self
            .rows(
                "select kc.name, c.name from sys.key_constraints kc \
                 join sys.index_columns ic on ic.object_id = kc.parent_object_id \
                 and ic.index_id = kc.unique_index_id \
                 join sys.columns c on c.object_id = ic.object_id and c.column_id = ic.column_id \
                 where kc.parent_object_id = @P1 and kc.type = 'PK' order by ic.key_ordinal",
                vec![Value::Int(object_id)],
            )
            .await?;
        let primary_key = pk_rows.first().map(|first| {
            PrimaryKey::new(pk_rows.iter().map(|r| text(&r[1])).collect())
                .with_name(text(&first[0]))
        });

        let fk_rows = self
            .rows(
                "select fk.name, pc.name, rs.name, rt.name, rc.name, \
                 fk.update_referential_action, fk.delete_referential_action \
                 from sys.foreign_keys fk \
                 join sys.foreign_key_columns fkc on fkc.constraint_object_id = fk.object_id \
                 join sys.columns pc on pc.object_id = fkc.parent_object_id \
                 and pc.column_id = fkc.parent_column_id \
                 join sys.tables rt on rt.object_id = fk.referenced_object_id \
                 join sys.schemas rs on rs.schema_id = rt.schema_id \
                 join sys.columns rc on rc.object_id = fkc.referenced_object_id \
                 and rc.column_id = fkc.referenced_column_id \
                 where fk.parent_object_id = @P1 \
                 order by fk.name, fkc.constraint_column_id",
                vec![Value::Int(object_id)],
            )
            .await?;
        let mut keys: Vec<ForeignKey> = Vec::new();
        for row in fk_rows {
            let (name, column, referenced) = (text(&row[0]), text(&row[1]), text(&row[4]));
            if let Some(key) = keys
                .last_mut()
                .filter(|key| key.name.as_deref() == Some(name.as_str()))
            {
                key.columns.push(column);
                key.referenced_columns.push(referenced);
                continue;
            }
            keys.push(
                ForeignKey::new(vec![column], text(&row[3]), vec![referenced])
                    .with_name(name)
                    .with_referenced_schema(text(&row[2]))
                    .with_on_update(action(int(&row[5])))
                    .with_on_delete(action(int(&row[6]))),
            );
        }
        Ok((primary_key, keys))
    }

    async fn load_table(&mut self, object: &DbObject) -> Result<Option<Table>, DriverError> {
        let object_type = match object.kind {
            ObjectKind::Table => "U",
            ObjectKind::View => "V",
            _ => return Ok(None),
        };
        self.check_database(object.parent.database.as_deref())
            .await?;
        let schema = self
            .schema_or_default(object.parent.schema.as_deref())
            .await?;
        let found = self
            .scalar(
                "select o.object_id from sys.objects o \
                 join sys.schemas s on s.schema_id = o.schema_id \
                 where s.name = @P1 and o.name = @P2 and o.type = @P3",
                vec![t(&schema), t(&object.name), t(object_type)],
            )
            .await?;
        let Some(Value::Int(object_id)) = found else {
            return Ok(None);
        };
        let columns = self.load_columns(object_id).await?;
        let (primary_key, keys) = self.load_keys(object_id).await?;
        let mut table = Table::new(object.clone())
            .with_columns(columns)
            .with_foreign_keys(keys);
        if let Some(pk) = primary_key {
            table = table.with_primary_key(pk);
        }
        Ok(Some(table))
    }

    async fn load_definition(&mut self, object: &DbObject) -> Result<Option<String>, DriverError> {
        let (sql, narrow) = match object.kind {
            ObjectKind::View => (
                "select object_definition(o.object_id) from sys.objects o \
                 join sys.schemas s on s.schema_id = o.schema_id \
                 where s.name = @P1 and o.name = @P2 and o.type = 'V'"
                    .to_owned(),
                false,
            ),
            ObjectKind::Function | ObjectKind::Procedure => {
                let types = if object.kind == ObjectKind::Function {
                    FUNCTION_TYPES
                } else {
                    PROCEDURE_TYPES
                };
                (
                    format!(
                        "select object_definition(o.object_id) from sys.objects o \
                         join sys.schemas s on s.schema_id = o.schema_id \
                         where s.name = @P1 and o.name = @P2 and o.type in {types}"
                    ),
                    false,
                )
            }
            ObjectKind::Trigger => (
                "select object_definition(g.object_id) from sys.triggers g \
                 join sys.objects p on p.object_id = g.parent_id \
                 join sys.schemas s on s.schema_id = p.schema_id \
                 where g.parent_class = 1 and s.name = @P1 and g.name = @P2 \
                 and (@P3 is null or p.name = @P3) order by p.name"
                    .to_owned(),
                true,
            ),
            _ => return Ok(None),
        };
        self.check_database(object.parent.database.as_deref())
            .await?;
        let schema = self
            .schema_or_default(object.parent.schema.as_deref())
            .await?;
        let mut params = vec![t(&schema), t(&object.name)];
        if narrow {
            params.push(object.parent.table.as_deref().map_or(Value::Null, t));
        }
        Ok(self.scalar(&sql, params).await?.and_then(|v| opt_text(&v)))
    }
}

impl Introspect for MssqlSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        !matches!(kind, ObjectKind::MaterializedView | ObjectKind::Extension)
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
mod tests {
    use super::*;

    #[test]
    fn data_types() {
        let cases = [
            ("bit", DataType::Boolean),
            ("bigint", DataType::Integer),
            ("money", DataType::Float),
            ("numeric", DataType::Decimal),
            ("nvarchar", DataType::Text),
            ("uniqueidentifier", DataType::Text),
            ("varbinary", DataType::Binary),
            ("date", DataType::Date),
            ("time", DataType::Time),
            ("datetime2", DataType::Timestamp),
            ("datetimeoffset", DataType::TimestampTz),
            ("geography", DataType::Other),
        ];
        for (name, expected) in cases {
            assert_eq!(data_type(name), expected, "{name}");
        }
    }

    #[test]
    fn native_types() {
        assert_eq!(native_type("nvarchar", 80, 0, 0), "nvarchar(40)");
        assert_eq!(native_type("nvarchar", -1, 0, 0), "nvarchar(max)");
        assert_eq!(native_type("varchar", 20, 0, 0), "varchar(20)");
        assert_eq!(native_type("varbinary", -1, 0, 0), "varbinary(max)");
        assert_eq!(native_type("decimal", 9, 10, 2), "decimal(10,2)");
        assert_eq!(native_type("datetime2", 8, 27, 7), "datetime2(7)");
        assert_eq!(native_type("int", 4, 10, 0), "int");
    }

    #[test]
    fn referential_actions() {
        assert_eq!(action(0), ReferentialAction::NoAction);
        assert_eq!(action(1), ReferentialAction::Cascade);
        assert_eq!(action(2), ReferentialAction::SetNull);
        assert_eq!(action(3), ReferentialAction::SetDefault);
    }

    #[test]
    fn list_queries_exist_for_schema_level_kinds() {
        for kind in [
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Sequence,
            ObjectKind::Type,
            ObjectKind::Index,
        ] {
            assert!(schema_level_sql(kind).is_some(), "{kind:?}");
        }
        assert!(schema_level_sql(ObjectKind::MaterializedView).is_none());
    }
}
