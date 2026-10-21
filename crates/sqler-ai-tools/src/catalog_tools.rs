// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The six catalog tools. They answer from the catalog cache and load a missing node through
//! the catalog worker; only `describe_function`'s definition text may need a session.

use serde_json::{Value, json};
use sqler_catalog::{NameKind, NameQuery, NodeKey};
use sqler_driver_api::sqler_core::{DbObject, Listing, ObjectKind, ObjectPath, Table};

use crate::error::ToolError;
use crate::schema::{
    DescribeArgs, ListSchemasArgs, ListTablesArgs, SearchSchemaArgs, TableKind, addr_path,
    name_kinds, table_kind_name,
};
use crate::source::ConnectionInfo;
use crate::toolset::Inner;

fn opt(value: Option<&String>) -> Value {
    value.map_or(Value::Null, |v| json!(v))
}

fn environment(info: &ConnectionInfo) -> Value {
    info.environment.map_or(Value::Null, |e| json!(e.as_str()))
}

pub(crate) fn list_connections(ts: &Inner) -> Value {
    let connections: Vec<Value> = ts
        .source
        .connections()
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "driver": c.driver.as_str(),
                "environment": environment(c),
                "read_only": c.read_only,
            })
        })
        .collect();
    json!({"connections": connections})
}

pub(crate) async fn list_schemas(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &ListSchemasArgs,
) -> Result<Value, ToolError> {
    let catalog = ts.catalog(info)?;
    let path = addr_path(args.database.as_deref(), None);
    let listing = catalog
        .children(NodeKey::new(path, ObjectKind::Schema))
        .await?;
    Ok(match &*listing {
        Listing::Objects(objects) => {
            let schemas: Vec<&str> = objects.iter().map(|o| o.name.as_str()).collect();
            json!({"connection": info.id, "supported": true, "schemas": schemas})
        }
        Listing::Unsupported => json!({"connection": info.id, "supported": false, "schemas": []}),
    })
}

fn object_json(obj: &DbObject) -> Value {
    json!({
        "name": obj.name,
        "kind": table_kind_name(obj.kind),
        "database": opt(obj.parent.database.as_ref()),
        "schema": opt(obj.parent.schema.as_ref()),
    })
}

pub(crate) async fn list_tables(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &ListTablesArgs,
) -> Result<Value, ToolError> {
    let catalog = ts.catalog(info)?;
    let path = addr_path(args.database.as_deref(), args.schema.as_deref());
    let kinds = args.kinds.clone().unwrap_or_else(|| {
        vec![
            TableKind::Table,
            TableKind::View,
            TableKind::MaterializedView,
        ]
    });
    let mut tables = Vec::new();
    for kind in kinds {
        let key = NodeKey::new(path.clone(), kind.object_kind());
        if let Listing::Objects(objects) = &*catalog.children(key).await? {
            tables.extend(objects.iter().map(object_json));
        }
    }
    Ok(json!({"connection": info.id, "tables": tables}))
}

fn table_json(table: &Table) -> Value {
    let columns: Vec<Value> = table
        .columns
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "type": crate::json::type_name(c.data_type),
                "native_type": opt(c.native_type.as_ref()),
                "nullable": c.nullable,
                "default": opt(c.default.as_ref()),
            })
        })
        .collect();
    let foreign_keys: Vec<Value> = table
        .foreign_keys
        .iter()
        .map(|f| {
            json!({
                "name": opt(f.name.as_ref()),
                "columns": f.columns,
                "referenced_schema": opt(f.referenced_schema.as_ref()),
                "referenced_table": f.referenced_table,
                "referenced_columns": f.referenced_columns,
            })
        })
        .collect();
    let mut value = object_json(&table.object);
    if let Some(map) = value.as_object_mut() {
        map.insert("columns".into(), json!(columns));
        map.insert(
            "primary_key".into(),
            table
                .primary_key
                .as_ref()
                .map_or(Value::Null, |pk| json!(pk.columns)),
        );
        map.insert("foreign_keys".into(), json!(foreign_keys));
    }
    value
}

pub(crate) async fn describe_table(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &DescribeArgs,
) -> Result<Value, ToolError> {
    let catalog = ts.catalog(info)?;
    let path = addr_path(args.database.as_deref(), args.schema.as_deref());
    for kind in [
        ObjectKind::Table,
        ObjectKind::View,
        ObjectKind::MaterializedView,
    ] {
        let obj = DbObject::new(kind, path.clone(), args.name.as_str());
        if let Some(table) = catalog.table(obj).await? {
            return Ok(json!({"connection": info.id, "table": table_json(&table)}));
        }
    }
    Err(ToolError::NotFound(format!(
        "no table, view or materialized view named `{}`",
        args.name
    )))
}

/// The definition text of `obj`, from the cache or one session.
async fn definition(
    ts: &Inner,
    info: &ConnectionInfo,
    obj: &DbObject,
) -> Result<Option<String>, ToolError> {
    let key = (info.id.clone(), obj.clone());
    if let Some(hit) = ts.cached_definition(&key) {
        return Ok(hit);
    }
    let mut session = ts.source.acquire(&info.id).await?;
    let text = match session.introspection() {
        Some(introspect) => introspect.definition(obj).await.map_err(ToolError::Driver),
        None => Ok(None),
    };
    ts.source.release(&info.id, session).await;
    let text = text?;
    ts.store_definition(key, text.clone());
    Ok(text)
}

pub(crate) async fn describe_function(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &DescribeArgs,
) -> Result<Value, ToolError> {
    let catalog = ts.catalog(info)?;
    let path = addr_path(args.database.as_deref(), args.schema.as_deref());
    let mut matches: Vec<DbObject> = Vec::new();
    let mut supported = false;
    for kind in [ObjectKind::Function, ObjectKind::Procedure] {
        if let Listing::Objects(objects) =
            &*catalog.children(NodeKey::new(path.clone(), kind)).await?
        {
            supported = true;
            matches.extend(objects.iter().filter(|o| o.name == args.name).cloned());
        }
    }
    if matches.is_empty() {
        return Err(if supported {
            ToolError::NotFound(format!("no function or procedure named `{}`", args.name))
        } else {
            ToolError::Unsupported("this driver lists no functions or procedures".to_owned())
        });
    }
    let mut functions = Vec::new();
    for obj in &matches {
        let text = definition(ts, info, obj).await?;
        functions.push(json!({
            "name": obj.name,
            "kind": obj.kind.as_str(),
            "database": opt(obj.parent.database.as_ref()),
            "schema": opt(obj.parent.schema.as_ref()),
            "definition": text,
        }));
    }
    Ok(json!({"connection": info.id, "functions": functions}))
}

fn path_part(path: &ObjectPath) -> (Value, Value, Value) {
    (
        opt(path.database.as_ref()),
        opt(path.schema.as_ref()),
        opt(path.table.as_ref()),
    )
}

pub(crate) fn search_schema(
    ts: &Inner,
    info: &ConnectionInfo,
    args: &SearchSchemaArgs,
) -> Result<Value, ToolError> {
    let catalog = ts.catalog(info)?;
    let limit = match args.limit {
        None => 50,
        Some(n @ 1..=200) => n as usize,
        Some(n) => {
            return Err(ToolError::InvalidArguments(format!(
                "`limit` must be between 1 and 200, got {n}"
            )));
        }
    };
    let kinds = match &args.kinds {
        Some(k) => name_kinds(k)?,
        None => Vec::new(),
    };
    let query = NameQuery::new(args.query.as_str())
        .fuzzy()
        .with_kinds(kinds)
        .with_limit(limit);
    let hits: Vec<Value> = catalog
        .snapshot()
        .lookup(&query)
        .iter()
        .map(|hit| {
            let (database, schema, table) = path_part(&hit.parent);
            let kind = match hit.kind {
                NameKind::Object(kind) => kind.as_str(),
                _ => "column",
            };
            json!({
                "name": hit.name,
                "kind": kind,
                "database": database,
                "schema": schema,
                "table": table,
                "quality": format!("{:?}", hit.quality).to_lowercase(),
            })
        })
        .collect();
    Ok(json!({"connection": info.id, "hits": hits}))
}
