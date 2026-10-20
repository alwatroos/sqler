// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SessionBridge`: runs connector and secret-store work on tokio and hands the result to the
//! UI thread.
//!
//! The UI thread has no tokio runtime context, so work is spawned on a stored
//! [`Handle`] and the result comes back on a `tokio::sync::oneshot`,
//! which can be awaited on any executor (for `GPUI`: its foreground executor).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use sqler_catalog::{CatalogWorker, IntrospectSource};
use sqler_config::{SecretError, SecretStore};
use sqler_driver_api::Session;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinSet};

use super::state::SecretOp;
use crate::connections::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt};

/// Opens and closes sessions and writes secrets on the tokio runtime.
#[derive(Clone)]
pub struct SessionBridge {
    connector: Arc<dyn Connector>,
    secrets: Arc<dyn SecretStore>,
    handle: Handle,
}

impl std::fmt::Debug for SessionBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionBridge").finish_non_exhaustive()
    }
}

impl SessionBridge {
    /// Creates a bridge over a connector, the secret store and a runtime handle.
    #[must_use]
    pub fn new(
        connector: Arc<dyn Connector>,
        secrets: Arc<dyn SecretStore>,
        handle: Handle,
    ) -> Self {
        Self {
            connector,
            secrets,
            handle,
        }
    }

    /// Starts connecting. Dropping the returned future aborts the attempt.
    #[must_use]
    pub fn connect(&self, request: ConnectRequest) -> PendingConnect {
        let (tx, rx) = oneshot::channel();
        let attempt = self.connector.connect(request);
        let handle = self.handle.clone();
        let task = self.handle.spawn(async move {
            let result = attempt.await;
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

    /// Adds a host key to `known_hosts`.
    #[must_use]
    pub fn trust(&self, key: HostKeyPrompt) -> PendingTrust {
        let (tx, rx) = oneshot::channel();
        let work = self.connector.trust_host_key(key);
        self.handle.spawn(async move {
            // A dropped receiver only means nobody needs the answer.
            let _ = tx.send(work.await);
        });
        PendingTrust { rx }
    }

    /// Applies keychain changes on a blocking thread. The receiver yields one message per
    /// failed change (none when everything worked).
    #[must_use]
    pub fn write_secrets(&self, ops: Vec<SecretOp>) -> oneshot::Receiver<Vec<String>> {
        let (tx, rx) = oneshot::channel();
        let secrets = Arc::clone(&self.secrets);
        self.handle.spawn_blocking(move || {
            let failures = ops
                .into_iter()
                .filter_map(|op| apply(secrets.as_ref(), op).err())
                .map(|err| err.to_string())
                .collect();
            let _ = tx.send(failures);
        });
        rx
    }

    /// Runs a blocking closure on tokio's blocking pool and hands its output back on a oneshot.
    /// Dropping the receiver does not stop the closure.
    #[must_use]
    pub fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> oneshot::Receiver<T> {
        let (tx, rx) = oneshot::channel();
        self.handle.spawn_blocking(move || {
            // A dropped receiver only means nobody needs the output.
            let _ = tx.send(f());
        });
        rx
    }

    /// Runs a future on tokio and hands its output back on a oneshot. Dropping the receiver does
    /// not stop the future.
    #[must_use]
    pub fn spawn<T: Send + 'static>(
        &self,
        fut: impl Future<Output = T> + Send + 'static,
    ) -> oneshot::Receiver<T> {
        let (tx, rx) = oneshot::channel();
        self.handle.spawn(async move {
            // A dropped receiver only means nobody needs the output.
            let _ = tx.send(fut.await);
        });
        rx
    }

    /// The connector, for jobs that open a short-lived session.
    #[must_use]
    pub fn connector(&self) -> Arc<dyn Connector> {
        Arc::clone(&self.connector)
    }

    /// Runs a catalog worker. When the worker stops (`Catalog::shutdown`), the session it owned
    /// is closed, and then the receiver fires.
    #[must_use]
    pub fn run_catalog(&self, worker: CatalogWorker) -> oneshot::Receiver<()> {
        self.spawn(async move {
            if let IntrospectSource::Session(session) = worker.run().await {
                close_logged(session).await;
            }
        })
    }

    /// Waits for all receivers, at most `limit`. The receiver fires when they have fired (or
    /// dropped their sender) or the limit is over.
    #[must_use]
    pub fn wait_all(
        &self,
        waits: Vec<oneshot::Receiver<()>>,
        limit: Duration,
    ) -> oneshot::Receiver<()> {
        self.spawn(async move {
            let all = async {
                for wait in waits {
                    // A closed channel means the task is gone, which is also "done".
                    let _ = wait.await;
                }
            };
            if tokio::time::timeout(limit, all).await.is_err() {
                eprintln!("sqler: some catalog workers did not stop in time");
            }
        })
    }

    /// Closes a session in the background; a failure is reported on stderr.
    pub fn close(&self, session: Box<dyn Session>) {
        self.handle.spawn(close_logged(session));
    }

    /// Closes all sessions concurrently, waiting at most `limit`. The receiver fires when they
    /// are closed or the limit is over (the slow ones are abandoned).
    #[must_use]
    pub fn close_all(
        &self,
        sessions: Vec<Box<dyn Session>>,
        limit: Duration,
    ) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.handle.spawn(async move {
            let mut set = JoinSet::new();
            for session in sessions {
                set.spawn(close_logged(session));
            }
            let all = async { while set.join_next().await.is_some() {} };
            if tokio::time::timeout(limit, all).await.is_err() {
                eprintln!("sqler: some sessions did not close in time");
            }
            // Dropping the set aborts whatever is still closing.
            drop(set);
            let _ = tx.send(());
        });
        rx
    }
}

fn apply(store: &dyn SecretStore, op: SecretOp) -> Result<(), SecretError> {
    match op {
        SecretOp::Set(reference, secret) => store.set(&reference, &secret),
        SecretOp::Delete(reference) => store.delete(&reference).map(|_| ()),
    }
}

async fn close_logged(session: Box<dyn Session>) {
    if let Err(err) = session.close().await {
        eprintln!("sqler: failed to close a session: {err}");
    }
}

/// A connect attempt in flight. Awaiting it never blocks a thread.
pub struct PendingConnect {
    rx: oneshot::Receiver<Result<Box<dyn Session>, ConnectFailure>>,
    abort: AbortHandle,
    handle: Handle,
}

impl std::fmt::Debug for PendingConnect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingConnect").finish_non_exhaustive()
    }
}

impl Future for PendingConnect {
    type Output = Result<Box<dyn Session>, ConnectFailure>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(ConnectFailure::Aborted)),
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

/// A host-key trust request in flight.
pub struct PendingTrust {
    rx: oneshot::Receiver<Result<(), ConnectFailure>>,
}

impl std::fmt::Debug for PendingTrust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingTrust").finish_non_exhaustive()
    }
}

impl Future for PendingTrust {
    type Output = Result<(), ConnectFailure>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(ConnectFailure::Aborted)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::workspace::testing::{FakeConnector, FakeSession};
    use sqler_config::{ConnectionSpec, MemoryStore, Secret, SecretKind, SecretRef};
    use sqler_driver_api::DriverError;
    use sqler_driver_api::sqler_core::DriverId;

    use crate::connections::SecretSource;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn request() -> ConnectRequest {
        ConnectRequest {
            spec: ConnectionSpec::new("c", DriverId::new("sqlite").unwrap(), "x"),
            db_password: SecretSource::None,
            ssh_secret: SecretSource::None,
        }
    }

    fn wait_until(flag: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !flag.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "timed out waiting for flag");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn bridge_with(
        rt: &tokio::runtime::Runtime,
        connector: Arc<FakeConnector>,
        store: Arc<MemoryStore>,
    ) -> SessionBridge {
        SessionBridge::new(connector, store, rt.handle().clone())
    }

    fn bridge(rt: &tokio::runtime::Runtime, connector: Arc<FakeConnector>) -> SessionBridge {
        bridge_with(rt, connector, Arc::new(MemoryStore::new()))
    }

    #[test]
    fn connect_ok_records_the_request_and_close_closes() {
        let rt = runtime();
        let connector = Arc::new(FakeConnector::new());
        let bridge = bridge(&rt, connector.clone());
        let session = rt.block_on(bridge.connect(request())).unwrap();
        assert_eq!(connector.requests.lock().unwrap().len(), 1);
        let closed = connector.closed_flags.lock().unwrap()[0].clone();
        bridge.close(session);
        wait_until(&closed);
    }

    #[test]
    fn scripted_failure_comes_back_typed() {
        let rt = runtime();
        let connector = Arc::new(FakeConnector::new());
        connector.push(Err(ConnectFailure::Driver(DriverError::UnknownDriver(
            "nope".into(),
        ))));
        let bridge = bridge(&rt, connector);
        let err = rt.block_on(bridge.connect(request())).err().unwrap();
        assert!(matches!(
            err,
            ConnectFailure::Driver(DriverError::UnknownDriver(_))
        ));
        assert_eq!(err.to_string(), "unknown driver: nope");
    }

    #[test]
    fn dropping_pending_aborts_the_attempt() {
        let rt = runtime();
        let connector = Arc::new(FakeConnector::hanging());
        let bridge = bridge(&rt, connector.clone());
        let pending = bridge.connect(request());
        wait_until(&connector.started);
        assert!(!connector.dropped.load(Ordering::SeqCst));
        drop(pending);
        wait_until(&connector.dropped);
    }

    #[test]
    fn dropping_pending_never_leaks_a_created_session() {
        let rt = runtime();
        let connector = Arc::new(FakeConnector::new());
        let bridge = bridge(&rt, connector.clone());
        let pending = bridge.connect(request());
        // Either the task was aborted before connecting, or the session was created and must
        // be closed whichever side of the oneshot it landed on.
        drop(pending);
        std::thread::sleep(Duration::from_millis(100));
        let flags = connector.closed_flags.lock().unwrap().clone();
        for closed in flags {
            wait_until(&closed);
        }
    }

    #[test]
    fn trust_reports_the_connector_result() {
        let rt = runtime();
        let connector = Arc::new(FakeConnector::new());
        let bridge = bridge(&rt, connector.clone());
        let key = HostKeyPrompt {
            host: "h".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        };
        assert!(rt.block_on(bridge.trust(key.clone())).is_ok());
        assert_eq!(
            connector.trusted.lock().unwrap().as_slice(),
            std::slice::from_ref(&key)
        );
        connector.fail_trust(ConnectFailure::Tunnel("disk full".into()));
        let err = rt.block_on(bridge.trust(key)).err().unwrap();
        assert_eq!(err.to_string(), "SSH tunnel failed: disk full");
    }

    #[test]
    fn write_secrets_applies_sets_and_deletes() {
        let rt = runtime();
        let store = Arc::new(MemoryStore::new());
        let keep = SecretRef::new("c1", SecretKind::DatabasePassword).unwrap();
        let drop_ = SecretRef::new("c1", SecretKind::SshPassword).unwrap();
        store.set(&drop_, &Secret::new("old")).unwrap();
        let bridge = bridge_with(&rt, Arc::new(FakeConnector::new()), store.clone());
        let failures = rt
            .block_on(bridge.write_secrets(vec![
                SecretOp::Set(keep.clone(), Secret::new("pw")),
                SecretOp::Delete(drop_.clone()),
                SecretOp::Delete(drop_.clone()),
            ]))
            .unwrap();
        assert_eq!(failures.len(), 0);
        assert_eq!(store.get(&keep).unwrap().unwrap().expose(), "pw");
        assert!(store.get(&drop_).unwrap().is_none());
    }

    #[test]
    fn write_secrets_reports_failures_without_values() {
        struct Refusing;
        impl SecretStore for Refusing {
            fn set(&self, r: &SecretRef, _: &Secret) -> Result<(), SecretError> {
                Err(SecretError::Unavailable {
                    reference: r.clone(),
                    reason: "locked".into(),
                })
            }
            fn get(&self, _: &SecretRef) -> Result<Option<Secret>, SecretError> {
                Ok(None)
            }
            fn delete(&self, _: &SecretRef) -> Result<bool, SecretError> {
                Ok(false)
            }
        }
        let rt = runtime();
        let bridge = SessionBridge::new(
            Arc::new(FakeConnector::new()),
            Arc::new(Refusing),
            rt.handle().clone(),
        );
        let r = SecretRef::new("c1", SecretKind::DatabasePassword).unwrap();
        let failures = rt
            .block_on(bridge.write_secrets(vec![SecretOp::Set(r, Secret::new("hunter2"))]))
            .unwrap();
        assert_eq!(
            failures,
            ["keychain unavailable for c1:db-password: locked"]
        );
    }

    #[test]
    fn close_all_closes_every_session() {
        let rt = runtime();
        let bridge = bridge(&rt, Arc::new(FakeConnector::new()));
        let (a, closed_a) = FakeSession::boxed();
        let (b, closed_b) = FakeSession::boxed();
        rt.block_on(bridge.close_all(vec![a, b], Duration::from_secs(5)))
            .unwrap();
        assert!(closed_a.load(Ordering::SeqCst) && closed_b.load(Ordering::SeqCst));
        rt.block_on(bridge.close_all(Vec::new(), Duration::from_secs(5)))
            .unwrap();
    }

    #[test]
    fn close_all_gives_up_after_the_limit() {
        let rt = runtime();
        let bridge = bridge(&rt, Arc::new(FakeConnector::new()));
        let (slow, slow_called) = FakeSession::hanging_close();
        let (fast, fast_closed) = FakeSession::boxed();
        let started = Instant::now();
        rt.block_on(bridge.close_all(vec![slow, fast], Duration::from_millis(100)))
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(slow_called.load(Ordering::SeqCst));
        assert!(fast_closed.load(Ordering::SeqCst));
    }
    #[test]
    fn spawn_returns_the_output_and_connector_is_shared() {
        let rt = runtime();
        let connector = Arc::new(FakeConnector::new());
        let bridge = bridge(&rt, connector.clone());
        assert_eq!(rt.block_on(bridge.spawn(async { 7 })).unwrap(), 7);
        let shared = bridge.connector();
        let session = rt.block_on(shared.connect(request())).unwrap();
        assert_eq!(connector.requests.lock().unwrap().len(), 1);
        bridge.close(session);
    }

    #[test]
    fn run_catalog_closes_its_session_after_shutdown() {
        use crate::browser::testing::{FakeIntrospect, SessionConnector};
        use sqler_catalog::Catalog;
        let rt = runtime();
        let scripted = SessionConnector::new(FakeIntrospect::new());
        let session = rt
            .block_on(Connector::connect(&scripted, request()))
            .unwrap();
        let (catalog, worker) = Catalog::new(session);
        let bridge = bridge(&rt, Arc::new(FakeConnector::new()));
        let done = bridge.run_catalog(worker);
        assert_eq!(scripted.closed(), 0);
        catalog.shutdown();
        rt.block_on(done).unwrap();
        assert_eq!(scripted.closed(), 1);
    }

    #[test]
    fn wait_all_gives_up_after_the_limit() {
        let rt = runtime();
        let bridge = bridge(&rt, Arc::new(FakeConnector::new()));
        let (_keep, never) = oneshot::channel::<()>();
        let (done_tx, done) = oneshot::channel::<()>();
        done_tx.send(()).unwrap();
        let started = Instant::now();
        rt.block_on(bridge.wait_all(vec![done, never], Duration::from_millis(100)))
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(4));
        rt.block_on(bridge.wait_all(Vec::new(), Duration::from_secs(5)))
            .unwrap();
    }
}
