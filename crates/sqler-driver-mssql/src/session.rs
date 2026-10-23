// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The SQL Server session.
//!
//! A task owns the `tiberius` client and runs the statements one at a time; the session talks to
//! it through channels. That is what makes cancel work: the task can drop the client (and with it
//! the socket) at any await point, even while the caller has stopped reading the stream.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use sqler_driver_api::sqler_core::{ConnectionConfig, Query};
use sqler_driver_api::{
    BoxFuture, CancelHandle, Canceller, DriverError, Introspect, QueryStream, Session,
};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt as _};
use tokio_util::sync::CancellationToken;

use crate::options;
use crate::stream::{ChannelStream, Event, Outcome, run_statement};

/// The connect timeout (TCP, TLS and login together).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Events buffered between the worker and the caller.
const CHANNEL_DEPTH: usize = 2;

/// The `tiberius` client over a Tokio socket.
pub(crate) type Client = tiberius::Client<Compat<TcpStream>>;

/// Text of the error after a cancel ended the session.
pub(crate) const CANCELLED_TEXT: &str =
    "the SQL Server connection was closed by a cancel; connect again";
const CLOSED_TEXT: &str = "the SQL Server connection is closed; connect again";

/// The state shared by the session, its cancel handles, its streams and the worker.
#[derive(Default)]
pub(crate) struct Shared {
    /// The token of the statement that is active (streaming or paused), if any.
    active: Mutex<Option<CancellationToken>>,
    /// A cancel closed the connection.
    cancelled: AtomicBool,
    /// The connection is gone (cancel, I/O failure or close).
    closed: AtomicBool,
}

impl Shared {
    fn begin(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.active.lock().unwrap_or_else(PoisonError::into_inner) = Some(token.clone());
        token
    }

    /// The active statement ended; a cancel is a no-op until the next one starts.
    pub(crate) fn finish(&self) {
        *self.active.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn cancel(&self) {
        let token = self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(token) = token {
            token.cancel();
        }
    }

    pub(crate) fn was_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn dead_error(&self) -> Option<DriverError> {
        if self.was_cancelled() {
            Some(DriverError::Connection(CANCELLED_TEXT.to_owned()))
        } else if self.closed.load(Ordering::SeqCst) {
            Some(DriverError::Connection(CLOSED_TEXT.to_owned()))
        } else {
            None
        }
    }
}

struct SqlServerCanceller(Arc<Shared>);

impl Canceller for SqlServerCanceller {
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
        self.0.cancel();
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A request for the worker.
enum Request {
    Execute {
        query: Query,
        events: mpsc::Sender<Event>,
        cancel: CancellationToken,
    },
    Close(oneshot::Sender<Result<(), DriverError>>),
}

/// The text of a `tiberius` error. A server error carries its number; the password is never part
/// of any of these texts.
fn error_text(e: &tiberius::error::Error) -> String {
    match e {
        tiberius::error::Error::Server(te) => format!("{} (error {})", te.message(), te.code()),
        other => other.to_string(),
    }
}

/// The error for a failure while connecting.
fn connect_text(e: &tiberius::error::Error) -> String {
    error_text(e)
}

/// The error for a failure while running a statement: server errors are query errors, the rest
/// breaks the connection.
pub(crate) fn query_error(e: &tiberius::error::Error) -> DriverError {
    match e {
        tiberius::error::Error::Server(_) => DriverError::Query(error_text(e)),
        other => DriverError::Connection(other.to_string()),
    }
}

async fn worker(
    mut client: Option<Client>,
    mut requests: mpsc::Receiver<Request>,
    shared: Arc<Shared>,
) {
    while let Some(request) = requests.recv().await {
        match request {
            Request::Execute {
                query,
                events,
                cancel,
            } => {
                let Some(conn) = client.as_mut() else {
                    let error = shared
                        .dead_error()
                        .unwrap_or_else(|| DriverError::Connection(CLOSED_TEXT.to_owned()));
                    let _ = events.send(Err(error)).await;
                    continue;
                };
                match run_statement(conn, &query, &events, &cancel).await {
                    Outcome::Finished => {}
                    Outcome::Cancelled => {
                        // Dropping the client closes the socket: the server ends the statement
                        // and rolls back an open transaction.
                        client = None;
                        shared.cancelled.store(true, Ordering::SeqCst);
                        shared.closed.store(true, Ordering::SeqCst);
                        let _ = events.try_send(Err(DriverError::Cancelled));
                    }
                    Outcome::Broken => {
                        client = None;
                        shared.closed.store(true, Ordering::SeqCst);
                    }
                }
            }
            Request::Close(reply) => {
                let result = match client.take() {
                    Some(conn) => conn.close().await.map_err(|e| query_error(&e)),
                    None => Ok(()),
                };
                shared.closed.store(true, Ordering::SeqCst);
                let _ = reply.send(result);
                break;
            }
        }
    }
}

/// One open SQL Server connection.
pub(crate) struct MssqlSession {
    requests: mpsc::Sender<Request>,
    shared: Arc<Shared>,
}

impl MssqlSession {
    pub(crate) async fn connect(config: &ConnectionConfig) -> Result<Self, DriverError> {
        crate::install_ring_provider();
        let (tds, target) = options::build(config)?;
        let host = target.host.clone();
        let port = target.port;
        drop(target);
        let attempt = async move {
            let tcp = TcpStream::connect((host.as_str(), port))
                .await
                .map_err(|e| DriverError::Connection(format!("could not reach the server: {e}")))?;
            tcp.set_nodelay(true)
                .map_err(|e| DriverError::Connection(e.to_string()))?;
            Box::pin(Client::connect(tds, tcp.compat_write()))
                .await
                .map_err(|e| {
                    DriverError::Connection(connect_text(&e).replace(host.as_str(), "the server"))
                })
        };
        let client = tokio::time::timeout(CONNECT_TIMEOUT, Box::pin(attempt))
            .await
            .map_err(|_| {
                DriverError::Connection("connecting to SQL Server timed out after 30 s".to_owned())
            })??;
        let shared = Arc::new(Shared::default());
        let (requests, rx) = mpsc::channel(1);
        tokio::spawn(worker(Some(client), rx, shared.clone()));
        Ok(Self { requests, shared })
    }

    async fn run<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> Result<Box<dyn QueryStream + 's>, DriverError> {
        if let Some(error) = self.shared.dead_error() {
            return Err(error);
        }
        let cancel = self.shared.begin();
        let (events, rx) = mpsc::channel(CHANNEL_DEPTH);
        let request = Request::Execute {
            query: query.clone(),
            events,
            cancel,
        };
        if self.requests.send(request).await.is_err() {
            self.shared.finish();
            return Err(DriverError::Connection(CLOSED_TEXT.to_owned()));
        }
        Ok(Box::new(ChannelStream::new(rx, self.shared.clone())))
    }

    /// Runs `query` to the end and collects it (for introspection).
    pub(crate) async fn fetch(
        &mut self,
        query: &Query,
    ) -> Result<sqler_driver_api::sqler_core::ResultSet, DriverError> {
        let mut stream = self.run(query).await?;
        sqler_driver_api::collect(&mut *stream).await
    }
}

impl Drop for MssqlSession {
    fn drop(&mut self) {
        // A statement still running on a dropped session ends with the connection.
        self.shared.cancel();
    }
}

impl Session for MssqlSession {
    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::new(Arc::new(SqlServerCanceller(self.shared.clone())))
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        Some(self)
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(self.run(query))
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(async move {
            let (reply, done) = oneshot::channel();
            if self.requests.send(Request::Close(reply)).await.is_err() {
                return Ok(());
            }
            done.await.unwrap_or(Ok(()))
        })
    }
}
