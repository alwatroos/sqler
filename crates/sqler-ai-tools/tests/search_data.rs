// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `search_data` tool: listed only with a data-search host, read-only, shown before it runs,
//! limited, redacted and audited (AC5).
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod assist_support;
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use assist_support::{answer, scripted};
use common::{SqliteSource, call};
use serde_json::json;
use sqler_ai_api::BoxFuture;
use sqler_ai_tools::{
    Assist, AssistSearch, ConnectionInfo, DataSearch, GeneratedSql, Limits, MemoryAudit, Surface,
    ToolError, ToolSet, names,
};

/// Writes a fixed statement and logs what the engine does with it.
struct Fixed {
    sql: String,
    source: Arc<SqliteSource>,
    /// `(sql, how many statements had reached the database when it was shown)`.
    shown: Mutex<Vec<(String, usize)>>,
}

impl Fixed {
    fn new(sql: &str, source: &Arc<SqliteSource>) -> Arc<Self> {
        Arc::new(Self {
            sql: sql.to_owned(),
            source: source.clone(),
            shown: Mutex::new(Vec::new()),
        })
    }
}

impl DataSearch for Fixed {
    fn generate<'a>(
        &'a self,
        _connection: &'a ConnectionInfo,
        _question: &'a str,
    ) -> BoxFuture<'a, Result<GeneratedSql, ToolError>> {
        Box::pin(std::future::ready(Ok(GeneratedSql::new(
            self.sql.clone(),
            "because",
        ))))
    }

    fn shown<'a>(&'a self, _connection: &'a ConnectionInfo, sql: &'a str) -> BoxFuture<'a, ()> {
        let at = self.source.executed().len();
        self.shown.lock().unwrap().push((sql.to_owned(), at));
        Box::pin(std::future::ready(()))
    }
}

fn args(question: &str) -> serde_json::Value {
    json!({"connection": "db", "question": question})
}

#[tokio::test(flavor = "current_thread")]
async fn listed_only_with_a_data_search_host() {
    let source = SqliteSource::new("sd-list", None).await;
    let plain = ToolSet::new(source.clone(), Limits::default());
    assert!(
        plain
            .definitions()
            .iter()
            .all(|d| d.name != names::SEARCH_DATA)
    );
    let (report, body) = call(&plain, names::SEARCH_DATA, args("users")).await;
    assert!(report.result.is_error);
    assert_eq!(body["error"]["code"], "not_available");

    let with = plain.with_data_search(Fixed::new("select 1", &source));
    let definition = with
        .definitions()
        .into_iter()
        .find(|d| d.name == names::SEARCH_DATA)
        .unwrap();
    assert!(definition.description.contains("read-only"));
    assert_eq!(
        definition.parameters["required"],
        json!(["connection", "question"])
    );
    assert!(
        plain
            .definitions()
            .iter()
            .all(|d| d.name != names::SEARCH_DATA)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_read_runs_after_it_is_shown_and_returns_sql_and_rows() {
    let source = SqliteSource::new("sd-read", None).await;
    let audit = Arc::new(MemoryAudit::new());
    let host = Fixed::new("select id, name from users order by id", &source);
    let ts = ToolSet::new(source.clone(), Limits::default())
        .with_audit(audit.clone(), Surface::Mcp)
        .with_data_search(host.clone());
    let (report, body) = call(&ts, names::SEARCH_DATA, args("who are the users")).await;
    assert!(!report.result.is_error, "{body}");
    assert_eq!(body["sql"], "select id, name from users order by id");
    assert_eq!(body["rationale"], "because");
    assert_eq!(body["connection"], "db");
    assert_eq!(body["statements"][0]["row_count"], 5);
    assert_eq!(body["statements"][0]["rows"][0], json!([1, "ann"]));
    assert!(body["limits"]["max_rows"].is_number());
    assert_eq!(
        *host.shown.lock().unwrap(),
        [("select id, name from users order by id".to_owned(), 0)],
        "shown before anything reached the database"
    );
    assert_eq!(
        source.executed(),
        ["select id, name from users order by id"]
    );
    let entries = audit.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].tool, names::SEARCH_DATA);
    assert_eq!(entries[0].statements.len(), 1);
    assert_eq!(
        entries[0].statements[0].sql,
        "select id, name from users order by id"
    );
    assert!(entries[0].statements[0].ran);
}

#[tokio::test(flavor = "current_thread")]
async fn anything_but_a_plain_read_is_refused() {
    for (i, sql) in [
        "delete from users",
        "select 1; delete from users",
        "drop table users",
        "pragma writable_schema = 1",
        "",
    ]
    .into_iter()
    .enumerate()
    {
        let source = SqliteSource::new(&format!("sd-refuse-{i}"), None).await;
        let audit = Arc::new(MemoryAudit::new());
        let host = Fixed::new(sql, &source);
        let ts = ToolSet::new(source.clone(), Limits::default())
            .with_audit(audit.clone(), Surface::Mcp)
            .with_data_search(host.clone());
        let (report, body) = call(&ts, names::SEARCH_DATA, args("remove them")).await;
        assert!(report.result.is_error, "{sql}: {body}");
        assert_eq!(body["error"]["code"], "invalid_arguments", "{sql}");
        assert_eq!(source.executed(), Vec::<String>::new(), "{sql}");
        assert_eq!(source.acquire_count(), 0, "{sql}");
        assert!(host.shown.lock().unwrap().is_empty(), "{sql}");
        let entries = audit.entries();
        assert_eq!(entries.len(), 1);
        if !sql.is_empty() {
            assert!(!entries[0].statements.is_empty(), "{sql}");
            assert!(entries[0].statements.iter().all(|s| !s.ran));
            assert_eq!(entries[0].statements[0].sql, sql.split(';').next().unwrap());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn bad_arguments_are_rejected_before_generating() {
    let source = SqliteSource::new("sd-args", None).await;
    let host = Fixed::new("select 1", &source);
    let ts = ToolSet::new(source.clone(), Limits::default()).with_data_search(host.clone());
    for bad in [
        json!({"connection": "db", "question": "  "}),
        json!({"connection": "db", "question": "x", "max_rows": 0}),
        json!({"connection": "db", "question": "x", "extra": 1}),
        json!({"connection": "db"}),
    ] {
        let (report, body) = call(&ts, names::SEARCH_DATA, bad.clone()).await;
        assert!(report.result.is_error, "{bad}");
        assert_eq!(body["error"]["code"], "invalid_arguments", "{bad}");
    }
    let (_, body) = call(
        &ts,
        names::SEARCH_DATA,
        json!({"connection": "x", "question": "q"}),
    )
    .await;
    assert_eq!(body["error"]["code"], "unknown_connection");
    assert_eq!(source.executed(), Vec::<String>::new());
}

#[tokio::test(flavor = "current_thread")]
async fn the_row_cap_and_the_requested_max_rows_apply() {
    let source = SqliteSource::new("sd-rows", None).await;
    let host = Fixed::new("select id from users order by id", &source);
    let ts = ToolSet::new(
        source.clone(),
        Limits::new(3, Duration::from_secs(30), false),
    )
    .with_data_search(host);
    let (_, body) = call(&ts, names::SEARCH_DATA, args("users")).await;
    assert_eq!(body["statements"][0]["row_count"], 3);
    assert_eq!(body["statements"][0]["truncated"], "row_limit");
    let (_, body) = call(
        &ts,
        names::SEARCH_DATA,
        json!({"connection": "db", "question": "users", "max_rows": 2}),
    )
    .await;
    assert_eq!(body["statements"][0]["row_count"], 2);
    let (_, body) = call(
        &ts,
        names::SEARCH_DATA,
        json!({"connection": "db", "question": "users", "max_rows": 100}),
    )
    .await;
    assert_eq!(
        body["statements"][0]["row_count"], 3,
        "never above the host's cap"
    );
}

const SECRETS: &[&str] = &[
    "create table creds (id integer primary key, user_password text, note text)",
    "insert into creds (id, user_password, note) values (1, 'CANARY-CELL', 'NOTE-VISIBLE')",
];

#[tokio::test(flavor = "current_thread")]
async fn redaction_and_send_rows_apply() {
    let source = SqliteSource::seeded("sd-redact", None, None, SECRETS).await;
    let ts = ToolSet::new(source.clone(), Limits::default())
        .with_data_search(Fixed::new("select * from creds", &source));
    let (report, body) = call(&ts, names::SEARCH_DATA, args("credentials")).await;
    assert!(!report.result.content.contains("CANARY-CELL"));
    assert!(report.result.content.contains("NOTE-VISIBLE"));
    assert_eq!(body["statements"][0]["columns"][1]["redacted"], true);

    let source = SqliteSource::seeded("sd-norows", None, Some(false), SECRETS).await;
    let ts = ToolSet::new(source.clone(), Limits::default())
        .with_data_search(Fixed::new("select note from creds", &source));
    let (report, body) = call(&ts, names::SEARCH_DATA, args("notes")).await;
    assert!(!report.result.content.contains("NOTE-VISIBLE"));
    assert_eq!(body["statements"][0]["rows"], json!([]));
    assert!(report.redaction.rows_withheld);
}

#[tokio::test(flavor = "current_thread")]
async fn assist_search_writes_the_sql_and_calls_the_notice() {
    let source = SqliteSource::new("sd-assist", None).await;
    let provider = scripted(
        false,
        true,
        vec![
            answer(r#"{"sql":"select name from users order by id","rationale":"names"}"#),
            answer(r#"{"sql":"delete from users","rationale":"oops"}"#),
        ],
    );
    let inner = ToolSet::new(source.clone(), Limits::default());
    let assist = Assist::new(inner, provider.clone(), "test-model");
    let notices = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let log = notices.clone();
    let search = AssistSearch::new(assist).with_notice(Arc::new(move |conn, sql| {
        log.lock().unwrap().push((conn.to_owned(), sql.to_owned()));
    }));
    let ts = ToolSet::new(source.clone(), Limits::default()).with_data_search(Arc::new(search));
    let (report, body) = call(&ts, names::SEARCH_DATA, args("list the user names")).await;
    assert!(!report.result.is_error, "{body}");
    assert_eq!(body["sql"], "select name from users order by id");
    assert_eq!(body["rationale"], "names");
    assert_eq!(body["statements"][0]["rows"][0], json!(["ann"]));
    assert_eq!(
        *notices.lock().unwrap(),
        [(
            "db".to_owned(),
            "select name from users order by id".to_owned()
        )]
    );
    let request = &provider.requests()[0];
    assert!(
        request
            .system
            .as_deref()
            .unwrap()
            .contains("exactly one read-only SELECT")
    );

    // A write written by the model is refused and never shown.
    let executed = source.executed().len();
    let (report, body) = call(&ts, names::SEARCH_DATA, args("remove the users")).await;
    assert!(report.result.is_error);
    assert_eq!(body["error"]["code"], "invalid_arguments");
    assert_eq!(source.executed().len(), executed);
    assert_eq!(notices.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_generation_is_a_host_error() {
    let source = SqliteSource::new("sd-nosql", None).await;
    let provider = scripted(false, false, vec![answer("I cannot.")]);
    let assist = Assist::new(
        ToolSet::new(source.clone(), Limits::default()),
        provider,
        "test-model",
    );
    let ts = ToolSet::new(source.clone(), Limits::default())
        .with_data_search(Arc::new(AssistSearch::new(assist)));
    let (report, body) = call(&ts, names::SEARCH_DATA, args("users")).await;
    assert!(report.result.is_error);
    assert_eq!(body["error"]["code"], "host");
    assert_eq!(source.executed(), Vec::<String>::new());
}
