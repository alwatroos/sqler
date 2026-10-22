// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! NL to SQL: grounding on a large catalog, capability modes and answer shapes (AC2).
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod assist_support;
mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use assist_support::{
    GENERATED, SyntheticSource, all_requests, answer, render, scripted, tool_call,
};
use sqler_ai_api::mock::ScriptedProvider;
use sqler_ai_api::{ResponseFormat, Role};
use sqler_ai_tools::{
    Assist, AssistError, AssistOptions, Conversation, Limits, PROMPT_VERSION, ToolSet,
};
use sqler_driver_api::sqler_core::{ColumnInfo, DataType};

fn engine(source: &Arc<SyntheticSource>, provider: &Arc<ScriptedProvider>) -> Assist {
    let tools = ToolSet::new(source.clone(), Limits::default());
    Assist::new(tools, provider.clone(), "test-model")
}

/// The distinct generated table names (`t` and four digits) in `text`.
fn generated_names(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut out = BTreeSet::new();
    for i in 0..bytes.len().saturating_sub(4) {
        let word = &bytes[i..i + 5];
        let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let after_ok = bytes.get(i + 5).is_none_or(|b| !b.is_ascii_alphanumeric());
        if before_ok && after_ok && word[0] == b't' && word[1..].iter().all(u8::is_ascii_digit) {
            out.insert(String::from_utf8_lossy(word).into_owned());
        }
    }
    out
}

#[tokio::test(flavor = "current_thread")]
async fn five_thousand_tables_ground_to_at_most_eight() {
    let source = SyntheticSource::new();
    let provider = scripted(
        false,
        false,
        vec![answer(
            "```sql\nSELECT * FROM customers\n```\nAll customers.",
        )],
    );
    let assist = engine(&source, &provider);
    let question = "compare customers with the t00 tables";
    let grounding = assist.ground("syn", question).await.unwrap();
    assert!(grounding.tables.len() <= 8, "{:?}", grounding.tables);
    assert!(grounding.tables.iter().any(|t| t.name == "customers"));
    assert_eq!(grounding.bytes, grounding.text.len());
    // Default window 8192 tokens, 1024 for the answer: (8192 - 1024) * 3 / 2.
    assert!(grounding.bytes <= 10_752, "{}", grounding.bytes);
    assert!(grounding.terms.contains(&"customers".to_owned()));

    let mut conv = Conversation::new();
    let result = assist.nl_to_sql("syn", question, &mut conv).await.unwrap();
    assert_eq!(result.sql, "SELECT * FROM customers");
    assert_eq!(result.rationale, "All customers.");
    assert_eq!(result.prompt_version, PROMPT_VERSION);
    assert!(result.tables.len() <= 8);

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let prompt = render(&requests[0]);
    let user = requests[0].messages.last().unwrap().text();
    assert!(
        user.matches(sqler_ai_tools::UNTRUSTED_OPEN).count() <= 8,
        "at most eight describe_table envelopes"
    );
    assert!(prompt.contains("customers"));
    assert!(
        generated_names(&prompt).len() <= 8,
        "{:?}",
        generated_names(&prompt)
    );
    const { assert!(GENERATED > 8) };
    assert_eq!(conv.len(), 1);
    // The engine ran no SQL and the answer is only text.
    assert_eq!(source.acquires.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_small_context_truncates_grounding() {
    let source = SyntheticSource::new();
    let provider = scripted(false, false, vec![answer("```sql\nSELECT 1\n```")]);
    let assist =
        engine(&source, &provider).with_options(AssistOptions::default().with_context_tokens(3000));
    let grounding = assist
        .ground("syn", "compare customers with the t00 tables")
        .await
        .unwrap();
    // (3000 - 1024) * 3 / 2 = 2964 bytes.
    assert!(grounding.bytes <= 2964, "{}", grounding.bytes);
    assert!(grounding.truncated);
    assert!(!grounding.tables.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn an_oversized_table_does_not_end_grounding() {
    // `awidezz` ranks first but alone exceeds the budget; `bsmallzz` still fits.
    let wide: Vec<ColumnInfo> = (0..400)
        .map(|i| ColumnInfo::new(format!("column_number_{i:03}"), DataType::Integer))
        .collect();
    let small = vec![ColumnInfo::new("id", DataType::Integer)];
    let source = SyntheticSource::with(vec![("awidezz", wide), ("bsmallzz", small)]);
    let provider = scripted(false, false, vec![answer("```sql\nSELECT 1\n```")]);
    let assist =
        engine(&source, &provider).with_options(AssistOptions::default().with_context_tokens(3000));
    let grounding = assist.ground("syn", "awidezz bsmallzz").await.unwrap();
    assert!(grounding.truncated);
    assert!(grounding.tables.iter().any(|t| t.name == "bsmallzz"));
    assert!(grounding.tables.iter().all(|t| t.name != "awidezz"));
    assert!(grounding.bytes <= 2964, "{}", grounding.bytes);
}

#[tokio::test(flavor = "current_thread")]
async fn too_large_requests_are_refused_before_the_provider() {
    let source = SyntheticSource::new();
    let provider = scripted(false, false, vec![]);
    let assist =
        engine(&source, &provider).with_options(AssistOptions::default().with_context_tokens(1200));
    let mut conv = Conversation::new();
    let err = assist
        .nl_to_sql("syn", "customers", &mut conv)
        .await
        .unwrap_err();
    assert!(matches!(err, AssistError::TooLarge(_)), "{err}");
    assert!(provider.requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn input_errors() {
    let source = SyntheticSource::new();
    let provider = scripted(false, false, vec![]);
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    assert!(matches!(
        assist.nl_to_sql("nope", "customers", &mut conv).await,
        Err(AssistError::UnknownConnection(_))
    ));
    assert!(matches!(
        assist.nl_to_sql("syn", "  ", &mut conv).await,
        Err(AssistError::InvalidInput(_))
    ));
    assert!(matches!(
        assist.ground("syn", "").await,
        Err(AssistError::InvalidInput(_))
    ));
    assert!(provider.requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn tools_mode_runs_the_model_requested_call() {
    let source = SyntheticSource::new();
    let provider = scripted(
        true,
        true,
        vec![
            tool_call(
                "c1",
                "search_schema",
                r#"{"connection":"syn","query":"orders"}"#,
            ),
            answer("```sql\nSELECT total FROM orders\n```"),
        ],
    );
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let result = assist
        .nl_to_sql("syn", "what do orders total", &mut conv)
        .await
        .unwrap();
    assert_eq!(result.sql, "SELECT total FROM orders");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let names: Vec<&str> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["describe_table", "search_schema"]);
    // JSON mode only in a round without tools.
    assert_eq!(requests[0].response_format, ResponseFormat::Text);
    let second = &requests[1];
    assert_eq!(second.messages.last().unwrap().role(), Role::Tool);
    assert!(render(second).contains(sqler_ai_tools::UNTRUSTED_OPEN));
}

#[tokio::test(flavor = "current_thread")]
async fn a_tool_outside_the_pair_runs_nothing() {
    let source = SyntheticSource::new();
    let provider = scripted(
        true,
        false,
        vec![tool_call(
            "c1",
            "run_query",
            r#"{"connection":"syn","sql":"delete from t"}"#,
        )],
    );
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let err = assist
        .nl_to_sql("syn", "customers", &mut conv)
        .await
        .unwrap_err();
    assert!(
        matches!(err, AssistError::UnexpectedTool(ref n) if n == "run_query"),
        "{err}"
    );
    assert_eq!(source.acquires.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(conv.is_empty());

    let provider = scripted(
        true,
        false,
        vec![tool_call(
            "c1",
            "search_schema",
            r#"{"connection":"other","query":"x"}"#,
        )],
    );
    let err = engine(&source, &provider)
        .nl_to_sql("syn", "customers", &mut conv)
        .await;
    assert!(matches!(err, Err(AssistError::UnexpectedTool(_))));
}

#[tokio::test(flavor = "current_thread")]
async fn endless_tool_requests_end_in_a_tool_loop_error() {
    let source = SyntheticSource::new();
    let call = || {
        tool_call(
            "c",
            "search_schema",
            r#"{"connection":"syn","query":"orders"}"#,
        )
    };
    let provider = scripted(true, false, vec![call(), call(), call()]);
    let assist =
        engine(&source, &provider).with_options(AssistOptions::default().with_max_tool_rounds(1));
    let mut conv = Conversation::new();
    let err = assist
        .nl_to_sql("syn", "customers", &mut conv)
        .await
        .unwrap_err();
    assert!(matches!(err, AssistError::ToolLoop), "{err}");
    // One offered round, then one request without tools.
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].tools.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn json_mode_and_json_answers() {
    let source = SyntheticSource::new();
    let provider = scripted(
        false,
        true,
        vec![answer(
            r#"{"sql":"SELECT id FROM orders","rationale":"ids"}"#,
        )],
    );
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let result = assist
        .nl_to_sql("syn", "order ids", &mut conv)
        .await
        .unwrap();
    assert_eq!(
        (result.sql.as_str(), result.rationale.as_str()),
        ("SELECT id FROM orders", "ids")
    );
    let request = &provider.requests()[0];
    assert_eq!(request.response_format, ResponseFormat::Json);
    assert!(request.tools.is_empty());
    assert!(request.system.as_deref().unwrap().contains("JSON object"));
}

#[tokio::test(flavor = "current_thread")]
async fn bare_sql_and_prose_answers() {
    let source = SyntheticSource::new();
    let provider = scripted(
        false,
        false,
        vec![
            answer("Try this:\nselect id\nfrom orders\n\nIt lists ids."),
            answer("I do not know."),
        ],
    );
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let result = assist
        .nl_to_sql("syn", "order ids", &mut conv)
        .await
        .unwrap();
    assert_eq!(result.sql, "select id\nfrom orders");
    let err = assist
        .nl_to_sql("syn", "order ids", &mut conv)
        .await
        .unwrap_err();
    assert!(matches!(err, AssistError::NoSql));
    assert_eq!(conv.len(), 1, "a failed call leaves no turn");
}

#[tokio::test(flavor = "current_thread")]
async fn history_is_sent_and_capped() {
    let source = SyntheticSource::new();
    let provider = scripted(
        false,
        false,
        (0..4)
            .map(|i| answer(&format!("```sql\nSELECT {i}\n```")))
            .collect(),
    );
    let assist =
        engine(&source, &provider).with_options(AssistOptions::default().with_max_turns(2));
    let mut conv = Conversation::new();
    for _ in 0..4 {
        assist
            .nl_to_sql("syn", "order ids", &mut conv)
            .await
            .unwrap();
    }
    assert_eq!(conv.len(), 2);
    assert_eq!(conv.turns()[1].answer, "```sql\nSELECT 3\n```");
    let counts: Vec<usize> = provider
        .requests()
        .iter()
        .map(|r| r.messages.len())
        .collect();
    assert_eq!(counts, [1, 3, 5, 5]);
    let last = &provider.requests()[3];
    assert_eq!(last.messages[0].role(), Role::User);
    assert_eq!(last.messages[1].role(), Role::Assistant);
    assert!(!render(last).contains("Schema context (tool results):\nSchema"));
}

#[tokio::test(flavor = "current_thread")]
async fn snapshot_of_the_rendered_request() {
    let source = SyntheticSource::new();
    let provider = scripted(false, false, vec![answer("```sql\nSELECT 1\n```")]);
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    assist
        .nl_to_sql("syn", "How many orders does each customer have?", &mut conv)
        .await
        .unwrap();
    insta::assert_snapshot!("nl_to_sql_request", all_requests(&provider));
}

#[tokio::test(flavor = "current_thread")]
async fn futures_are_send() {
    fn is_send<T: Send>(_: &T) {}
    let source = SyntheticSource::new();
    let provider = scripted(false, false, vec![]);
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    is_send(&assist.nl_to_sql("syn", "q", &mut conv));
    is_send(&assist.explain("syn", "select 1", true, &mut conv));
    is_send(&assist.fix("syn", "select 1", "e", &mut conv));
    is_send(&assist.ground("syn", "q"));
}

#[tokio::test(flavor = "current_thread")]
async fn the_applicable_flag_is_false_for_a_redacted_answer() {
    let source = SyntheticSource::new();
    let provider = scripted(
        false,
        false,
        vec![
            answer("```sql\nSELECT * FROM customers WHERE token = '<redacted>'\n```\nBy token."),
            answer("```sql\nSELECT * FROM customers\n```\nAll."),
        ],
    );
    let assist = engine(&source, &provider);
    let mut conv = Conversation::new();
    let redacted = assist
        .nl_to_sql("syn", "customers by token", &mut conv)
        .await
        .unwrap();
    assert!(redacted.sql.contains("<redacted>"), "{}", redacted.sql);
    assert!(!redacted.applicable);
    let plain = assist
        .nl_to_sql("syn", "all customers", &mut conv)
        .await
        .unwrap();
    assert!(plain.applicable);
}
