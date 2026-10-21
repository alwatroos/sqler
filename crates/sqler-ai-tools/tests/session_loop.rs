// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The session loop: multi-step success, catalog-only Q&A, bad calls, redaction.
#![allow(clippy::unwrap_used)]

mod common;
mod session_common;

use std::sync::Arc;

use common::SqliteSource;
use serde_json::json;
use session_common::{
    assert_well_formed, provider, session, text, tool_entries, tools, with_events,
};
use sqler_ai_api::mock::ScriptedProvider;
use sqler_ai_api::{AiProvider, Part, Role};
use sqler_ai_tools::session::{
    ExplorationSession, SessionConfig, SessionError, SessionLimits, SessionState, ToolCallStatus,
    TranscriptEntry, TurnOutcome,
};
use sqler_ai_tools::{ToolSet, UNTRUSTED_OPEN, unwrap_untrusted};

#[tokio::test(flavor = "current_thread")]
async fn multi_step_success_builds_the_transcript() {
    let source = SqliteSource::new("loop", None).await;
    let p = provider();
    p.push_script(tools(&[("c1", "list_tables", r#"{"connection":"db"}"#)]));
    p.push_script(tools(&[(
        "c2",
        "run_query",
        r#"{"connection":"db","sql":"select id, name from users order by id"}"#,
    )]));
    p.push_script(text("There are five users."));
    let (mut s, _events) = with_events(session(&source, p.clone(), None, SessionLimits::default()));

    assert_eq!(s.send("who are the users?").await, TurnOutcome::Completed);
    assert_eq!(s.state(), SessionState::Done);

    let kinds: Vec<&str> = s
        .transcript()
        .entries()
        .iter()
        .map(|e| match e {
            TranscriptEntry::User { .. } => "user",
            TranscriptEntry::ToolCall(_) => "call",
            TranscriptEntry::Sql(_) => "sql",
            TranscriptEntry::Assistant { .. } => "assistant",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["user", "call", "call", "sql", "assistant"]);
    let sql = s.transcript().sql().next().unwrap().clone();
    assert_eq!(sql.rows, 5);
    assert_eq!(sql.connection, "db");

    // The second request carries the enveloped result of the first call.
    let requests = p.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .system
            .as_ref()
            .unwrap()
            .contains(UNTRUSTED_OPEN)
    );
    assert!(requests[0].tools.len() > 5);
    let result = requests[1]
        .messages
        .iter()
        .flat_map(|m| m.parts().iter())
        .find_map(|p| match p {
            Part::ToolResult(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(result.call_id, "c1");
    assert!(unwrap_untrusted(&result.content).is_some());
    assert_well_formed(&requests[2].messages);
    assert_eq!(requests[2].messages.last().unwrap().role(), Role::Tool);
    assert!(s.usage().total() > 0);

    // A re-run request is the exact SQL and the connection; no provider request happens.
    let rerun = sql.rerun();
    assert_eq!(rerun.connection, "db");
    assert_eq!(rerun.sql, "select id, name from users order by id");
    assert_eq!(p.requests().len(), 3);
    assert_eq!(source.executed().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn schema_questions_use_catalog_tools_only() {
    let source = SqliteSource::new("catalog", None).await;
    let p = provider();
    p.push_script(tools(&[
        ("c1", "list_tables", r#"{"connection":"db"}"#),
        (
            "c2",
            "describe_table",
            r#"{"connection":"db","name":"users"}"#,
        ),
    ]));
    p.push_script(text("users has id, name, email."));
    let mut s = session(&source, p, None, SessionLimits::default());
    assert_eq!(s.send("what columns?").await, TurnOutcome::Completed);
    assert_eq!(source.acquire_count(), 0);
    assert_eq!(source.executed().len(), 0);
    assert_eq!(s.transcript().sql().count(), 0);
    let calls = tool_entries(s.transcript());
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.status == ToolCallStatus::Ok));
    assert_eq!(calls[0].arguments, json!({"connection": "db"}));
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_and_unknown_calls_do_not_stop_the_loop() {
    let source = SqliteSource::new("bad", None).await;
    let p = provider();
    p.push_script(tools(&[("c1", "run_query", "{oops"), ("c2", "zap", "{}")]));
    p.push_script(text("sorry"));
    let mut s = session(&source, p.clone(), None, SessionLimits::default());
    assert_eq!(s.send("go").await, TurnOutcome::Completed);
    let codes: Vec<String> = tool_entries(s.transcript())
        .into_iter()
        .map(|c| match c.status {
            ToolCallStatus::Error { code, .. } => code,
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(codes, ["invalid_arguments", "unknown_tool"]);
    assert_well_formed(&p.requests()[1].messages);
}

#[tokio::test(flavor = "current_thread")]
async fn secrets_stay_out_of_transcript_events_and_requests() {
    let source = SqliteSource::seeded(
        "secret",
        None,
        None,
        &[
            "create table creds (id integer primary key, api_key text, note text)",
            "insert into creds values (1, 'CANARY-SECRET-1', 'visible')",
        ],
    )
    .await;
    let p = provider();
    p.push_script(tools(&[(
        "c1",
        "run_query",
        r#"{"connection":"db","sql":"select * from creds"}"#,
    )]));
    p.push_script(text("done"));
    let (mut s, events) = with_events(session(&source, p.clone(), None, SessionLimits::default()));
    assert_eq!(s.send("show creds").await, TurnOutcome::Completed);
    assert_eq!(s.transcript().sql().next().unwrap().rows, 1);
    let calls = tool_entries(s.transcript());
    assert!(calls[0].result.to_string().contains("visible"));
    for dump in [
        format!("{:?}", s.transcript()),
        format!("{:?}", events.events()),
        format!("{:?}", p.requests()),
    ] {
        assert!(!dump.contains("CANARY-"), "{dump}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_provider_without_tools_is_refused() {
    let source = SqliteSource::new("notools", None).await;
    let p: Arc<dyn AiProvider> = Arc::new(ScriptedProvider::new("mock"));
    let tools = ToolSet::new(source.clone(), common::limits());
    let err = ExplorationSession::new(p, tools, source.info.clone(), None, SessionConfig::new("m"))
        .unwrap_err();
    assert_eq!(err, SessionError::ToolsUnsupported);
}
