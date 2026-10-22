// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The events a session emits to the UI.

use std::sync::{Arc, Mutex, PoisonError};

use sqler_ai_api::Usage;

use crate::approve::{ApprovalRequest, Decision};
use crate::session::TurnOutcome;
use crate::session::transcript::TranscriptEntry;

/// Where a session is in its turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionState {
    /// No turn has run yet.
    Idle,
    /// A model step is streaming.
    Streaming,
    /// A tool call is running.
    ToolCallPending,
    /// A tool call waits for the user's decision.
    AwaitingApproval,
    /// The last turn ended without error (also when a limit stopped it).
    Done,
    /// The last turn was cancelled.
    Cancelled,
    /// The last turn failed.
    Failed,
}

/// One thing a session tells the UI.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SessionEvent {
    /// The state changed.
    State(SessionState),
    /// A piece of the model's answer, as it streams.
    TextDelta(String),
    /// An entry was appended to the transcript.
    Entry(TranscriptEntry),
    /// The user must decide on a statement.
    ApprovalRequested(ApprovalRequest),
    /// The decision came back (`None`: the wait was dropped, for example by a cancel).
    ApprovalResolved(Option<Decision>),
    /// Token usage of the last step and of the whole session.
    Usage {
        /// This step.
        step: Usage,
        /// The session so far.
        session: Usage,
    },
    /// The turn ended.
    TurnEnded(TurnOutcome),
}

/// Receives session events. `emit` must not block: it runs inside the turn.
pub trait SessionEvents: Send + Sync {
    /// Takes one event.
    fn emit(&self, event: SessionEvent);
}

/// Collects events in memory, for tests and simple hosts.
#[derive(Debug, Default)]
pub struct MemoryEvents {
    events: Mutex<Vec<SessionEvent>>,
}

impl MemoryEvents {
    /// An empty collector.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Every event received so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<SessionEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl SessionEvents for MemoryEvents {
    fn emit(&self, event: SessionEvent) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }
}

struct NoEvents;

impl SessionEvents for NoEvents {
    fn emit(&self, _event: SessionEvent) {}
}

/// A forwarding sink whose target can change after the approver captured it.
pub(crate) struct EventHub {
    target: Mutex<Arc<dyn SessionEvents>>,
}

impl EventHub {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(Arc::new(NoEvents)),
        })
    }

    pub(crate) fn set(&self, events: Arc<dyn SessionEvents>) {
        *self.target.lock().unwrap_or_else(PoisonError::into_inner) = events;
    }
}

impl SessionEvents for EventHub {
    fn emit(&self, event: SessionEvent) {
        let target = Arc::clone(&self.target.lock().unwrap_or_else(PoisonError::into_inner));
        target.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_forwards_to_the_latest_target() {
        let hub = EventHub::new();
        hub.emit(SessionEvent::TextDelta("lost".into()));
        let mem = MemoryEvents::new();
        hub.set(mem.clone());
        hub.emit(SessionEvent::TextDelta("kept".into()));
        assert_eq!(mem.events(), [SessionEvent::TextDelta("kept".into())]);
    }
}
