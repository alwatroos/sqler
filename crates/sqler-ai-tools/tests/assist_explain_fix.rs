// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Explain with a plan (never ANALYZE) and fix with a diff (AC3, AC4), on SQLite.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod assist_support;
mod common;

use std::sync::Arc;

use assist_support::{all_requests, answer, assert_inside_envelopes, render, scripted};
use common::SqliteSource;
use sqler_ai_api::mock::ScriptedProvider;
use sqler_ai_tools::{
    Assist, AssistError, AssistOptions, Conversation, Limits, PROMPT_VERSION, ToolSet,
};

fn engine(source: &Arc<SqliteSource>, provider: &Arc<ScriptedProvider>) -> Assist {
    Assist::new(
        ToolSet::new(source.clone(), Limits::default()),
        provider.clone(),
        "test-model",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn explain_with_plan_uses_explain_query_plan_only() {
    let source = SqliteSource::new("explain-plan", None).await;
    let provider = scripted(false, false, vec![answer("It reads one user by key.")]);
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let result = assist
        .explain("db", "select name from users where id = 1", true, &mut conv)
        .await
        .unwrap();
    assert!(result.plan_included);
    assert_eq!(result.text, "It reads one user by key.");
    assert_eq!(result.prompt_version, PROMPT_VERSION);
    let executed = source.executed();
    assert_ne!(executed, Vec::<String>::new());
    for sql in &executed {
        assert!(sql.starts_with("EXPLAIN QUERY PLAN "), "{sql}");
        assert!(!sql.to_ascii_uppercase().contains("ANALYZE"), "{sql}");
    }
    let prompt = all_requests(&provider);
    assert!(prompt.contains("Query plan (tool result):"));
    assert_inside_envelopes(&prompt, "USING INTEGER PRIMARY KEY");
    assert_eq!(conv.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn explain_without_plan_runs_nothing() {
    let source = SqliteSource::new("explain-noplan", None).await;
    let provider = scripted(false, false, vec![answer("A scan.")]);
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let result = assist
        .explain("db", "select * from users", false, &mut conv)
        .await
        .unwrap();
    assert!(!result.plan_included);
    assert_eq!(source.executed(), Vec::<String>::new());
    assert!(!all_requests(&provider).contains("Query plan"));
    insta::assert_snapshot!("explain_request", all_requests(&provider));
}

#[tokio::test(flavor = "current_thread")]
async fn explain_of_a_write_skips_the_plan() {
    let source = SqliteSource::new("explain-write", None).await;
    let provider = scripted(
        false,
        false,
        vec![answer("It deletes users."), answer("Two.")],
    );
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let result = assist
        .explain("db", "delete from users where id = 1", true, &mut conv)
        .await
        .unwrap();
    assert!(!result.plan_included);
    let result = assist
        .explain("db", "select 1; delete from users", true, &mut conv)
        .await
        .unwrap();
    assert!(!result.plan_included);
    assert_eq!(
        source.executed(),
        Vec::<String>::new(),
        "{:?}",
        source.executed()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explain_drops_a_plan_that_is_too_big() {
    let source = SqliteSource::new("explain-big", None).await;
    let provider = scripted(false, false, vec![answer("ok")]);
    // Total budget (2200 - 1024) * 3 = 3528 bytes; a quarter is 882, a plan is larger.
    let assist = engine(&source, &provider)
        .with_options(sqler_ai_tools::AssistOptions::default().with_context_tokens(2300));
    let mut conv = Conversation::new();
    let sql = "select * from users a join users b on b.name = a.name \
               join users c on c.name = a.email join users d on d.name = b.email \
               join users e on e.name = c.email join users f on f.name = d.email \
               join orders o on o.total = f.id join orders p on p.total = o.id";
    match assist.explain("db", sql, true, &mut conv).await {
        Ok(result) => assert!(!result.plan_included),
        Err(other) => panic!("{other}"),
    }
    assert!(!all_requests(&provider).contains("Query plan"));
    assert!(!source.executed().is_empty(), "the plan was asked for");
}

#[tokio::test(flavor = "current_thread")]
async fn fix_returns_a_diff_and_runs_nothing() {
    let source = SqliteSource::new("fix", None).await;
    let provider = scripted(
        false,
        false,
        vec![answer(
            "```sql\nselect name\nfrom users\n```\nThe column is `name`.",
        )],
    );
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let proposal = assist
        .fix(
            "db",
            "select nme\nfrom users",
            "no such column: nme",
            &mut conv,
        )
        .await
        .unwrap();
    assert_eq!(proposal.original, "select nme\nfrom users");
    assert_eq!(proposal.sql, "select name\nfrom users");
    assert_eq!(
        proposal.diff,
        "--- original\n+++ fixed\n@@ -1,2 +1,2 @@\n-select nme\n+select name\n from users\n"
    );
    assert!(!proposal.unchanged);
    assert!(proposal.applicable);
    assert_eq!(proposal.prompt_version, PROMPT_VERSION);
    assert_eq!(source.executed(), Vec::<String>::new());
    assert_eq!(source.acquire_count(), 0);
    let prompt = all_requests(&provider);
    assert_inside_envelopes(&prompt, "no such column");
    insta::assert_snapshot!("fix_request", prompt);
}

#[tokio::test(flavor = "current_thread")]
async fn an_unchanged_or_redacted_fix_is_not_applicable() {
    let source = SqliteSource::new("fix-flags", None).await;
    let provider = scripted(
        false,
        false,
        vec![
            answer("```sql\nselect * from users\n```\nNothing to change."),
            answer("```sql\nselect * from users where email = '<redacted>'\n```"),
        ],
    );
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let same = assist
        .fix("db", "select * from users", "disk I/O error", &mut conv)
        .await
        .unwrap();
    assert!(same.unchanged);
    assert!(!same.applicable);
    assert_eq!(same.diff, "");
    let masked = assist
        .fix(
            "db",
            "select * from users",
            "no such column: email",
            &mut conv,
        )
        .await
        .unwrap();
    assert!(!masked.unchanged);
    assert!(!masked.applicable);
}

#[tokio::test(flavor = "current_thread")]
async fn fix_rejects_empty_input() {
    let source = SqliteSource::new("fix-empty", None).await;
    let provider = scripted(false, false, vec![]);
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    for (sql, error) in [("", "e"), ("select 1", " ")] {
        let err = assist.fix("db", sql, error, &mut conv).await.unwrap_err();
        assert!(matches!(err, AssistError::InvalidInput(_)), "{err}");
    }
    assert!(provider.requests().is_empty());
    assert!(render(&sqler_ai_api::ChatRequest::new("m", vec![])).contains("model: m"));
}

#[tokio::test(flavor = "current_thread")]
async fn a_5_kib_statement_in_an_8_kib_window_stays_inside_it() {
    let source = SqliteSource::new("explain-window", None).await;
    let provider = scripted(false, false, vec![answer("A long scan.")]);
    // 8 KiB of input room: (256 + 2731 - 256) tokens * 3 bytes.
    let assist = engine(&source, &provider).with_options(
        AssistOptions::default()
            .with_context_tokens(256 + 2731)
            .with_max_output_tokens(256),
    );
    let sql = format!("select '{}' from users", "x".repeat(5 * 1024));
    let mut conv = Conversation::new();
    match assist.explain("db", &sql, false, &mut conv).await {
        Err(AssistError::TooLarge(_)) => {}
        Ok(_) => {
            let requests = provider.requests();
            assert_eq!(requests.len(), 1);
            let r = &requests[0];
            let bytes: usize = r.system.as_deref().map_or(0, str::len)
                + r.messages
                    .iter()
                    .flat_map(sqler_ai_api::Message::parts)
                    .map(|p| match p {
                        sqler_ai_api::Part::Text(t) => t.len(),
                        _ => 0,
                    })
                    .sum::<usize>();
            assert!(bytes <= 8 * 1024, "{bytes}");
        }
        Err(other) => panic!("{other:?}"),
    }
}
