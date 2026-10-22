// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Limits, loop detection and provider errors.
#![allow(clippy::unwrap_used)]

mod common;
mod session_common;

use std::time::Duration;

use common::SqliteSource;
use session_common::{
    Move, TestProvider, assert_well_formed, events, notices, provider, session, text, tool_entries,
    tools, with_events,
};
use sqler_ai_api::{AiError, ChatEvent, StopReason, Usage};
use sqler_ai_tools::session::{
    LimitKind, NotRunReason, Notice, SessionError, SessionEvent, SessionLimits, SessionState,
    ToolCallStatus, TranscriptEntry, TurnOutcome,
};

const LIST: &str = r#"{"connection":"db"}"#;

fn fast_retry(limits: &mut SessionLimits) {
    limits.retry.base_delay = Duration::from_millis(1);
    limits.retry.max_delay = Duration::from_millis(2);
}

#[tokio::test(flavor = "current_thread")]
async fn model_steps_are_bounded() {
    let source = SqliteSource::new("steps", None).await;
    let p = provider();
    p.push_script(tools(&[("c1", "list_tables", LIST)]));
    p.push_script(tools(&[(
        "c2",
        "describe_table",
        r#"{"connection":"db","name":"users"}"#,
    )]));
    let mut limits = SessionLimits::default();
    limits.max_model_steps = 2;
    let mut s = session(&source, p.clone(), None, limits);
    assert_eq!(
        s.send("go").await,
        TurnOutcome::LimitReached(LimitKind::ModelSteps)
    );
    assert_eq!(p.requests().len(), 2);
    assert!(notices(s.transcript()).contains(&Notice::LimitReached(LimitKind::ModelSteps)));
    assert_eq!(s.state(), SessionState::Done);
}

#[tokio::test(flavor = "current_thread")]
async fn excess_tool_calls_are_not_run_and_not_in_the_history() {
    let source = SqliteSource::new("calls", None).await;
    let p = provider();
    p.push_script(tools(&[
        ("c1", "list_tables", LIST),
        (
            "c2",
            "describe_table",
            r#"{"connection":"db","name":"users"}"#,
        ),
        (
            "c3",
            "describe_table",
            r#"{"connection":"db","name":"orders"}"#,
        ),
        ("c4", "list_schemas", LIST),
    ]));
    p.push_script(text("ok"));
    let mut limits = SessionLimits::default();
    limits.max_tool_calls = 2;
    let mut s = session(&source, p.clone(), None, limits);
    assert_eq!(
        s.send("go").await,
        TurnOutcome::LimitReached(LimitKind::ToolCalls)
    );
    let statuses: Vec<ToolCallStatus> = tool_entries(s.transcript())
        .into_iter()
        .map(|c| c.status)
        .collect();
    let not_run = ToolCallStatus::NotRun(NotRunReason::Limit(LimitKind::ToolCalls));
    assert_eq!(
        statuses,
        [
            ToolCallStatus::Ok,
            ToolCallStatus::Ok,
            not_run.clone(),
            not_run
        ]
    );
    // The next turn's request has only the calls that ran, each with its result.
    assert_eq!(s.send("again").await, TurnOutcome::Completed);
    let messages = &p.requests()[1].messages;
    assert_well_formed(messages);
    let ids: Vec<String> = messages
        .iter()
        .flat_map(|m| m.parts().iter())
        .filter_map(|part| match part {
            sqler_ai_api::Part::ToolCall(c) => Some(c.id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["c1", "c2"]);
}

#[tokio::test(flavor = "current_thread")]
async fn the_token_budget_cuts_a_stream() {
    let source = SqliteSource::new("tokens", None).await;
    let p = provider();
    p.push_script(vec![
        Ok(ChatEvent::TextDelta("par".into())),
        Ok(ChatEvent::Usage(Usage::new(8, 5))),
        Ok(ChatEvent::TextDelta("tial".into())),
        Ok(ChatEvent::Done {
            stop: StopReason::EndTurn,
        }),
    ]);
    let mut limits = SessionLimits::default();
    limits.token_budget = 10;
    let (mut s, events) = with_events(session(&source, p.clone(), None, limits));
    assert_eq!(
        s.send("go").await,
        TurnOutcome::LimitReached(LimitKind::Tokens)
    );
    assert_eq!(p.open_streams(), 0);
    assert_eq!(s.usage(), Usage::new(8, 5));
    assert!(events.events().iter().any(|e| matches!(
        e,
        SessionEvent::Usage { session, .. } if *session == Usage::new(8, 5)
    )));
    assert!(
        s.transcript()
            .entries()
            .contains(&TranscriptEntry::Assistant { text: "par".into() })
    );
    // The budget covers the session: the next turn does not start a model step.
    assert_eq!(
        s.send("more").await,
        TurnOutcome::LimitReached(LimitKind::Tokens)
    );
    assert_eq!(p.requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn the_turn_time_is_bounded() {
    let source = SqliteSource::new("time", None).await;
    let p = TestProvider::new(vec![vec![Move::Pend]]);
    let mut limits = SessionLimits::default();
    limits.turn_timeout = Duration::from_millis(30);
    let mut s = session(&source, p.clone(), None, limits);
    assert_eq!(
        s.send("go").await,
        TurnOutcome::LimitReached(LimitKind::Time)
    );
    assert_eq!(p.open_streams(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_repeated_call_is_a_loop() {
    let source = SqliteSource::new("loop", None).await;
    let p = provider();
    for i in 0..3 {
        p.push_script(tools(&[(&format!("c{i}"), "list_tables", LIST)]));
    }
    let mut s = session(&source, p.clone(), None, SessionLimits::default());
    assert_eq!(s.send("go").await, TurnOutcome::LoopDetected);
    assert_eq!(p.requests().len(), 3);
    let calls = tool_entries(s.transcript());
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[2].status,
        ToolCallStatus::NotRun(NotRunReason::Repeated)
    );
    assert!(notices(s.transcript()).contains(&Notice::LoopDetected {
        tool: "list_tables".into()
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn rate_limits_are_retried_then_succeed() {
    let source = SqliteSource::new("retry", None).await;
    let p = provider();
    p.push_script(vec![Err(AiError::RateLimited {
        retry_after: Some(Duration::from_millis(1)),
    })]);
    p.push_script(text("ok"));
    let mut limits = SessionLimits::default();
    fast_retry(&mut limits);
    let mut s = session(&source, p.clone(), None, limits);
    assert_eq!(s.send("go").await, TurnOutcome::Completed);
    assert_eq!(p.requests().len(), 2);
    assert_eq!(
        notices(s.transcript()),
        [Notice::Retrying {
            attempt: 1,
            delay: Duration::from_millis(1)
        }]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn overloaded_is_retried_then_succeeds() {
    let source = SqliteSource::new("overloaded", None).await;
    let p = provider();
    p.push_script(vec![Err(AiError::Overloaded {
        retry_after: Some(Duration::from_millis(1)),
    })]);
    p.push_script(text("ok"));
    let mut limits = SessionLimits::default();
    fast_retry(&mut limits);
    let mut s = session(&source, p.clone(), None, limits);
    assert_eq!(s.send("go").await, TurnOutcome::Completed);
    assert_eq!(p.requests().len(), 2);
    assert_eq!(
        notices(s.transcript()),
        [Notice::Retrying {
            attempt: 1,
            delay: Duration::from_millis(1)
        }]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn auth_errors_are_not_retried() {
    let source = SqliteSource::new("auth", None).await;
    let p = provider();
    p.push_script(vec![Err(AiError::Auth("bad key".into()))]);
    p.push_script(text("never"));
    let mut s = session(&source, p.clone(), None, SessionLimits::default());
    assert_eq!(
        s.send("go").await,
        TurnOutcome::Failed(SessionError::Provider(AiError::Auth("bad key".into())))
    );
    assert_eq!(p.requests().len(), 1);
    assert_eq!(s.state(), SessionState::Failed);
    assert!(matches!(
        notices(s.transcript()).as_slice(),
        [Notice::ProviderError { .. }]
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn retries_run_out() {
    let source = SqliteSource::new("exhaust", None).await;
    let p = provider();
    for _ in 0..4 {
        p.push_script(vec![Err(AiError::Network("down".into()))]);
    }
    let mut limits = SessionLimits::default();
    fast_retry(&mut limits);
    let mut s = session(&source, p.clone(), None, limits);
    assert_eq!(
        s.send("go").await,
        TurnOutcome::Failed(SessionError::Provider(AiError::Network("down".into())))
    );
    assert_eq!(p.requests().len(), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn an_error_after_text_is_not_retried() {
    let source = SqliteSource::new("late", None).await;
    let p = provider();
    p.push_script(vec![
        Ok(ChatEvent::TextDelta("a".into())),
        Err(AiError::Network("cut".into())),
    ]);
    p.push_script(text("never"));
    let mut limits = SessionLimits::default();
    fast_retry(&mut limits);
    let mut s = session(&source, p.clone(), None, limits);
    assert!(matches!(s.send("go").await, TurnOutcome::Failed(_)));
    assert_eq!(p.requests().len(), 1);
    assert!(
        s.transcript()
            .entries()
            .contains(&TranscriptEntry::Assistant { text: "a".into() })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_stream_that_ends_early_is_a_protocol_error() {
    let source = SqliteSource::new("early", None).await;
    let p = TestProvider::new(vec![events(vec![Ok(ChatEvent::TextDelta("a".into()))])]);
    let mut s = session(&source, p, None, SessionLimits::default());
    assert_eq!(
        s.send("go").await,
        TurnOutcome::Failed(SessionError::Provider(AiError::Protocol(
            "stream ended before Done".into()
        )))
    );
}
