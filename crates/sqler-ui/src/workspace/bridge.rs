// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SessionBridge`: runs driver work on tokio and hands the result to the UI thread.
//!
//! The UI thread has no tokio runtime context, so work is spawned on a stored
//! [`Handle`] and the result comes back on a `tokio::sync::oneshot`,
//! which can be awaited on any executor (for `GPUI`: its foreground executor).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use sqler_driver_api::sqler_core::ConnectionConfig;
use sqler_driver_api::{DriverError, DriverRegistry, Session};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

/// Why a connect attempt produced no session.
#[derive(Debug, Error)]
pub enum ConnectError {
    /// The driver (or the registry) failed.
    #[error(transparent)]
    Driver(#[from] DriverError),
    /// The attempt was aborted before it finished.
    #[error("connect aborted")]
    Aborted,
}

/// Opens and closes sessions on the tokio runtime.
#[derive(Debug, Clone)]
pub struct SessionBridge {
    registry: Arc<DriverRegistry>,
    handle: Handle,
}

impl SessionBridge {
    /// Creates a bridge over a registry and a runtime handle.
    #[must_use]
    pub fn new(registry: Arc<DriverRegistry>, handle: Handle) -> Self {
        Self { registry, handle }
    }

    /// Starts connecting. Dropping the returned future aborts the attempt.
    #[must_use]
    pub fn connect(&self, config: ConnectionConfig) -> PendingConnect {
        let (tx, rx) = oneshot::channel();
        let registry = Arc::clone(&self.registry);
        let handle = self.handle.clone();
        let task = self.handle.spawn(async move {
            let result = registry.connect(&config).await;
            if let Err(Ok(session)) = tx.send(result) {
                // Nobody is waiting any more: do not leak the session.
                close_logged(session).await;
            }
        });
        PendingConnect {
            rx,
            abort: task.abort_handle(),
            handle,
        }
    }

    /// Closes a session in the background; a failure is reported on stderr.
    pub fn close(&self, session: Box<dyn Session>) {
        self.handle.spawn(close_logged(session));
    }
}

async fn close_logged(session: Box<dyn Session>) {
    if let Err(err) = session.close().await {
        eprintln!("sqler: failed to close a session: {err}");
    }
}

/// A connect attempt in flight. Awaiting it never blocks a thread.
pub struct PendingConnect {
    rx: oneshot::Receiver<Result<Box<dyn Session>, DriverError>>,
    abort: AbortHandle,
    handle: Handle,
}

impl std::fmt::Debug for PendingConnect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingConnect").finish_non_exhaustive()
    }
}

impl Future for PendingConnect {
    type Output = Result<Box<dyn Session>, ConnectError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(result)) => Poll::Ready(result.map_err(ConnectError::Driver)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(ConnectError::Aborted)),
        }
    }
}

impl Drop for PendingConnect {
    fn drop(&mut self) {
        self.abort.abort();
        // A result that arrived but was never taken still owns a session: close it.
        self.rx.close();
        if let Ok(Ok(session)) = self.rx.try_recv() {
            self.handle.spawn(close_logged(session));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::workspace::testing::FakeSession;
    use sqler_driver_api::sqler_core::DriverId;
    use sqler_driver_api::{BoxFuture, Driver};

    /// `Ok` driver: connects immediately, hands out its `closed` flag.
    struct OkDriver {
        created: Arc<AtomicBool>,
        closed: Arc<AtomicBool>,
    }

    impl Driver for OkDriver {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn connect<'a>(
            &'a self,
            _config: &'a ConnectionConfig,
        ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
            let session = FakeSession {
                closed: self.closed.clone(),
            };
            self.created.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(Box::new(session) as Box<dyn Session>) })
        }
    }

    /// Driver that never finishes connecting and notes when its future is dropped.
    struct HangDriver {
        started: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl Driver for HangDriver {
        fn id(&self) -> &'static str {
            "hang"
        }
        fn connect<'a>(
            &'a self,
            _config: &'a ConnectionConfig,
        ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
            let guard = DropFlag(self.dropped.clone());
            self.started.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
                Err(DriverError::Cancelled)
            })
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn cfg(driver: &str) -> ConnectionConfig {
        ConnectionConfig::new("c", DriverId::new(driver).unwrap(), "x")
    }

    fn wait_until(flag: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !flag.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "timed out waiting for flag");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn bridge(rt: &tokio::runtime::Runtime, registry: DriverRegistry) -> SessionBridge {
        SessionBridge::new(Arc::new(registry), rt.handle().clone())
    }

    #[test]
    fn connect_ok_and_close() {
        let rt = runtime();
        let created = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let mut registry = DriverRegistry::new();
        registry
            .register(Box::new(OkDriver {
                created: created.clone(),
                closed: closed.clone(),
            }))
            .unwrap();
        let bridge = bridge(&rt, registry);
        let session = rt.block_on(bridge.connect(cfg("fake"))).unwrap();
        assert!(created.load(Ordering::SeqCst));
        bridge.close(session);
        wait_until(&closed);
    }

    #[test]
    fn unknown_driver_is_a_driver_error() {
        let rt = runtime();
        let bridge = bridge(&rt, DriverRegistry::new());
        let err = rt.block_on(bridge.connect(cfg("nope"))).err().unwrap();
        assert!(matches!(
            err,
            ConnectError::Driver(DriverError::UnknownDriver(_))
        ));
        assert_eq!(err.to_string(), "unknown driver: nope");
    }

    #[test]
    fn dropping_pending_aborts_the_task() {
        let rt = runtime();
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let mut registry = DriverRegistry::new();
        registry
            .register(Box::new(HangDriver {
                started: started.clone(),
                dropped: dropped.clone(),
            }))
            .unwrap();
        let bridge = bridge(&rt, registry);
        let pending = bridge.connect(cfg("hang"));
        wait_until(&started);
        assert!(!dropped.load(Ordering::SeqCst));
        drop(pending);
        wait_until(&dropped);
    }

    #[test]
    fn dropping_pending_never_leaks_a_created_session() {
        let rt = runtime();
        let created = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let mut registry = DriverRegistry::new();
        registry
            .register(Box::new(OkDriver {
                created: created.clone(),
                closed: closed.clone(),
            }))
            .unwrap();
        let bridge = bridge(&rt, registry);
        let pending = bridge.connect(cfg("fake"));
        // Either the task was aborted before connecting, or the session was created and must
        // be closed whichever side of the oneshot it landed on.
        drop(pending);
        std::thread::sleep(Duration::from_millis(100));
        if created.load(Ordering::SeqCst) {
            wait_until(&closed);
        }
    }
}
