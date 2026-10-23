// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Introspect` for `DuckDB`, from the `duckdb_*()` catalog functions.
//!
//! Every query binds its parameters. No name is spliced into SQL text.

use sqler_driver_api::sqler_core::{
    ColumnInfo, DataType, DbObject, ForeignKey, Listing, ObjectKind, ObjectPath, PrimaryKey,
    ResultSet, Table, Value,
};
use sqler_driver_api::{BoxFuture, DriverError, Introspect};

use crate::session::DuckSession;

/// Separates the elements of a list column rendered with `array_to_string`.
const SEP: char = '\u{1f}';

/// Matches a catalog row to the database and schema of a path (`$1`, `$2`; `NULL` = current).
const SCOPE: &str = "database_name = coalesce($1::varchar, current_database()) \
                     and schema_name = coalesce($2::varchar, current_schema())";

fn text_param(value: Option<&String>) -> Value {
    value.map_or(Value::Null, |v| Value::Text(v.clone()))
}

fn scope(path: &ObjectPath) -> Vec<Value> {
    vec![
        text_param(path.database.as_ref()),
        text_param(path.schema.as_ref()),
    ]
}

fn cell(rs: &ResultSet, row: usize, col: usize) -> Option<&Value> {
    rs.rows().get(row).and_then(|r| r.get(col))
}

fn text(rs: &ResultSet, row: usize, col: usize) -> String {
    match cell(rs, row, col) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

fn opt_text(rs: &ResultSet, row: usize, col: usize) -> Option<String> {
    match cell(rs, row, col) {
        Some(Value::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

fn split(list: &str) -> Vec<String> {
    if list.is_empty() {
        Vec::new()
    } else {
        list.split(SEP).map(str::to_owned).collect()
    }
}

/// Maps a native type name to a [`DataType`].
fn native_type(name: &str) -> DataType {
    let upper = name.to_ascii_uppercase();
    let base = upper.split('(').next().unwrap_or("").trim();
    match base {
        "BOOLEAN" => DataType::Boolean,
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "UTINYINT" | "USMALLINT" | "UINTEGER"
        | "UBIGINT" => DataType::Integer,
        "FLOAT" | "DOUBLE" => DataType::Float,
        "DECIMAL" | "HUGEINT" | "UHUGEINT" => DataType::Decimal,
        "VARCHAR" => DataType::Text,
        "BLOB" => DataType::Binary,
        "DATE" => DataType::Date,
        "TIME" => DataType::Time,
        "TIMESTAMP" | "TIMESTAMP_S" | "TIMESTAMP_MS" | "TIMESTAMP_NS" => DataType::Timestamp,
        "TIMESTAMP WITH TIME ZONE" => DataType::TimestampTz,
        "JSON" => DataType::Json,
        _ => DataType::Other,
    }
}

/// The catalog function, name column and (for `Table`/`View`) kind label of a supported kind.
fn catalog(kind: ObjectKind) -> Option<(&'static str, &'static str)> {
    match kind {
        ObjectKind::Table => Some(("duckdb_tables()", "table_name")),
        ObjectKind::View => Some(("duckdb_views()", "view_name")),
        ObjectKind::Index => Some(("duckdb_indexes()", "index_name")),
        ObjectKind::Sequence => Some(("duckdb_sequences()", "sequence_name")),
        _ => None,
    }
}

fn is_supported(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Database
            | ObjectKind::Schema
            | ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::Index
            | ObjectKind::Sequence
    )
}

impl DuckSession {
    async fn list_objects(
        &mut self,
        parent: &ObjectPath,
        kind: ObjectKind,
    ) -> Result<Listing, DriverError> {
        match kind {
            ObjectKind::Database => {
                let rs = self
                    .run_all(
                        "select database_name from duckdb_databases() \
                         where not internal order by database_name",
                        Vec::new(),
                    )
                    .await?;
                let objects = (0..rs.len())
                    .map(|i| DbObject::new(kind, ObjectPath::root(), text(&rs, i, 0)))
                    .collect();
                Ok(Listing::Objects(objects))
            }
            ObjectKind::Schema => {
                let rs = self
                    .run_all(
                        "select database_name, schema_name from duckdb_schemas() \
                         where (not internal or schema_name = 'main') \
                         and database_name = coalesce($1::varchar, current_database()) \
                         order by oid",
                        vec![text_param(parent.database.as_ref())],
                    )
                    .await?;
                let objects = (0..rs.len())
                    .map(|i| {
                        let path = ObjectPath::root().in_database(text(&rs, i, 0));
                        DbObject::new(kind, path, text(&rs, i, 1))
                    })
                    .collect();
                Ok(Listing::Objects(objects))
            }
            _ => {
                let Some((function, name_column)) = catalog(kind) else {
                    return Ok(Listing::Unsupported);
                };
                let narrowed = kind == ObjectKind::Index && parent.table.is_some();
                let internal = if matches!(kind, ObjectKind::Table | ObjectKind::View) {
                    "and not internal "
                } else {
                    ""
                };
                let narrow = if narrowed {
                    "and table_name = $3::varchar "
                } else {
                    ""
                };
                let table_column = if kind == ObjectKind::Index {
                    "table_name"
                } else {
                    "null::varchar"
                };
                let sql = format!(
                    "select database_name, schema_name, {name_column}, {table_column} \
                     from {function} where {SCOPE} {internal}{narrow}\
                     order by {name_column}"
                );
                let mut params = scope(parent);
                if narrowed {
                    params.push(text_param(parent.table.as_ref()));
                }
                let rs = self.run_all(&sql, params).await?;
                let objects = (0..rs.len())
                    .map(|i| {
                        let mut path = ObjectPath::root()
                            .in_database(text(&rs, i, 0))
                            .in_schema(text(&rs, i, 1));
                        if let Some(table) = opt_text(&rs, i, 3) {
                            path = path.in_table(table);
                        }
                        DbObject::new(kind, path, text(&rs, i, 2))
                    })
                    .collect();
                Ok(Listing::Objects(objects))
            }
        }
    }

    async fn load_table(&mut self, object: &DbObject) -> Result<Option<Table>, DriverError> {
        let Some((function, name_column)) = catalog(object.kind)
            .filter(|_| matches!(object.kind, ObjectKind::Table | ObjectKind::View))
        else {
            return Ok(None);
        };
        let mut params = scope(&object.parent);
        params.push(Value::Text(object.name.clone()));
        let exists = self
            .run_all(
                &format!("select 1 from {function} where {SCOPE} and {name_column} = $3"),
                params.clone(),
            )
            .await?;
        if exists.is_empty() {
            return Ok(None);
        }
        let rs = self
            .run_all(
                &format!(
                    "select column_name, data_type, is_nullable, column_default \
                     from duckdb_columns() where {SCOPE} and table_name = $3 \
                     order by column_index"
                ),
                params.clone(),
            )
            .await?;
        let columns = (0..rs.len())
            .map(|i| {
                let native = text(&rs, i, 1);
                let nullable = !matches!(cell(&rs, i, 2), Some(Value::Bool(false)));
                let mut info = ColumnInfo::new(text(&rs, i, 0), native_type(&native))
                    .with_native_type(native)
                    .with_nullable(nullable);
                if let Some(default) = opt_text(&rs, i, 3) {
                    info = info.with_default(default);
                }
                info
            })
            .collect();

        let mut table = Table::new(object.clone()).with_columns(columns);
        if object.kind == ObjectKind::Table {
            let pk = self
                .run_all(
                    &format!(
                        "select constraint_name, array_to_string(constraint_column_names, chr(31)) \
                         from duckdb_constraints() \
                         where {SCOPE} and table_name = $3 and constraint_type = 'PRIMARY KEY' \
                         order by constraint_index"
                    ),
                    params.clone(),
                )
                .await?;
            if !pk.is_empty() {
                let mut key = PrimaryKey::new(split(&text(&pk, 0, 1)));
                if let Some(name) = opt_text(&pk, 0, 0) {
                    key = key.with_name(name);
                }
                table = table.with_primary_key(key);
            }
            let fks = self
                .run_all(
                    &format!(
                        "select constraint_name, \
                         array_to_string(constraint_column_names, chr(31)), \
                         referenced_table, \
                         array_to_string(referenced_column_names, chr(31)) \
                         from duckdb_constraints() \
                         where {SCOPE} and table_name = $3 and constraint_type = 'FOREIGN KEY' \
                         order by constraint_index"
                    ),
                    params,
                )
                .await?;
            let keys = (0..fks.len())
                .map(|i| {
                    let mut key = ForeignKey::new(
                        split(&text(&fks, i, 1)),
                        text(&fks, i, 2),
                        split(&text(&fks, i, 3)),
                    );
                    if let Some(name) = opt_text(&fks, i, 0) {
                        key = key.with_name(name);
                    }
                    if let Some(schema) = object.parent.schema.clone() {
                        key = key.with_referenced_schema(schema);
                    }
                    key
                })
                .collect();
            table = table.with_foreign_keys(keys);
        }
        Ok(Some(table))
    }

    async fn load_definition(&mut self, object: &DbObject) -> Result<Option<String>, DriverError> {
        let Some((function, name_column)) = catalog(object.kind) else {
            return Ok(None);
        };
        let mut params = scope(&object.parent);
        params.push(Value::Text(object.name.clone()));
        let rs = self
            .run_all(
                &format!("select sql from {function} where {SCOPE} and {name_column} = $3"),
                params,
            )
            .await?;
        Ok(opt_text(&rs, 0, 0))
    }
}

impl Introspect for DuckSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        is_supported(kind)
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        Box::pin(async move {
            if !is_supported(kind) {
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
    fn native_types() {
        assert_eq!(native_type("DECIMAL(18,4)"), DataType::Decimal);
        assert_eq!(
            native_type("TIMESTAMP WITH TIME ZONE"),
            DataType::TimestampTz
        );
        assert_eq!(native_type("INTEGER[]"), DataType::Other);
        assert_eq!(native_type("BIGINT"), DataType::Integer);
    }

    #[test]
    fn splits_lists() {
        assert_eq!(split(""), Vec::<String>::new());
        assert_eq!(split("a\u{1f}b"), ["a", "b"]);
    }
}
