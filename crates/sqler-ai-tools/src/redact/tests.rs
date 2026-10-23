// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use serde_json::{Value, json};

use super::*;

fn scope(send_rows: bool) -> RedactScope {
    RedactScope::new(send_rows, Some(Dialect::Sqlite))
}

fn query_result(sql: &str) -> Value {
    json!({"statements": [{
        "sql": sql,
        "columns": [
            {"name": "id", "type": "integer"},
            {"name": "user_password", "type": "text"},
            {"name": "apiKey", "type": "text"},
            {"name": "note", "type": "text"},
        ],
        "rows": [[1, "CANARY-CELL", null, "NOTE-VISIBLE"], [2, "CANARY-CELL", "k", "n"]],
        "row_count": 2,
        "error": null,
    }]})
}

#[test]
fn r1_hides_listed_columns_and_keeps_names_types_and_other_cells() {
    let mut v = query_result("select * from creds");
    let s = Redaction::default().apply(&mut v, scope(true));
    let st = &v["statements"][0];
    assert_eq!(
        st["rows"][0],
        json!([1, REDACTED, REDACTED, "NOTE-VISIBLE"])
    );
    assert_eq!(st["rows"][1], json!([2, REDACTED, REDACTED, "n"]));
    assert_eq!(st["columns"][1]["name"], "user_password");
    assert_eq!(st["columns"][1]["type"], "text");
    assert_eq!(st["columns"][1]["redacted"], true);
    assert!(st["columns"][3].get("redacted").is_none());
    assert_eq!(
        st["redaction"],
        json!({"columns": ["apiKey", "user_password"], "all_values": false, "rows_withheld": false})
    );
    assert_eq!(s.columns, ["apiKey", "user_password"]);
    assert_eq!(s.tainted_statements, 0);
    assert!(!s.rows_withheld);
    assert!(!v.to_string().contains("CANARY"));
}

#[test]
fn r1_on_a_describe_table_body_replaces_defaults_only_when_non_null() {
    let mut v = json!({"connection": "c", "table": {"columns": [
        {"name": "user_password", "type": "text", "default": "CANARY-DEF"},
        {"name": "token", "type": "text", "default": null},
        {"name": "note", "type": "text", "default": "x"},
    ], "foreign_keys": [{"columns": ["a"]}]}});
    Redaction::default().apply(&mut v, scope(true));
    let cols = &v["table"]["columns"];
    assert_eq!(cols[0]["default"], REDACTED);
    assert_eq!(cols[0]["type"], "text");
    assert_eq!(cols[0]["redacted"], true);
    assert_eq!(cols[1]["default"], Value::Null);
    assert_eq!(cols[1]["redacted"], true);
    assert_eq!(cols[2]["default"], "x");
    assert!(v["table"].get("redaction").is_none());
}

#[test]
fn r2_taints_an_alias_bypass() {
    let mut v = query_result("select note as n, user_password as p from creds");
    // The result columns are the aliases, so only the taint can hide them.
    v["statements"][0]["columns"] = json!([{"name": "n"}, {"name": "p"}]);
    v["statements"][0]["rows"] = json!([["NOTE-VISIBLE", "CANARY-CELL"]]);
    let s = Redaction::default().apply(&mut v, scope(true));
    let st = &v["statements"][0];
    assert_eq!(st["rows"][0], json!([REDACTED, REDACTED]));
    assert_eq!(st["columns"][0]["redacted"], true);
    assert_eq!(st["redaction"]["all_values"], true);
    assert_eq!(s.tainted_statements, 1);
}

#[test]
fn r2_ignores_string_literals_but_sees_quoted_identifiers() {
    let r = Redaction::default();
    let d = Some(Dialect::Sqlite);
    assert!(!r.statement_tainted(d, "select 'my password' from t"));
    assert!(r.statement_tainted(d, "select \"Pass_Word\" from t"));
    assert!(r.statement_tainted(d, "select `api-key` from t"));
    assert!(r.statement_tainted(Some(Dialect::MySql), "select /*! password */ 1"));
    assert!(!r.statement_tainted(d, "select note from t"));
}

#[test]
fn r2_without_a_dialect_matches_the_whole_text() {
    let r = Redaction::default();
    assert!(r.statement_tainted(None, "select 'my password' from t"));
    assert!(!r.statement_tainted(None, "select 1"));
    let mut v = query_result("select user_password as p from creds");
    let s = Redaction::default().apply(&mut v, RedactScope::new(true, None));
    assert_eq!(s.tainted_statements, 1);
    assert_eq!(
        v["statements"][0]["rows"][0],
        json!([REDACTED, REDACTED, REDACTED, REDACTED])
    );
}

#[test]
fn r3_empties_rows_and_keeps_counts_and_columns() {
    let mut v = query_result("select note from creds");
    let s = Redaction::default().apply(&mut v, scope(false));
    let st = &v["statements"][0];
    assert_eq!(st["rows"], json!([]));
    assert_eq!(st["row_count"], 2);
    assert_eq!(st["columns"].as_array().unwrap().len(), 4);
    assert_eq!(st["redaction"]["rows_withheld"], true);
    assert!(s.rows_withheld);
}

#[test]
fn r4_withholds_error_text() {
    let err = "UNIQUE constraint failed: creds.apiKey (CANARY-ERR)";
    let mut on = json!({"statements": [{"sql": "insert into creds values (1)", "columns": [],
        "rows": [], "error": err}]});
    let s = Redaction::default().apply(&mut on, scope(true));
    assert_eq!(on["statements"][0]["error"], WITHHELD_MENTION_FOR_TEST);
    assert_eq!(s.errors_withheld, 1);

    let mut harmless = json!({"statements": [{"sql": "x", "columns": [], "rows": [],
        "error": "no such table: t"}]});
    let s = Redaction::default().apply(&mut harmless, scope(true));
    assert_eq!(harmless["statements"][0]["error"], "no such table: t");
    assert_eq!(s.errors_withheld, 0);

    let mut off = json!({"statements": [{"sql": "x", "columns": [], "rows": [],
        "error": "no such table: t"}]});
    let s = Redaction::default().apply(&mut off, scope(false));
    assert_eq!(
        off["statements"][0]["error"],
        "The error text was withheld because row data is not sent for this connection."
    );
    assert_eq!(s.errors_withheld, 1);
}

const WITHHELD_MENTION_FOR_TEST: &str =
    "The error text mentions a redacted column and was withheld.";

#[test]
fn r4_covers_driver_error_objects_only() {
    let mut driver = json!({"error": {"code": "driver", "message": "bad token near x"}});
    Redaction::default().apply(&mut driver, scope(true));
    assert_eq!(driver["error"]["message"], WITHHELD_MENTION_FOR_TEST);
    assert_eq!(driver["error"]["code"], "driver");

    let mut other = json!({"error": {"code": "denied", "message": "token the user said no"}});
    Redaction::default().apply(&mut other, scope(true));
    assert_eq!(other["error"]["message"], "token the user said no");
}

#[test]
fn catalog_bodies_are_unchanged() {
    let original = json!({"connection": "c", "tables": [{"name": "creds", "schema": "main"}],
        "hits": [{"name": "user_password_hash"}]});
    let mut v = original.clone();
    let s = Redaction::default().apply(&mut v, scope(false));
    assert_eq!(v, original);
    assert_eq!(s, RedactionSummary::default());
}

#[test]
fn cells_are_not_walked_as_structure() {
    let mut v = json!({"sql": "select j from t", "columns": [{"name": "j"}],
        "rows": [[{"columns": [{"name": "password"}], "rows": [[1]]}]]});
    Redaction::default().apply(&mut v, scope(true));
    assert_eq!(v["rows"][0][0]["rows"], json!([[1]]));
}

#[test]
fn applying_twice_gives_the_same_json() {
    for send_rows in [true, false] {
        for sql in [
            "select * from creds",
            "select user_password as p from creds",
        ] {
            let mut v = query_result(sql);
            v["statements"][0]["error"] = json!("bad token");
            let red = Redaction::default();
            red.apply(&mut v, scope(send_rows));
            let once = v.clone();
            red.apply(&mut v, scope(send_rows));
            assert_eq!(v, once, "{sql} send_rows={send_rows}");
        }
    }
}

#[test]
fn a_user_entry_extends_the_list() {
    let list = RedactionList::new(["salary".to_owned()]).unwrap();
    let red = Redaction::new(list);
    assert!(red.column_redacted("Base_Salary"));
    assert!(!Redaction::default().column_redacted("salary"));
}

#[test]
fn summary_columns_are_deduplicated_sorted_and_capped() {
    let cols: Vec<Value> = (0..100)
        .map(|i| json!({"name": format!("password{:03}", 99 - i % 80)}))
        .collect();
    let mut v = json!({"columns": cols});
    let s = Redaction::default().apply(&mut v, scope(true));
    assert_eq!(s.columns.len(), 64);
    assert!(s.columns.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn r4_withholds_the_error_of_a_tainted_statement_without_a_listed_word() {
    let mut v = json!({"statements": [{
        "sql": "select user_password::int from creds",
        "columns": [],
        "rows": [],
        "row_count": 0,
        "error": "invalid input syntax for type integer: \"CANARY-ERR\"",
    }]});
    let s = Redaction::default().apply(&mut v, scope(true));
    assert_eq!(v["statements"][0]["error"], WITHHELD_MENTION_FOR_TEST);
    assert_eq!(s.errors_withheld, 1);
    assert!(!v.to_string().contains("CANARY-ERR"));
    // Idempotent.
    let again = Redaction::default().apply(&mut v, scope(true));
    assert_eq!(again.errors_withheld, 0);
}

#[test]
fn plain_string_columns_of_an_external_tool_are_masked_like_named_ones() {
    let mut v = json!({"columns": ["id", "user_password"], "rows": [[1, "CANARY-CELL"]]});
    let s = Redaction::default().apply(&mut v, scope(true));
    assert_eq!(v["rows"][0], json!([1, REDACTED]));
    assert_eq!(s.columns, vec!["user_password".to_owned()]);
    assert!(!v.to_string().contains("CANARY-CELL"));
}

#[test]
fn plain_string_columns_without_rows_are_not_columns() {
    // `describe_table` foreign keys list column names as plain strings.
    let mut v = json!({"foreign_keys": [{"columns": ["user_password"], "ref_table": "t"}]});
    let before = v.clone();
    let s = Redaction::default().apply(&mut v, scope(true));
    assert_eq!(v, before);
    assert_eq!(s.columns, Vec::<String>::new());
}
