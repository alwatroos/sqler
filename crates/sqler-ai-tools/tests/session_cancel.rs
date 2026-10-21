// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Cancellation at every await, and a session that survives it.
#![allow(clippy::unwrap_used)]

mod common;
mod session_common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::SqliteSource;
use session_common::{
    Move, TestProvider, events, notices, session, text, tool_entries, tools, with_events,
};
use sqler_ai_api::{BoxFuture, ChatEvent, Role};
use sqler_ai_tools::session::{
    ExplorationSession, NotRunReason, Notice, SessionCancel, SessionConfig, SessionEvent,
    SessionLimits, SessionState, ToolCallStatus, TranscriptEntry, TurnOutcome,
};
use sqler_ai_tools::{
    ApprovalRequest, Approver, ConnectionInfo, Decision, SessionSource, ToolError, ToolSet,
};
use sqler_catalog::Catalog;
use sqler_driver_api::Session;

const INSERT: &str =
    r#"{"connection":"db","sql":"insert into users (id, name) values (9, 'zed')"}"#;

#[tokio::test(flavor = "current_thread")]
async fn cancel_mid_stream_drops_the_stream() {
    let source = SqliteSource::new("c1", None).await;
    let p = TestProvider::new(vec![vec![
        Move::Event(Ok(ChatEvent::TextDelta("hi".into()))),
        Move::Cancel,
        Move::Pend,
    ]]);
    let (mut s, events) = with_events(session(&source, p.clone(), None, SessionLimits::default()));
    *p.cancel.lock().unwrap() = Some(s.cancel_handle());
    assert_eq!(s.send("go").await, TurnOutcome::Cancelled);
    assert_eq!(p.open_streams(), 0);
    assert_eq!(s.state(), SessionState::Cancelled);
    assert!(notices(s.transcript()).contains(&Notice::Cancelled));
    assert!(
        s.transcript()
            .entries()
            .contains(&TranscriptEntry::Assistant { text: "hi".into() })
    );
    assert!(
        events
            .events()
            .contains(&SessionEvent::TurnEnded(TurnOutcome::Cancelled))
    );
}

struct CancellingApprover {
    cancel: Mutex<Option<SessionCancel>>,
}

impl Approver for CancellingApprover {
    fn approve<'a>(&'a self, _request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        if let Some(c) = self.cancel.lock().unwrap().as_ref() {
            c.cancel();
        }
        Box::pin(std::future::pending())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_while_awaiting_approval_runs_nothing() {
    let source = SqliteSource::new("c2", None).await;
    let a = Arc::new(CancellingApprover {
        cancel: Mutex::new(None),
    });
    let p = TestProvider::new(vec![events(tools(&[("c1", "run_query", INSERT)]))]);
    let (mut s, events) = with_events(session(
        &source,
        p,
        Some(a.clone() as Arc<dyn Approver>),
        SessionLimits::default(),
    ));
    *a.cancel.lock().unwrap() = Some(s.cancel_handle());
    assert_eq!(s.send("add").await, TurnOutcome::Cancelled);
    assert_eq!(source.executed().len(), 0);
    let all = events.events();
    assert!(all.contains(&SessionEvent::ApprovalResolved(None)));
    assert!(
        all.iter()
            .any(|e| matches!(e, SessionEvent::ApprovalRequested(_)))
    );
    assert_eq!(
        tool_entries(s.transcript())[0].status,
        ToolCallStatus::NotRun(NotRunReason::Cancelled)
    );
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A source whose `acquire` cancels the session and then never returns.
struct HangingSource {
    info: ConnectionInfo,
    cancel: Mutex<Option<SessionCancel>>,
    dropped: Arc<AtomicBool>,
    acquires: AtomicUsize,
}

impl SessionSource for HangingSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![self.info.clone()]
    }

    fn catalog(&self, _connection: &str) -> Option<Catalog> {
        None
    }

    fn acquire<'a>(
        &'a self,
        _connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        self.acquires.fetch_add(1, Ordering::SeqCst);
        let flag = DropFlag(Arc::clone(&self.dropped));
        if let Some(c) = self.cancel.lock().unwrap().as_ref() {
            c.cancel();
        }
        Box::pin(async move {
            let _flag = flag;
            std::future::pending::<()>().await;
            Err(ToolError::NotAvailable("unreachable".into()))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_mid_tool_drops_the_call() {
    let base = SqliteSource::new("c3", None).await;
    let source = Arc::new(HangingSource {
        info: base.info.clone(),
        cancel: Mutex::new(None),
        dropped: Arc::new(AtomicBool::new(false)),
        acquires: AtomicUsize::new(0),
    });
    let p = TestProvider::new(vec![events(tools(&[(
        "c1",
        "run_query",
        r#"{"connection":"db","sql":"select 1"}"#,
    )]))]);
    let tool_set = ToolSet::new(source.clone(), common::limits());
    let mut s = ExplorationSession::new(
        p,
        tool_set,
        source.info.clone(),
        None,
        SessionConfig::new("m"),
    )
    .unwrap();
    *source.cancel.lock().unwrap() = Some(s.cancel_handle());
    assert_eq!(s.send("go").await, TurnOutcome::Cancelled);
    assert_eq!(source.acquires.load(Ordering::SeqCst), 1);
    assert!(source.dropped.load(Ordering::SeqCst));
    assert_eq!(
        tool_entries(s.transcript())[0].status,
        ToolCallStatus::NotRun(NotRunReason::Cancelled)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_the_future_cancels_and_the_session_stays_usable() {
    let source = SqliteSource::new("c4", None).await;
    let p = TestProvider::new(vec![vec![Move::Pend], events(text("fine"))]);
    let (mut s, events) = with_events(session(&source, p.clone(), None, SessionLimits::default()));
    let timed = tokio::time::timeout(Duration::from_millis(30), s.send("first")).await;
    assert!(timed.is_err());
    assert_eq!(s.state(), SessionState::Cancelled);
    assert_eq!(p.open_streams(), 0);
    assert!(notices(s.transcript()).contains(&Notice::Cancelled));
    assert!(
        events
            .events()
            .contains(&SessionEvent::TurnEnded(TurnOutcome::Cancelled))
    );

    // The next message merges into the pending one and the turn completes.
    assert_eq!(s.send("second").await, TurnOutcome::Completed);
    let messages = &p.requests()[1].messages;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), Role::User);
    assert_eq!(messages[0].text(), "first\n\nsecond");
}

#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_session_resets_the_flag_for_the_next_turn() {
    let source = SqliteSource::new("c5", None).await;
    let p = TestProvider::new(vec![vec![Move::Cancel, Move::Pend], events(text("fine"))]);
    let mut s = session(&source, p.clone(), None, SessionLimits::default());
    *p.cancel.lock().unwrap() = Some(s.cancel_handle());
    assert_eq!(s.send("a").await, TurnOutcome::Cancelled);
    assert!(s.cancel_handle().is_cancelled());
    assert_eq!(s.send("b").await, TurnOutcome::Completed);
    assert!(!s.cancel_handle().is_cancelled());
    assert_eq!(s.state(), SessionState::Done);
}
