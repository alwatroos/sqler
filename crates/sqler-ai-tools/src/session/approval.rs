// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The approver the session installs: it reports waits and forwards decisions unchanged.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sqler_ai_api::BoxFuture;

use crate::approve::{ApprovalRequest, Approver, Decision};
use crate::session::SessionState;
use crate::session::events::{SessionEvent, SessionEvents};

pub(crate) struct SessionApprover {
    pub(crate) inner: Arc<dyn Approver>,
    pub(crate) events: Arc<dyn SessionEvents>,
    pub(crate) state: Arc<Mutex<SessionState>>,
    pub(crate) waited: Arc<Mutex<Duration>>,
}

/// Emits `ApprovalResolved(None)` when the wait is dropped before it resolved.
struct DropGuard<'a> {
    events: &'a dyn SessionEvents,
    resolved: bool,
}

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        if !self.resolved {
            self.events.emit(SessionEvent::ApprovalResolved(None));
        }
    }
}

impl SessionApprover {
    fn set_state(&self, state: SessionState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
        self.events.emit(SessionEvent::State(state));
    }
}

impl Approver for SessionApprover {
    fn approve<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            self.set_state(SessionState::AwaitingApproval);
            self.events
                .emit(SessionEvent::ApprovalRequested(request.clone()));
            let mut guard = DropGuard {
                events: &*self.events,
                resolved: false,
            };
            let started = Instant::now();
            let decision = self.inner.approve(request).await;
            {
                let mut waited = self.waited.lock().unwrap_or_else(PoisonError::into_inner);
                *waited = waited.saturating_add(started.elapsed());
            }
            guard.resolved = true;
            self.events
                .emit(SessionEvent::ApprovalResolved(Some(decision)));
            self.set_state(SessionState::ToolCallPending);
            decision
        })
    }
}
