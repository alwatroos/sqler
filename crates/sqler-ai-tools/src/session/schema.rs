// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The compact schema context of a chat turn.
//!
//! The context is built through the session's own [`ToolSet`] (`list_tables`, `search_schema`,
//! `describe_table`), as the assist engine does (ADR-0038). Every name in it therefore comes from
//! a redacted and audited tool result, and the block is wrapped in the untrusted-data envelope.
//! This module writes no database text of its own: it only reshapes parsed tool results into
//! one short line per table.

use std::collections::HashSet;
use std::fmt::Write;

use serde_json::{Value, json};

use crate::assist::grounding::{self, Key};
use crate::schema::names;
use crate::source::ConnectionInfo;
use crate::toolset::ToolSet;
use crate::untrusted::{unwrap_untrusted, wrap_untrusted};

/// The default token budget of the schema context (1024 tokens, 3072 bytes).
pub const DEFAULT_SCHEMA_TOKENS: u32 = 1024;

/// Bytes per token, the rate every budget in the crate uses.
const BYTES_PER_TOKEN: usize = 3;
/// The default number of tables described in full.
const DEFAULT_MAX_DESCRIBED: usize = 8;
/// The most schemas listed when the default listing is empty.
const MAX_SCHEMAS: usize = 3;
/// System schemas that are never listed.
const SYSTEM_SCHEMAS: &[&str] = &[
    "information_schema",
    "pg_catalog",
    "pg_toast",
    "sys",
    "mysql",
    "performance_schema",
];

/// The bounds of the schema context of a chat turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SchemaContextOptions {
    /// The size of the context in tokens, at 3 bytes per token (default 1024).
    ///
    /// The budget bounds the JSON payload; the fixed untrusted-data envelope around it is not
    /// counted.
    pub token_budget: u32,
    /// The most tables described with columns (default 8). The others are listed by name.
    pub max_described: usize,
}

impl Default for SchemaContextOptions {
    fn default() -> Self {
        Self {
            token_budget: DEFAULT_SCHEMA_TOKENS,
            max_described: DEFAULT_MAX_DESCRIBED,
        }
    }
}

/// A table the user is looking at (for example the one selected in the object browser). It is
/// described first.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FocusTable {
    /// The database, if the connection has them.
    pub database: Option<String>,
    /// The schema, if the connection has them.
    pub schema: Option<String>,
    /// The table, view or materialized view name.
    pub name: String,
}

impl FocusTable {
    /// A focus table.
    #[must_use]
    pub fn new(database: Option<String>, schema: Option<String>, name: impl Into<String>) -> Self {
        Self {
            database,
            schema,
            name: name.into(),
        }
    }
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(text)
        .collect()
}

/// `schema.name`, else `database.name`, else `name`.
fn qualified(database: Option<&str>, schema: Option<&str>, name: &str) -> String {
    match schema.or(database) {
        Some(prefix) => format!("{prefix}.{name}"),
        None => name.to_owned(),
    }
}

/// The tables of a `list_tables` body.
fn listed(body: &Value) -> Vec<Key> {
    body["tables"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|t| Some((text(&t["database"]), text(&t["schema"]), text(&t["name"])?)))
        .collect()
}

async fn call_body(tools: &ToolSet, connection: &str, tool: &str, args: &Value) -> Option<Value> {
    grounding::body_of(&tools.call(&grounding::call(connection, tool, args)).await)
}

/// Every listed table: the default listing, else the first schemas' listings.
async fn list_all(tools: &ToolSet, connection: &str) -> Vec<Key> {
    let mut all = call_body(tools, connection, names::LIST_TABLES, &json!({}))
        .await
        .map(|b| listed(&b))
        .unwrap_or_default();
    if !all.is_empty() {
        return all;
    }
    let Some(body) = call_body(tools, connection, names::LIST_SCHEMAS, &json!({})).await else {
        return all;
    };
    let schemas = strings(&body["schemas"]);
    for schema in schemas
        .iter()
        .filter(|s| !SYSTEM_SCHEMAS.contains(&s.to_ascii_lowercase().as_str()))
        .take(MAX_SCHEMAS)
    {
        let args = json!({"schema": schema});
        if let Some(body) = call_body(tools, connection, names::LIST_TABLES, &args).await {
            all.extend(listed(&body));
        }
    }
    all
}

/// The key a focus table has in the listing, else the focus table as given.
fn resolve(focus: &FocusTable, listing: &[Key]) -> Key {
    listing
        .iter()
        .find(|(database, schema, name)| {
            *name == focus.name
                && focus
                    .database
                    .as_ref()
                    .is_none_or(|d| database.as_ref() == Some(d))
                && focus
                    .schema
                    .as_ref()
                    .is_none_or(|s| schema.as_ref() == Some(s))
        })
        .cloned()
        .unwrap_or_else(|| {
            (
                focus.database.clone(),
                focus.schema.clone(),
                focus.name.clone(),
            )
        })
}

fn same_part(a: Option<&String>, b: Option<&String>) -> bool {
    a.is_none() || b.is_none() || a == b
}

fn parts(key: &Key) -> usize {
    usize::from(key.0.is_some()) + usize::from(key.1.is_some())
}

/// Adds `key` to `ranked` unless the table is already there. A listing may leave out the schema
/// that a search hit names, so keys that differ only by a missing part are one table; the
/// fuller key replaces the earlier one in its place.
fn add_ranked(ranked: &mut Vec<Key>, key: Key) {
    let same = ranked.iter().position(|k| {
        k.2 == key.2
            && same_part(k.0.as_ref(), key.0.as_ref())
            && same_part(k.1.as_ref(), key.1.as_ref())
    });
    match same {
        Some(i) if parts(&key) > parts(&ranked[i]) => ranked[i] = key,
        Some(_) => {}
        None => ranked.push(key),
    }
}

/// The reference part of a foreign key: `schema.table`, or `table`.
fn reference(fk: &Value) -> String {
    let table = fk["referenced_table"].as_str().unwrap_or("?");
    match fk["referenced_schema"].as_str() {
        Some(schema) => format!("{schema}.{table}"),
        None => table.to_owned(),
    }
}

/// One line for the body of a `describe_table` result, or `None` if it has no table.
fn render(body: &Value) -> Option<String> {
    let table = body.get("table")?;
    let name = table["name"].as_str()?;
    let pk = strings(&table["primary_key"]);
    let fks = table["foreign_keys"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let single_fk = |column: &str| {
        fks.iter().find(|fk| {
            let cols = strings(&fk["columns"]);
            cols.len() == 1 && cols[0] == column
        })
    };
    let mut parts: Vec<String> = Vec::new();
    for column in table["columns"].as_array().map_or(&[][..], Vec::as_slice) {
        let Some(col) = column["name"].as_str() else {
            continue;
        };
        let ty = column["native_type"]
            .as_str()
            .or_else(|| column["type"].as_str())
            .unwrap_or("?");
        let mut part = format!("{col} {ty}");
        if pk.iter().any(|p| p == col) {
            part.push_str(" PK");
        } else if column["nullable"] == false {
            part.push_str(" NOT NULL");
        }
        if let Some(fk) = single_fk(col) {
            let target = strings(&fk["referenced_columns"]);
            let _ = write!(
                part,
                " -> {}.{}",
                reference(fk),
                target.first().map_or("?", String::as_str)
            );
        }
        parts.push(part);
    }
    for fk in fks {
        let cols = strings(&fk["columns"]);
        if cols.len() > 1 {
            parts.push(format!(
                "FK({}) -> {}({})",
                cols.join(","),
                reference(fk),
                strings(&fk["referenced_columns"]).join(",")
            ));
        }
    }
    let qualified = qualified(table["database"].as_str(), table["schema"].as_str(), name);
    Some(format!("{qualified}({})", parts.join(", ")))
}

/// The size of the payload inside the envelope.
fn payload_len(value: &Value) -> usize {
    let wrapped = wrap_untrusted(value);
    unwrap_untrusted(&wrapped).map_or(wrapped.len(), str::len)
}

fn payload(tables: &[String], others: &[String], truncated: bool) -> Value {
    json!({"tables": tables, "other_tables": others, "truncated": truncated})
}

/// Builds the schema context of one turn: the wrapped envelope with a compact line per table.
///
/// Tables rank focus first, then by `search_schema` hits for the terms of `text`, then by name.
/// The payload stays within `opts.token_budget` tokens (3 bytes each). A failed listing gives an
/// empty payload, never an error.
pub(crate) async fn build_schema_context(
    tools: &ToolSet,
    info: &ConnectionInfo,
    text: &str,
    focus: &[FocusTable],
    opts: SchemaContextOptions,
) -> String {
    let cap = opts.token_budget as usize * BYTES_PER_TOKEN;
    let listing = list_all(tools, &info.id).await;
    let scores = grounding::score(tools, &info.id, &grounding::terms(text))
        .await
        .unwrap_or_default();

    let mut ranked: Vec<Key> = Vec::new();
    for f in focus {
        add_ranked(&mut ranked, resolve(f, &listing));
    }
    let mut hits: Vec<(Key, u32)> = scores.into_iter().collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.2.cmp(&b.0.2)));
    // Only names a tool returned may be listed without a description.
    let known: HashSet<String> = listing
        .iter()
        .map(|k| k.2.clone())
        .chain(hits.iter().map(|(k, _)| k.2.clone()))
        .collect();
    for (key, _) in hits {
        add_ranked(&mut ranked, key);
    }
    let mut rest = listing;
    rest.sort_by(|a, b| a.2.cmp(&b.2));
    for key in rest {
        add_ranked(&mut ranked, key);
    }

    let mut lines: Vec<String> = Vec::new();
    let mut others: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut undescribed: Vec<Key> = Vec::new();
    for (index, key) in ranked.into_iter().enumerate() {
        let line = if index < opts.max_described {
            let args = json!({"database": key.0, "schema": key.1, "name": key.2});
            call_body(tools, &info.id, names::DESCRIBE_TABLE, &args)
                .await
                .and_then(|b| render(&b))
        } else {
            None
        };
        match line {
            Some(line) => {
                lines.push(line);
                if payload_len(&payload(&lines, &others, truncated)) > cap {
                    lines.pop();
                    truncated = true;
                    undescribed.push(key);
                }
            }
            None => undescribed.push(key),
        }
    }
    for (database, schema, name) in undescribed {
        if !known.contains(&name) {
            continue;
        }
        others.push(qualified(database.as_deref(), schema.as_deref(), &name));
        if payload_len(&payload(&lines, &others, truncated)) > cap {
            others.pop();
            truncated = true;
        }
    }
    wrap_untrusted(&payload(&lines, &others, truncated))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn table(extra: &Value) -> Value {
        let mut base = json!({
            "name": "orders", "kind": "table", "database": null, "schema": "main",
            "columns": [
                {"name": "id", "type": "integer", "native_type": "INTEGER", "nullable": false},
                {"name": "user_id", "type": "integer", "native_type": null, "nullable": true},
                {"name": "total", "type": "real", "native_type": null, "nullable": false},
                {"name": "a", "type": "text", "native_type": null, "nullable": true},
                {"name": "b", "type": "text", "native_type": null, "nullable": true},
            ],
            "primary_key": ["id"],
            "foreign_keys": [],
        });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        json!({"table": base})
    }

    #[test]
    fn renders_pk_not_null_and_a_single_foreign_key() {
        let body = table(&json!({"foreign_keys": [
            {"columns": ["user_id"], "referenced_schema": "main",
             "referenced_table": "users", "referenced_columns": ["id"]}
        ]}));
        assert_eq!(
            render(&body).unwrap(),
            "main.orders(id INTEGER PK, user_id integer -> main.users.id, total real NOT NULL, \
             a text, b text)"
        );
    }

    #[test]
    fn renders_a_multi_column_foreign_key_after_the_columns() {
        let body = table(&json!({"foreign_keys": [
            {"columns": ["a", "b"], "referenced_schema": "main",
             "referenced_table": "pairs", "referenced_columns": ["x", "y"]}
        ]}));
        let line = render(&body).unwrap();
        assert!(line.ends_with(", FK(a,b) -> main.pairs(x,y))"), "{line}");
        assert!(!line.contains("a text ->"), "{line}");
    }

    #[test]
    fn a_table_without_a_schema_uses_the_database_then_the_bare_name() {
        let mut body = table(&json!({"schema": null, "database": "shop"}));
        assert!(render(&body).unwrap().starts_with("shop.orders("));
        body["table"]["database"] = Value::Null;
        assert!(render(&body).unwrap().starts_with("orders("));
    }

    #[test]
    fn a_body_without_a_table_renders_nothing() {
        assert_eq!(render(&json!({"error": "x"})), None);
    }

    #[test]
    fn focus_resolves_to_the_listed_key() {
        let listing = vec![(None, Some("main".to_owned()), "orders".to_owned())];
        let f = FocusTable::new(None, None, "orders");
        assert_eq!(resolve(&f, &listing), listing[0]);
        let f = FocusTable::new(None, Some("other".to_owned()), "orders");
        assert_eq!(
            resolve(&f, &listing),
            (None, Some("other".to_owned()), "orders".to_owned())
        );
    }

    #[test]
    fn the_payload_size_ignores_the_envelope() {
        let value = payload(&["t(a int)".to_owned()], &[], false);
        assert_eq!(
            payload_len(&value),
            serde_json::to_string(&value).unwrap().len()
        );
    }

    #[test]
    fn defaults() {
        let o = SchemaContextOptions::default();
        assert_eq!((o.token_budget, o.max_described), (1024, 8));
    }
}
