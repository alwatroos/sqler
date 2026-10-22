// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Approvals through the tool set: the session reports waits and never decides.
#![allow(clippy::unwrap_used)]

mod common;
mod session_common;

use common::{ScriptedApprover, SqliteSource};
use session_common::{approver, provider, session, text, tool_entries, tools, with_events};
use sqler_ai_api::Part;
use sqler_ai_tools::session::{
    SessionEvent, SessionLimits, SessionState, ToolCallStatus, TurnOutcome,
};
use sqler_ai_tools::{ApprovalStage, Decision};
use sqler_driver_api::sqler_core::Environment;

const INSERT: &str =
    r#"{"connection":"db","sql":"insert into users (id, name) values (9, 'zed')"}"#;

#[tokio::test(flavor = "current_thread")]
async fn an_approved_write_runs() {
    let source = SqliteSource::new("appr", None).await;
    let a = ScriptedApprover::always(Decision::Approve);
    let p = provider();
    p.push_script(tools(&[("c1", "run_query", INSERT)]));
    p.push_script(text("inserted"));
    let (mut s, events) = with_events(session(&source, p, approver(&a), SessionLimits::default()));
    assert_eq!(s.send("add zed").await, TurnOutcome::Completed);
    assert_eq!(a.requests().len(), 1);
    assert_eq!(source.executed().len(), 1);
    assert!(s.transcript().sql().next().unwrap().approved);

    let seen: Vec<SessionEvent> = events
        .events()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                SessionEvent::State(SessionState::AwaitingApproval | SessionState::ToolCallPending)
                    | SessionEvent::ApprovalRequested(_)
                    | SessionEvent::ApprovalResolved(_)
            )
        })
        .collect();
    let names: Vec<&str> = seen
        .iter()
        .map(|e| match e {
            SessionEvent::State(SessionState::AwaitingApproval) => "awaiting",
            SessionEvent::State(_) => "pending",
            SessionEvent::ApprovalRequested(_) => "requested",
            SessionEvent::ApprovalResolved(Some(Decision::Approve)) => "approved",
            _ => "other",
        })
        .collect();
    // The tool call state comes first; then the wait, the request, the answer, back to pending.
    assert_eq!(
        names,
        ["pending", "awaiting", "requested", "approved", "pending"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_denied_write_runs_nothing_and_the_model_hears_it() {
    let source = SqliteSource::new("deny", None).await;
    let a = ScriptedApprover::always(Decision::Deny);
    let p = provider();
    p.push_script(tools(&[("c1", "run_query", INSERT)]));
    p.push_script(text("understood, I will not insert"));
    let mut s = session(&source, p.clone(), approver(&a), SessionLimits::default());
    assert_eq!(s.send("add zed").await, TurnOutcome::Completed);
    assert_eq!(source.executed().len(), 0);
    assert_eq!(s.transcript().sql().count(), 0);
    let calls = tool_entries(s.transcript());
    assert!(matches!(
        &calls[0].status,
        ToolCallStatus::Error { code, .. } if code == "denied"
    ));
    let result = p.requests()[1]
        .messages
        .iter()
        .flat_map(|m| m.parts().iter())
        .find_map(|part| match part {
            Part::ToolResult(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("denied"));
}

#[tokio::test(flavor = "current_thread")]
async fn without_an_approver_the_tool_set_refuses() {
    let source = SqliteSource::new("noappr", None).await;
    let p = provider();
    p.push_script(tools(&[("c1", "run_query", INSERT)]));
    p.push_script(text("refused"));
    let mut s = session(&source, p, None, SessionLimits::default());
    assert_eq!(s.send("add zed").await, TurnOutcome::Completed);
    assert_eq!(source.executed().len(), 0);
    assert!(matches!(
        &tool_entries(s.transcript())[0].status,
        ToolCallStatus::Error { code, .. } if code == "no_approver"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn prod_asks_twice_and_a_second_denial_runs_nothing() {
    let source = SqliteSource::new("prod", Some(Environment::Prod)).await;
    let a = ScriptedApprover::new(|r| {
        if r.stage == ApprovalStage::ProdSecond {
            Decision::Deny
        } else {
            Decision::Approve
        }
    });
    let p = provider();
    p.push_script(tools(&[("c1", "run_query", INSERT)]));
    p.push_script(text("stopped"));
    let (mut s, events) = with_events(session(&source, p, approver(&a), SessionLimits::default()));
    assert_eq!(s.send("add zed").await, TurnOutcome::Completed);
    let stages: Vec<ApprovalStage> = events
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::ApprovalRequested(r) => Some(r.stage),
            _ => None,
        })
        .collect();
    assert_eq!(stages, [ApprovalStage::Confirm, ApprovalStage::ProdSecond]);
    assert_eq!(source.executed().len(), 0);
    assert!(matches!(
        &tool_entries(s.transcript())[0].status,
        ToolCallStatus::Error { code, .. } if code == "denied"
    ));
}
