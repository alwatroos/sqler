// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Fake connector and session shared by the workspace unit tests.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sqler_driver_api::sqler_core::{DriverId, Query};
use sqler_driver_api::{BoxFuture, CancelFlag, CancelHandle, DriverError, QueryStream, Session};

use crate::connections::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt};

/// A session whose `close` sets a flag.
pub(crate) struct FakeSession {
    pub(crate) closed: Arc<AtomicBool>,
    /// `close` never finishes.
    pub(crate) hang_on_close: bool,
}

impl FakeSession {
    pub(crate) fn boxed() -> (Box<dyn Session>, Arc<AtomicBool>) {
        let closed = Arc::new(AtomicBool::new(false));
        (
            Box::new(Self {
                closed: closed.clone(),
                hang_on_close: false,
            }),
            closed,
        )
    }

    /// A session whose `close` records the call and then hangs.
    pub(crate) fn hanging_close() -> (Box<dyn Session>, Arc<AtomicBool>) {
        let closed = Arc::new(AtomicBool::new(false));
        (
            Box::new(Self {
                closed: closed.clone(),
                hang_on_close: true,
            }),
            closed,
        )
    }
}

impl Session for FakeSession {
    fn cancel_handle(&self) -> CancelHandle {
        CancelFlag::new().handle()
    }

    fn execute<'s>(
        &'s mut self,
        _query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(async { Err(DriverError::Unsupported("test".into())) })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        self.closed.store(true, Ordering::SeqCst);
        if self.hang_on_close {
            return Box::pin(async {
                std::future::pending::<()>().await;
                Ok(())
            });
        }
        Box::pin(async { Ok(()) })
    }
}

/// Sets a flag when dropped.
pub(crate) struct DropFlag(pub(crate) Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

type Scripted = Result<Box<dyn Session>, ConnectFailure>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A [`Connector`] that plays back scripted results and records what it was asked.
///
/// With an empty script every attempt yields a [`FakeSession`]. In hang mode the attempt never
/// finishes; `started` and `dropped` show that it ran and that its future was dropped.
pub(crate) struct FakeConnector {
    script: Mutex<VecDeque<Scripted>>,
    hang: bool,
    trust_result: Mutex<Option<ConnectFailure>>,
    /// Every request passed to `connect`.
    pub(crate) requests: Mutex<Vec<ConnectRequest>>,
    /// Every key passed to `trust_host_key`.
    pub(crate) trusted: Mutex<Vec<HostKeyPrompt>>,
    /// Flags of the sessions handed out by the default (empty-script) path.
    pub(crate) closed_flags: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    pub(crate) started: Arc<AtomicBool>,
    pub(crate) dropped: Arc<AtomicBool>,
}

impl FakeConnector {
    pub(crate) fn new() -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            hang: false,
            trust_result: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            trusted: Mutex::new(Vec::new()),
            closed_flags: Arc::new(Mutex::new(Vec::new())),
            started: Arc::new(AtomicBool::new(false)),
            dropped: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn hanging() -> Self {
        Self {
            hang: true,
            ..Self::new()
        }
    }

    pub(crate) fn push(&self, result: Scripted) {
        lock(&self.script).push_back(result);
    }

    pub(crate) fn fail_trust(&self, failure: ConnectFailure) {
        *lock(&self.trust_result) = Some(failure);
    }
}

impl Connector for FakeConnector {
    fn driver_ids(&self) -> Vec<DriverId> {
        vec![DriverId::new("fake").unwrap()]
    }

    fn connect(&self, request: ConnectRequest) -> BoxFuture<'static, Scripted> {
        lock(&self.requests).push(request);
        if self.hang {
            let guard = DropFlag(self.dropped.clone());
            self.started.store(true, Ordering::SeqCst);
            return Box::pin(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
                Err(ConnectFailure::Aborted)
            });
        }
        let scripted = lock(&self.script).pop_front();
        let flags = Arc::clone(&self.closed_flags);
        let started = Arc::clone(&self.started);
        // The session is made when the future runs, like a real driver does.
        Box::pin(async move {
            started.store(true, Ordering::SeqCst);
            scripted.unwrap_or_else(|| {
                let (session, closed) = FakeSession::boxed();
                lock(&flags).push(closed);
                Ok(session)
            })
        })
    }

    fn trust_host_key(&self, key: HostKeyPrompt) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        lock(&self.trusted).push(key);
        let failure = lock(&self.trust_result).take();
        Box::pin(async move { failure.map_or(Ok(()), Err) })
    }
}
