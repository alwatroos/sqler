// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The six catalog tools on a real SQLite file and a fake function catalog.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::atomic::Ordering;

use common::{FunctionSource, SqliteSource, call, limits};
use serde_json::json;
use sqler_ai_tools::{ToolSet, names};
use sqler_driver_api::sqler_core::Environment;

async fn set(env: Option<Environment>) -> (ToolSet, std::sync::Arc<SqliteSource>) {
    let source = SqliteSource::new("cat", env).await;
    (ToolSet::new(source.clone(), limits()), source)
}

#[tokio::test(flavor = "current_thread")]
async fn list_connections_shows_environment_and_read_only() {
    let (ts, _s) = set(Some(Environment::Prod)).await;
    let (report, v) = call(&ts, names::LIST_CONNECTIONS, json!({})).await;
    assert!(!report.result.is_error);
    assert_eq!(
        v,
        json!({"connections": [{
            "id": "db", "name": "Test DB", "driver": "sqlite",
            "environment": "prod", "read_only": false
        }]})
    );
    let (ts, _s) = set(None).await;
    let (_, v) = call(&ts, names::LIST_CONNECTIONS, json!({})).await;
    assert_eq!(v["connections"][0]["environment"], json!(null));
}

#[tokio::test(flavor = "current_thread")]
async fn schemas_and_tables() {
    let (ts, s) = set(None).await;
    let (_, v) = call(&ts, names::LIST_SCHEMAS, json!({"connection": "db"})).await;
    assert_eq!(v["supported"], true);
    assert_eq!(v["schemas"], json!(["main"]));

    let (_, v) = call(
        &ts,
        names::LIST_TABLES,
        json!({"connection": "db", "schema": "main"}),
    )
    .await;
    let names_kinds: Vec<(String, String)> = v["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_owned(),
                t["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        names_kinds,
        [
            ("orders".to_owned(), "table".to_owned()),
            ("users".to_owned(), "table".to_owned()),
            ("user_names".to_owned(), "view".to_owned()),
        ]
    );
    assert_eq!(v["tables"][0]["schema"], "main");

    let (_, v) = call(
        &ts,
        names::LIST_TABLES,
        json!({"connection": "db", "schema": "main", "kinds": ["view"]}),
    )
    .await;
    assert_eq!(v["tables"].as_array().unwrap().len(), 1);
    assert_eq!(s.acquire_count(), 0, "catalog tools never take a session");
}

#[tokio::test(flavor = "current_thread")]
async fn describe_table_and_view() {
    let (ts, _s) = set(None).await;
    let (_, v) = call(
        &ts,
        names::DESCRIBE_TABLE,
        json!({"connection": "db", "schema": "main", "name": "orders"}),
    )
    .await;
    let t = &v["table"];
    assert_eq!(t["name"], "orders");
    assert_eq!(t["kind"], "table");
    assert_eq!(t["primary_key"], json!(["id"]));
    assert_eq!(t["columns"][0]["name"], "id");
    assert_eq!(t["columns"][0]["type"], "integer");
    assert_eq!(t["columns"][2]["name"], "total");
    assert_eq!(t["foreign_keys"][0]["columns"], json!(["user_id"]));
    assert_eq!(t["foreign_keys"][0]["referenced_table"], "users");
    assert_eq!(t["foreign_keys"][0]["referenced_columns"], json!(["id"]));

    let (_, v) = call(
        &ts,
        names::DESCRIBE_TABLE,
        json!({"connection": "db", "schema": "main", "name": "users"}),
    )
    .await;
    let email = &v["table"]["columns"][2];
    assert_eq!(email["default"], "'none'");
    assert_eq!(email["nullable"], true);
    assert_eq!(v["table"]["columns"][1]["nullable"], false);

    let (_, v) = call(
        &ts,
        names::DESCRIBE_TABLE,
        json!({"connection": "db", "schema": "main", "name": "user_names"}),
    )
    .await;
    assert_eq!(v["table"]["kind"], "view");
    assert_eq!(v["table"]["primary_key"], json!(null));

    let (report, v) = call(
        &ts,
        names::DESCRIBE_TABLE,
        json!({"connection": "db", "schema": "main", "name": "nope"}),
    )
    .await;
    assert!(report.result.is_error);
    assert_eq!(v["error"]["code"], "not_found");
}

#[tokio::test(flavor = "current_thread")]
async fn second_call_is_served_from_the_cache() {
    let (ts, s) = set(None).await;
    let args = json!({"connection": "db", "schema": "main", "name": "users"});
    let list = json!({"connection": "db", "schema": "main"});
    let (_, first) = call(&ts, names::DESCRIBE_TABLE, args.clone()).await;
    let (_, tables) = call(&ts, names::LIST_TABLES, list.clone()).await;
    let (_, schemas) = call(&ts, names::LIST_SCHEMAS, json!({"connection": "db"})).await;
    let after_first = s.introspection_calls();
    assert!(after_first >= 3);
    let (_, second) = call(&ts, names::DESCRIBE_TABLE, args).await;
    let (_, tables2) = call(&ts, names::LIST_TABLES, list).await;
    let (_, schemas2) = call(&ts, names::LIST_SCHEMAS, json!({"connection": "db"})).await;
    assert_eq!(first, second);
    assert_eq!(tables, tables2);
    assert_eq!(schemas, schemas2);
    assert_eq!(s.introspection_calls(), after_first, "cached: no DB call");
    assert_eq!(s.acquire_count(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn search_schema_finds_cached_names() {
    let (ts, s) = set(None).await;
    call(
        &ts,
        names::DESCRIBE_TABLE,
        json!({"connection": "db", "schema": "main", "name": "users"}),
    )
    .await;
    call(
        &ts,
        names::LIST_TABLES,
        json!({"connection": "db", "schema": "main"}),
    )
    .await;
    let before = s.introspection_calls();
    let (_, v) = call(
        &ts,
        names::SEARCH_SCHEMA,
        json!({"connection": "db", "query": "usr"}),
    )
    .await;
    let hits = v["hits"].as_array().unwrap();
    assert!(
        hits.iter()
            .any(|h| h["name"] == "users" && h["kind"] == "table")
    );
    assert!(
        hits.iter()
            .any(|h| h["name"] == "user_names" && h["kind"] == "view")
    );

    let (_, v) = call(
        &ts,
        names::SEARCH_SCHEMA,
        json!({"connection": "db", "query": "email", "kinds": ["column"]}),
    )
    .await;
    let hit = &v["hits"][0];
    assert_eq!(hit["name"], "email");
    assert_eq!(hit["kind"], "column");
    assert_eq!(hit["table"], "users");
    assert!(hit["quality"].is_string());

    let (_, v) = call(
        &ts,
        names::SEARCH_SCHEMA,
        json!({"connection": "db", "query": "u", "limit": 1}),
    )
    .await;
    assert_eq!(v["hits"].as_array().unwrap().len(), 1);
    assert_eq!(s.introspection_calls(), before);

    for bad in [
        json!({"limit": 0}),
        json!({"limit": 201}),
        json!({"kinds": ["bogus"]}),
    ] {
        let mut args = json!({"connection": "db", "query": "u"});
        args.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        let (report, v) = call(&ts, names::SEARCH_SCHEMA, args).await;
        assert!(report.result.is_error);
        assert_eq!(v["error"]["code"], "invalid_arguments");
    }
    assert_eq!(s.acquire_count(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn describe_function_reads_definition_once() {
    let source = FunctionSource::new();
    let ts = ToolSet::new(source.clone(), limits());
    let args = json!({"connection": "pg", "name": "area"});
    let (report, v) = call(&ts, names::DESCRIBE_FUNCTION, args.clone()).await;
    assert!(!report.result.is_error, "{v}");
    let functions = v["functions"].as_array().unwrap();
    assert_eq!(functions.len(), 2);
    assert_eq!(functions[0]["kind"], "function");
    assert_eq!(functions[0]["definition"], "create function area ...");
    assert_eq!(functions[1]["kind"], "procedure");
    assert_eq!(source.acquires.load(Ordering::SeqCst), 2);
    assert_eq!(source.intro.definitions.load(Ordering::SeqCst), 2);

    let (_, again) = call(&ts, names::DESCRIBE_FUNCTION, args).await;
    assert_eq!(again, v);
    assert_eq!(source.intro.definitions.load(Ordering::SeqCst), 2, "cached");
    assert_eq!(source.acquires.load(Ordering::SeqCst), 2);

    let (report, v) = call(
        &ts,
        names::DESCRIBE_FUNCTION,
        json!({"connection": "pg", "name": "missing"}),
    )
    .await;
    assert!(report.result.is_error);
    assert_eq!(v["error"]["code"], "not_found");
}

#[tokio::test(flavor = "current_thread")]
async fn describe_function_on_a_driver_without_functions() {
    let (ts, _s) = set(None).await;
    let (report, v) = call(
        &ts,
        names::DESCRIBE_FUNCTION,
        json!({"connection": "db", "name": "f"}),
    )
    .await;
    assert!(report.result.is_error);
    assert_eq!(v["error"]["code"], "unsupported");
}

#[tokio::test(flavor = "current_thread")]
async fn argument_and_connection_errors() {
    let (ts, _s) = set(None).await;
    let cases = [
        (
            names::LIST_SCHEMAS,
            json!({"connection": "zzz"}),
            "unknown_connection",
        ),
        (names::LIST_SCHEMAS, json!({}), "invalid_arguments"),
        (
            names::LIST_SCHEMAS,
            json!({"connection": "db", "extra": 1}),
            "invalid_arguments",
        ),
        (
            names::LIST_CONNECTIONS,
            json!({"x": 1}),
            "invalid_arguments",
        ),
        ("no_such_tool", json!({}), "unknown_tool"),
    ];
    for (tool, args, code) in cases {
        let (report, v) = call(&ts, tool, args).await;
        assert!(report.result.is_error, "{tool}");
        assert_eq!(v["error"]["code"], code, "{tool}");
        assert!(v["error"]["message"].is_string());
    }
    let report = ts
        .call_detailed(&sqler_ai_api::ToolCall::new(
            "1",
            names::LIST_SCHEMAS,
            "{oops",
        ))
        .await;
    assert!(report.result.is_error);
    assert!(report.result.content.contains("invalid_arguments"));
    let report = ts
        .call_detailed(&sqler_ai_api::ToolCall::new(
            "1",
            names::LIST_CONNECTIONS,
            "",
        ))
        .await;
    assert!(!report.result.is_error, "empty arguments mean {{}}");
    assert_eq!(report.result.call_id, "1");
}

#[tokio::test(flavor = "current_thread")]
async fn connection_resolves_by_name() {
    let (ts, _s) = set(None).await;
    let (report, v) = call(&ts, names::LIST_SCHEMAS, json!({"connection": "Test DB"})).await;
    assert!(!report.result.is_error);
    assert_eq!(v["connection"], "db");
    assert_eq!(report.connection.as_deref(), Some("db"));
}
