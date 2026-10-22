// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Fixtures of the session tests: scripted turns and a provider that can hang or cancel.

#![allow(dead_code, clippy::unwrap_used, clippy::unnecessary_wraps)]

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sqler_ai_api::mock::ScriptedProvider;
use sqler_ai_api::{
    AiError, AiProvider, BoxFuture, Capabilities, ChatEvent, ChatRequest, ChatStream, Message,
    ModelInfo, Part, Role, StopReason, ToolCall, Usage,
};
use sqler_ai_tools::session::{
    ExplorationSession, MemoryEvents, SessionCancel, SessionConfig, SessionLimits, Transcript,
    TranscriptEntry,
};
use sqler_ai_tools::{Approver, ToolSet};

use super::common::{ScriptedApprover, SqliteSource, limits};

pub type Script = Vec<Result<ChatEvent, AiError>>;

pub fn provider() -> Arc<ScriptedProvider> {
    Arc::new(
        ScriptedProvider::new("mock")
            .with_capabilities(Capabilities::new(true, false, false, None)),
    )
}

/// A text answer that ends the turn.
pub fn text(t: &str) -> Script {
    vec![
        Ok(ChatEvent::TextDelta(t.to_owned())),
        Ok(ChatEvent::Usage(Usage::new(10, 5))),
        Ok(ChatEvent::Done {
            stop: StopReason::EndTurn,
        }),
    ]
}

/// A step that asks for tools: `(id, name, raw arguments)`.
pub fn tools(calls: &[(&str, &str, &str)]) -> Script {
    let mut script: Script = calls
        .iter()
        .map(|(id, name, args)| Ok(ChatEvent::ToolCall(ToolCall::new(*id, *name, *args))))
        .collect();
    script.push(Ok(ChatEvent::Usage(Usage::new(10, 5))));
    script.push(Ok(ChatEvent::Done {
        stop: StopReason::ToolUse,
    }));
    script
}

pub fn session(
    source: &Arc<SqliteSource>,
    provider: Arc<dyn AiProvider>,
    approver: Option<Arc<dyn Approver>>,
    limits_: SessionLimits,
) -> ExplorationSession {
    let tools = ToolSet::new(source.clone(), limits());
    let mut config = SessionConfig::new("test-model");
    config.limits = limits_;
    ExplorationSession::new(provider, tools, source.info.clone(), approver, config).unwrap()
}

pub fn with_events(s: ExplorationSession) -> (ExplorationSession, Arc<MemoryEvents>) {
    let events = MemoryEvents::new();
    (s.with_events(events.clone()), events)
}

pub fn approver(a: &Arc<ScriptedApprover>) -> Option<Arc<dyn Approver>> {
    Some(a.clone())
}

/// Every call in the assistant messages has exactly one result in the tool messages.
pub fn assert_well_formed(messages: &[Message]) {
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    for m in messages {
        for p in m.parts() {
            match (m.role(), p) {
                (Role::Assistant, Part::ToolCall(c)) => {
                    calls.insert(c.id.clone());
                }
                (Role::Tool, Part::ToolResult(r)) => {
                    results.insert(r.call_id.clone());
                }
                _ => {}
            }
        }
    }
    assert_eq!(calls, results, "{messages:?}");
}

pub fn notices(t: &Transcript) -> Vec<sqler_ai_tools::session::Notice> {
    t.entries()
        .iter()
        .filter_map(|e| match e {
            TranscriptEntry::Notice(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

pub fn tool_entries(t: &Transcript) -> Vec<sqler_ai_tools::session::ToolCallEntry> {
    t.entries()
        .iter()
        .filter_map(|e| match e {
            TranscriptEntry::ToolCall(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

/// One move of a [`TestProvider`] stream.
pub enum Move {
    Event(Result<ChatEvent, AiError>),
    /// Calls the session's cancel handle.
    Cancel,
    /// Never yields again.
    Pend,
}

pub fn events(script: Script) -> Vec<Move> {
    script.into_iter().map(Move::Event).collect()
}

/// A provider whose streams can cancel the session and then hang.
pub struct TestProvider {
    plans: Mutex<VecDeque<Vec<Move>>>,
    requests: Mutex<Vec<ChatRequest>>,
    pub open: Arc<AtomicUsize>,
    pub cancel: Arc<Mutex<Option<SessionCancel>>>,
}

impl TestProvider {
    pub fn new(plans: Vec<Vec<Move>>) -> Arc<Self> {
        Arc::new(Self {
            plans: Mutex::new(plans.into()),
            requests: Mutex::new(Vec::new()),
            open: Arc::new(AtomicUsize::new(0)),
            cancel: Arc::new(Mutex::new(None)),
        })
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn open_streams(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }
}

struct TestStream {
    moves: VecDeque<Move>,
    open: Arc<AtomicUsize>,
    cancel: Arc<Mutex<Option<SessionCancel>>>,
}

impl ChatStream for TestStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>> {
        loop {
            match self.moves.pop_front() {
                Some(Move::Event(e)) => return Box::pin(std::future::ready(Some(e))),
                Some(Move::Cancel) => {
                    if let Some(c) = self.cancel.lock().unwrap().as_ref() {
                        c.cancel();
                    }
                }
                Some(Move::Pend) => return Box::pin(std::future::pending()),
                None => return Box::pin(std::future::ready(None)),
            }
        }
    }
}

impl Drop for TestStream {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AiProvider for TestProvider {
    fn id(&self) -> &'static str {
        "test"
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
        Box::pin(std::future::ready(Ok(Vec::new())))
    }

    fn stream(&self, request: ChatRequest) -> Box<dyn ChatStream + '_> {
        self.requests.lock().unwrap().push(request);
        let plan = self.plans.lock().unwrap().pop_front().unwrap_or_default();
        self.open.fetch_add(1, Ordering::SeqCst);
        Box::new(TestStream {
            moves: plan.into(),
            open: Arc::clone(&self.open),
            cancel: Arc::clone(&self.cancel),
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::new(true, false, false, None)
    }
}
