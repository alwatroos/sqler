// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The database access of one AI session tab (ADR-0053).
//!
//! [`TabSource`] is the tool set's [`SessionSource`] for exactly one connection. It opens
//! sessions on the binary's connect pipeline (the `Connector` of the `SessionBridge`), keeps one
//! idle session between tool calls, records the in-flight cancel handle and is shut when the tab
//! closes or the app quits. Nothing connects until the first tool call.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sqler_ai_tools::{ConnectionInfo, SessionSource, ToolError};
use sqler_catalog::Catalog;
use sqler_driver_api::{BoxFuture, CancelHandle, Session};

use super::TAB_CLOSED;
use crate::connections::{ConnectRequest, Connector};

#[derive(Default)]
struct State {
    idle: Option<Box<dyn Session>>,
    in_flight: Option<CancelHandle>,
    closed: bool,
}

/// The session source of one AI session tab.
pub struct TabSource {
    info: ConnectionInfo,
    request: ConnectRequest,
    connector: Arc<dyn Connector>,
    catalog: Mutex<Option<Catalog>>,
    state: Mutex<State>,
}

impl std::fmt::Debug for TabSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TabSource")
            .field("connection", &self.info.id)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TabSource {
    /// A source for the connection `info`, opened with `request` on `connector`.
    #[must_use]
    pub fn new(
        info: ConnectionInfo,
        request: ConnectRequest,
        connector: Arc<dyn Connector>,
    ) -> Self {
        Self {
            info,
            request,
            connector,
            catalog: Mutex::new(None),
            state: Mutex::new(State::default()),
        }
    }

    /// Sets the catalog the tools read (the browser's, passed again each turn).
    pub fn set_catalog(&self, catalog: Option<Catalog>) {
        *lock(&self.catalog) = catalog;
    }

    /// The handle that cancels the statement in flight, if a tool call holds a session.
    #[must_use]
    pub fn cancel_in_flight(&self) -> Option<CancelHandle> {
        lock(&self.state).in_flight.clone()
    }

    /// Marks the source closed and takes the idle session out. A session that comes back later
    /// is closed instead of kept.
    pub fn shut(&self) -> Option<Box<dyn Session>> {
        let mut state = lock(&self.state);
        state.closed = true;
        state.idle.take()
    }
}

impl SessionSource for TabSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![self.info.clone()]
    }

    fn catalog(&self, connection: &str) -> Option<Catalog> {
        if connection != self.info.id {
            return None;
        }
        lock(&self.catalog).clone()
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        Box::pin(async move {
            if connection != self.info.id {
                return Err(ToolError::UnknownConnection(connection.to_owned()));
            }
            let idle = {
                let mut state = lock(&self.state);
                if state.closed {
                    return Err(ToolError::NotAvailable(TAB_CLOSED.to_owned()));
                }
                state.idle.take()
            };
            let session = match idle {
                Some(session) => session,
                None => self
                    .connector
                    .connect(self.request.clone())
                    .await
                    .map_err(|failure| ToolError::NotAvailable(failure.to_string()))?,
            };
            let closed = {
                let mut state = lock(&self.state);
                if !state.closed {
                    state.in_flight = Some(session.cancel_handle());
                }
                state.closed
            };
            if closed {
                let _ = session.close().await;
                return Err(ToolError::NotAvailable(TAB_CLOSED.to_owned()));
            }
            Ok(session)
        })
    }

    fn release(&self, _connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        let surplus = {
            let mut state = lock(&self.state);
            state.in_flight = None;
            if state.closed || state.idle.is_some() {
                Some(session)
            } else {
                state.idle = Some(session);
                None
            }
        };
        Box::pin(async move {
            if let Some(session) = surplus {
                let _ = session.close().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_config::ConnectionSpec;
    use sqler_driver_api::sqler_core::{DriverId, Query};
    use sqler_driver_api::{CancelFlag, DriverError, QueryStream};

    use super::*;
    use crate::connections::{ConnectFailure, HostKeyPrompt, SecretSource};

    /// A session whose `close` counts.
    struct Fake {
        closed: Arc<AtomicUsize>,
        flag: CancelFlag,
    }

    impl Session for Fake {
        fn cancel_handle(&self) -> CancelHandle {
            self.flag.handle()
        }

        fn execute<'s>(
            &'s mut self,
            _query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            Box::pin(async { Err(DriverError::Unsupported("test".into())) })
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            self.closed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    struct Counting {
        connects: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
        fail: bool,
    }

    impl Connector for Counting {
        fn driver_ids(&self) -> Vec<sqler_driver_api::sqler_core::DriverId> {
            Vec::new()
        }

        fn connect(
            &self,
            _request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            let closed = Arc::clone(&self.closed);
            let fail = self.fail;
            Box::pin(async move {
                if fail {
                    return Err(ConnectFailure::Aborted);
                }
                let session: Box<dyn Session> = Box::new(Fake {
                    closed,
                    flag: CancelFlag::new(),
                });
                Ok(session)
            })
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    struct Fixture {
        source: TabSource,
        connects: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
    }

    fn fixture(fail: bool) -> Fixture {
        let connects = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let connector = Arc::new(Counting {
            connects: Arc::clone(&connects),
            closed: Arc::clone(&closed),
            fail,
        });
        let driver = DriverId::new("sqlite").unwrap();
        let info = ConnectionInfo::new("c1", "db", driver.clone(), None);
        let request = ConnectRequest {
            spec: ConnectionSpec::new("db", driver, "x"),
            db_password: SecretSource::None,
            ssh_secret: SecretSource::None,
        };
        Fixture {
            source: TabSource::new(info, request, connector),
            connects,
            closed,
        }
    }

    #[tokio::test]
    async fn it_connects_lazily_and_reuses_the_idle_session() {
        let f = fixture(false);
        assert_eq!(f.connects.load(Ordering::SeqCst), 0);
        assert!(f.source.cancel_in_flight().is_none());
        let s = f.source.acquire("c1").await.unwrap();
        assert_eq!(f.connects.load(Ordering::SeqCst), 1);
        assert!(f.source.cancel_in_flight().is_some());
        f.source.release("c1", s).await;
        assert!(f.source.cancel_in_flight().is_none());
        let s = f.source.acquire("c1").await.unwrap();
        assert_eq!(
            f.connects.load(Ordering::SeqCst),
            1,
            "the idle session is reused"
        );
        f.source.release("c1", s).await;
        assert_eq!(f.closed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_second_released_session_is_closed() {
        let f = fixture(false);
        let a = f.source.acquire("c1").await.unwrap();
        let b = f.source.acquire("c1").await.unwrap();
        assert_eq!(f.connects.load(Ordering::SeqCst), 2);
        f.source.release("c1", a).await;
        f.source.release("c1", b).await;
        assert_eq!(f.closed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shut_takes_the_idle_session_and_a_later_release_closes() {
        let f = fixture(false);
        let s = f.source.acquire("c1").await.unwrap();
        assert!(f.source.shut().is_none(), "the session is on loan");
        f.source.release("c1", s).await;
        assert_eq!(f.closed.load(Ordering::SeqCst), 1);
        assert!(matches!(
            f.source.acquire("c1").await,
            Err(ToolError::NotAvailable(m)) if m == TAB_CLOSED
        ));

        let f = fixture(false);
        let s = f.source.acquire("c1").await.unwrap();
        f.source.release("c1", s).await;
        let idle = f.source.shut().expect("the idle session");
        let _ = idle.close().await;
        assert_eq!(f.closed.load(Ordering::SeqCst), 1);
        assert!(f.source.shut().is_none());
    }

    #[tokio::test]
    async fn only_its_own_connection_is_served() {
        let f = fixture(false);
        assert!(matches!(
            f.source.acquire("other").await,
            Err(ToolError::UnknownConnection(_))
        ));
        assert_eq!(f.connects.load(Ordering::SeqCst), 0);
        assert_eq!(f.source.connections().len(), 1);
        assert!(f.source.catalog("other").is_none());
        assert!(f.source.catalog("c1").is_none());
    }

    #[tokio::test]
    async fn a_connect_failure_is_not_available() {
        let f = fixture(true);
        let err = f.source.acquire("c1").await.err().unwrap();
        assert!(matches!(err, ToolError::NotAvailable(m) if m == "connect aborted"));
        assert!(f.source.cancel_in_flight().is_none());
    }
}
