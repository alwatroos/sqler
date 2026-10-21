// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Tool names, JSON Schemas and the argument structs they describe.

use serde::Deserialize;
use serde_json::{Value, json};
use sqler_ai_api::ToolDefinition;
use sqler_catalog::NameKind;
use sqler_driver_api::sqler_core::{ObjectKind, ObjectPath};

use crate::error::ToolError;

/// The tool names.
pub mod names {
    /// Lists the lent connections.
    pub const LIST_CONNECTIONS: &str = "list_connections";
    /// Lists the schemas of a connection.
    pub const LIST_SCHEMAS: &str = "list_schemas";
    /// Lists tables, views and materialised views.
    pub const LIST_TABLES: &str = "list_tables";
    /// Describes one table or view.
    pub const DESCRIBE_TABLE: &str = "describe_table";
    /// Describes functions and procedures by name.
    pub const DESCRIBE_FUNCTION: &str = "describe_function";
    /// Searches cached names.
    pub const SEARCH_SCHEMA: &str = "search_schema";
    /// Reads sample rows of a table.
    pub const SAMPLE_ROWS: &str = "sample_rows";
    /// Runs SQL, guarded.
    pub const RUN_QUERY: &str = "run_query";
    /// Explains a statement.
    pub const EXPLAIN_QUERY: &str = "explain_query";
    /// Puts SQL into an editor tab (UI hosts only).
    pub const OPEN_IN_EDITOR: &str = "open_in_editor";
    /// Opens a results tab (UI hosts only).
    pub const OPEN_RESULTS_TAB: &str = "open_results_tab";
    /// Natural-language data search (added by a later release; schema only here).
    pub const SEARCH_DATA: &str = "search_data";
}

/// Arguments of a tool that takes none.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListSchemasArgs {
    pub connection: String,
    pub database: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TableKind {
    Table,
    View,
    MaterializedView,
}

impl TableKind {
    pub(crate) fn object_kind(self) -> ObjectKind {
        match self {
            Self::Table => ObjectKind::Table,
            Self::View => ObjectKind::View,
            Self::MaterializedView => ObjectKind::MaterializedView,
        }
    }
}

/// The wire name of a table-like kind (`materialized_view` with an underscore).
pub(crate) fn table_kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::MaterializedView => "materialized_view",
        other => other.as_str(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListTablesArgs {
    pub connection: String,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub kinds: Option<Vec<TableKind>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DescribeArgs {
    pub connection: String,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchSchemaArgs {
    pub connection: String,
    pub query: String,
    pub kinds: Option<Vec<String>>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SampleRowsArgs {
    pub connection: String,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunQueryArgs {
    pub connection: String,
    pub sql: String,
    pub max_rows: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExplainArgs {
    pub connection: String,
    pub sql: String,
    pub analyze: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenInEditorArgs {
    pub sql: String,
    pub connection: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenResultsTabArgs {
    pub connection: String,
    pub sql: String,
    pub title: Option<String>,
}

/// Arguments of `search_data`; the tool itself arrives in a later release.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // schema/struct agreement is tested; the tool is not dispatched yet
pub(crate) struct SearchDataArgs {
    pub connection: String,
    pub question: String,
    pub max_rows: Option<u32>,
}

/// Parses tool arguments, mapping every failure to [`ToolError::InvalidArguments`].
pub(crate) fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))
}

/// The path named by optional `database` and `schema` arguments.
pub(crate) fn addr_path(database: Option<&str>, schema: Option<&str>) -> ObjectPath {
    let mut path = ObjectPath::root();
    if let Some(d) = database {
        path = path.in_database(d);
    }
    if let Some(s) = schema {
        path = path.in_schema(s);
    }
    path
}

/// Parses the `kinds` of `search_schema`.
pub(crate) fn name_kinds(kinds: &[String]) -> Result<Vec<NameKind>, ToolError> {
    kinds
        .iter()
        .map(|k| {
            if k == "column" {
                Ok(NameKind::Column)
            } else {
                ObjectKind::from_id(k).map(NameKind::Object).ok_or_else(|| {
                    ToolError::InvalidArguments(format!("unknown kind `{k}` in `kinds`"))
                })
            }
        })
        .collect()
}

fn object(properties: &Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

const CONNECTION: &str = "The connection id from list_connections.";
const DATABASE: &str = "Database (catalog) name; omit for the connection's default.";
const SCHEMA: &str = "Schema name; omit for the connection's default.";

/// Schema of `list_connections`.
pub(crate) fn list_connections_schema() -> Value {
    object(&json!({}), &[])
}

/// Schema of `list_schemas`.
pub(crate) fn list_schemas_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "database": {"type": "string", "description": DATABASE},
        }),
        &["connection"],
    )
}

/// Schema of `list_tables`.
pub(crate) fn list_tables_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "database": {"type": "string", "description": DATABASE},
            "schema": {"type": "string", "description": SCHEMA},
            "kinds": {
                "type": "array",
                "items": {"type": "string", "enum": ["table", "view", "materialized_view"]},
                "description": "Kinds to list; default all three.",
            },
        }),
        &["connection"],
    )
}

/// Schema of `describe_table`.
pub(crate) fn describe_table_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "database": {"type": "string", "description": DATABASE},
            "schema": {"type": "string", "description": SCHEMA},
            "name": {"type": "string", "description": "The table, view or materialized view name."},
        }),
        &["connection", "name"],
    )
}

/// Schema of `describe_function`.
pub(crate) fn describe_function_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "database": {"type": "string", "description": DATABASE},
            "schema": {"type": "string", "description": SCHEMA},
            "name": {"type": "string", "description": "The function or procedure name."},
        }),
        &["connection", "name"],
    )
}

fn search_kinds() -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = ObjectKind::ALL.iter().map(|k| k.as_str()).collect();
    kinds.push("column");
    kinds
}

/// Schema of `search_schema`.
pub(crate) fn search_schema_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "query": {"type": "string", "description": "Text to look for in cached names (fuzzy, case-insensitive)."},
            "kinds": {
                "type": "array",
                "items": {"type": "string", "enum": search_kinds()},
                "description": "Restrict to these kinds; default all.",
            },
            "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Most hits to return; default 50."},
        }),
        &["connection", "query"],
    )
}

/// Schema of `sample_rows`.
pub(crate) fn sample_rows_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "database": {"type": "string", "description": DATABASE},
            "schema": {"type": "string", "description": SCHEMA},
            "table": {"type": "string", "description": "The table name."},
            "limit": {"type": "integer", "minimum": 1, "description": "Rows to read; default 50, never above the host's row limit."},
        }),
        &["connection", "table"],
    )
}

/// Schema of `run_query`.
pub(crate) fn run_query_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "sql": {"type": "string", "description": "One or more SQL statements. Anything but a plain read needs the user's approval, statement by statement."},
            "max_rows": {"type": "integer", "minimum": 1, "description": "Rows to keep per statement; never above the host's row limit."},
        }),
        &["connection", "sql"],
    )
}

/// Schema of `explain_query`.
pub(crate) fn explain_query_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "sql": {"type": "string", "description": "Exactly one statement to explain."},
            "analyze": {"type": "boolean", "description": "Run the statement and report real timings; needs approval. Default false."},
        }),
        &["connection", "sql"],
    )
}

/// Schema of `open_in_editor`.
pub(crate) fn open_in_editor_schema() -> Value {
    object(
        &json!({
            "sql": {"type": "string", "description": "SQL to put into an editor tab. It is not run."},
            "connection": {"type": "string", "description": CONNECTION},
        }),
        &["sql"],
    )
}

/// Schema of `open_results_tab`.
pub(crate) fn open_results_tab_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "sql": {"type": "string", "description": "Read-only SQL to show in a results tab."},
            "title": {"type": "string", "description": "Tab title."},
        }),
        &["connection", "sql"],
    )
}

/// Schema of `search_data`; the tool is added by a later release, so only tests use it for now.
#[allow(dead_code)]
pub(crate) fn search_data_schema() -> Value {
    object(
        &json!({
            "connection": {"type": "string", "description": CONNECTION},
            "question": {"type": "string", "description": "What to find, in plain language."},
            "max_rows": {"type": "integer", "minimum": 1, "description": "Rows to keep; never above the host's row limit."},
        }),
        &["connection", "question"],
    )
}

/// The tools every host offers, as `(name, description, schema)`.
fn core_tools() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            names::LIST_CONNECTIONS,
            "List the database connections you may use, with driver, environment (dev, staging, prod) and read-only flag.",
            list_connections_schema(),
        ),
        (
            names::LIST_SCHEMAS,
            "List the schemas of a connection from the cached catalog.",
            list_schemas_schema(),
        ),
        (
            names::LIST_TABLES,
            "List tables, views and materialized views of a schema from the cached catalog.",
            list_tables_schema(),
        ),
        (
            names::DESCRIBE_TABLE,
            "Describe a table or view: columns, types, nullability, defaults, primary key and foreign keys.",
            describe_table_schema(),
        ),
        (
            names::DESCRIBE_FUNCTION,
            "Describe functions and procedures with a given name, including their definition text when the database provides it.",
            describe_function_schema(),
        ),
        (
            names::SEARCH_SCHEMA,
            "Search cached schema, table, view, function and column names. Matches names only, not comments or data.",
            search_schema_schema(),
        ),
        (
            names::SAMPLE_ROWS,
            "Read a few rows of a table. Row and time limits apply.",
            sample_rows_schema(),
        ),
        (
            names::RUN_QUERY,
            "Run SQL on a connection. Plain reads run at once; writes, DDL and anything unrecognized need the user's approval per statement, and prod connections need a second confirmation. Row and time limits apply and results are truncated when exceeded.",
            run_query_schema(),
        ),
        (
            names::EXPLAIN_QUERY,
            "Show the execution plan of one statement. With analyze the statement is executed, which needs approval.",
            explain_query_schema(),
        ),
    ]
}

/// The UI-only tools, as `(name, description, schema)`.
fn ui_tools() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            names::OPEN_IN_EDITOR,
            "Put SQL into an editor tab for the user. The SQL is not run.",
            open_in_editor_schema(),
        ),
        (
            names::OPEN_RESULTS_TAB,
            "Show a read-only query in a results tab for the user.",
            open_results_tab_schema(),
        ),
    ]
}

/// The tool definitions a host exports. The UI-only tools are listed only when `ui` is true;
/// `search_data` is never listed here.
#[must_use]
pub fn definitions(ui: bool) -> Vec<ToolDefinition> {
    let mut tools = core_tools();
    if ui {
        tools.extend(ui_tools());
    }
    tools
        .into_iter()
        .map(|(name, description, schema)| ToolDefinition::new(name, description, schema))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(prop: &Value) -> Value {
        if let Some(e) = prop.get("items").and_then(|i| i.get("enum")) {
            return json!([e[0].clone()]);
        }
        match prop["type"].as_str() {
            Some("integer") => json!(1),
            Some("boolean") => json!(true),
            _ => json!("x"),
        }
    }

    /// Every schema agrees with its argument struct: required-only and full objects parse, an
    /// unknown field and a missing required field do not.
    #[test]
    fn schemas_agree_with_structs() {
        fn check<T: for<'de> Deserialize<'de>>(schema: &Value) {
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], false);
            let props = schema["properties"].as_object().unwrap();
            let required: Vec<&str> = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let mut min = serde_json::Map::new();
            let mut full = serde_json::Map::new();
            for (k, v) in props {
                full.insert(k.clone(), sample(v));
                if required.contains(&k.as_str()) {
                    min.insert(k.clone(), sample(v));
                }
            }
            assert!(parse::<T>(Value::Object(min.clone())).is_ok(), "{schema}");
            assert!(parse::<T>(Value::Object(full.clone())).is_ok(), "{schema}");
            let mut extra = full.clone();
            extra.insert("bogus".into(), json!(1));
            assert!(parse::<T>(Value::Object(extra)).is_err());
            for r in &required {
                let mut missing = full.clone();
                missing.remove(*r);
                assert!(parse::<T>(Value::Object(missing)).is_err(), "{r}");
            }
        }
        check::<NoArgs>(&list_connections_schema());
        check::<ListSchemasArgs>(&list_schemas_schema());
        check::<ListTablesArgs>(&list_tables_schema());
        check::<DescribeArgs>(&describe_table_schema());
        check::<DescribeArgs>(&describe_function_schema());
        check::<SearchSchemaArgs>(&search_schema_schema());
        check::<SampleRowsArgs>(&sample_rows_schema());
        check::<RunQueryArgs>(&run_query_schema());
        check::<ExplainArgs>(&explain_query_schema());
        check::<OpenInEditorArgs>(&open_in_editor_schema());
        check::<OpenResultsTabArgs>(&open_results_tab_schema());
        check::<SearchDataArgs>(&search_data_schema());
    }

    #[test]
    fn definitions_lists_ui_tools_on_demand() {
        let plain = definitions(false);
        let ui = definitions(true);
        assert_eq!(plain.len(), 9);
        assert_eq!(ui.len(), 11);
        assert!(plain.iter().all(|d| d.name != names::OPEN_IN_EDITOR));
        assert!(ui.iter().any(|d| d.name == names::OPEN_RESULTS_TAB));
        assert!(ui.iter().all(|d| d.name != names::SEARCH_DATA));
    }

    #[test]
    fn kinds_parse() {
        let kinds = name_kinds(&["column".into(), "materialized-view".into()]).unwrap();
        assert_eq!(
            kinds,
            vec![
                NameKind::Column,
                NameKind::Object(ObjectKind::MaterializedView)
            ]
        );
        assert!(name_kinds(&["bogus".into()]).is_err());
    }

    #[test]
    fn addr_paths() {
        assert_eq!(addr_path(None, None), ObjectPath::root());
        assert_eq!(
            addr_path(Some("d"), Some("s")),
            ObjectPath::root().in_database("d").in_schema("s")
        );
    }
}
