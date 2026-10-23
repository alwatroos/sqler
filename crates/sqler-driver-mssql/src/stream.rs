// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The two halves of a statement's event stream.
//!
//! The worker half ([`run_statement`]) runs inside the task that owns the connection: it sends
//! the statement, turns the first result set into `QueryEvent`s and pushes them through a bounded
//! channel. The consumer half ([`ChannelStream`]) is what the caller polls.

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::Arc;

use futures_core::Stream as _;
use sqler_driver_api::sqler_core::{Query, Row};
use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};
use tiberius::QueryItem;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::convert::{column, decode_row};
use crate::session::{Client, Shared, query_error};

/// Rows per `QueryEvent::Rows`.
const BATCH: usize = 256;

/// One item on the channel between the worker and the consumer.
pub(crate) type Event = Result<QueryEvent, DriverError>;

/// How a statement ended, as far as the worker is concerned.
pub(crate) enum Outcome {
    /// The statement ended, or the consumer went away; the connection is still usable.
    Finished,
    /// A cancel arrived; the connection must be closed.
    Cancelled,
    /// The connection failed; it must be closed. The error is already sent to the consumer.
    Broken,
}

/// Awaits `fut` unless `cancel` fires first.
async fn or_cancel<F: Future>(cancel: &CancellationToken, fut: F) -> Option<F::Output> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        out = fut => Some(out),
    }
}

enum Sent {
    Yes,
    ConsumerGone,
    Cancelled,
}

async fn send(tx: &mpsc::Sender<Event>, cancel: &CancellationToken, event: Event) -> Sent {
    match or_cancel(cancel, tx.send(event)).await {
        None => Sent::Cancelled,
        Some(Ok(())) => Sent::Yes,
        Some(Err(_)) => Sent::ConsumerGone,
    }
}

/// Sends `event` and maps a lost consumer or a cancel to the outcome to stop with.
macro_rules! emit {
    ($tx:expr, $cancel:expr, $event:expr) => {
        match send($tx, $cancel, $event).await {
            Sent::Yes => {}
            Sent::ConsumerGone => return Outcome::Finished,
            Sent::Cancelled => return Outcome::Cancelled,
        }
    };
}

/// Runs `query` on `client` and streams the first result set to `tx`.
pub(crate) async fn run_statement(
    client: &mut Client,
    query: &Query,
    tx: &mpsc::Sender<Event>,
    cancel: &CancellationToken,
) -> Outcome {
    let started = if query.params.is_empty() {
        // A plain batch keeps the session state (`USE`, `SET`) between statements.
        or_cancel(cancel, client.simple_query(query.sql.clone())).await
    } else {
        let mut bound = tiberius::Query::new(query.sql.clone());
        for param in &query.params {
            crate::convert::bind(&mut bound, param);
        }
        or_cancel(cancel, bound.query(client)).await
    };
    let mut stream = match started {
        None => return Outcome::Cancelled,
        Some(Ok(stream)) => stream,
        Some(Err(e)) => return fail(e, tx, cancel).await,
    };

    let mut sets = 0_usize;
    let mut batch: Vec<Row> = Vec::new();
    loop {
        let next = or_cancel(cancel, poll_fn(|cx| Pin::new(&mut stream).poll_next(cx))).await;
        let item = match next {
            None => return Outcome::Cancelled,
            Some(None) => break,
            Some(Some(Err(e))) => return fail(e, tx, cancel).await,
            Some(Some(Ok(item))) => item,
        };
        match item {
            QueryItem::Metadata(meta) => {
                sets += 1;
                if sets == 1 {
                    let columns = meta.columns().iter().map(column).collect();
                    emit!(tx, cancel, Ok(QueryEvent::Columns(columns)));
                }
            }
            // Later result sets are drained and ignored.
            QueryItem::Row(row) if sets == 1 => match decode_row(&row) {
                Ok(row) => {
                    batch.push(row);
                    if batch.len() >= BATCH {
                        let rows = std::mem::take(&mut batch);
                        emit!(tx, cancel, Ok(QueryEvent::Rows(rows)));
                    }
                }
                Err(e) => {
                    let _ = send(tx, cancel, Err(e)).await;
                    return Outcome::Finished;
                }
            },
            QueryItem::Row(_) => {}
        }
    }
    if sets == 0 {
        emit!(tx, cancel, Ok(QueryEvent::Columns(Vec::new())));
    }
    if !batch.is_empty() {
        emit!(tx, cancel, Ok(QueryEvent::Rows(batch)));
    }
    emit!(
        tx,
        cancel,
        Ok(QueryEvent::Done {
            rows_affected: None
        })
    );
    Outcome::Finished
}

/// Sends a tiberius error. A server error leaves the connection usable; anything else breaks it.
async fn fail(
    e: tiberius::error::Error,
    tx: &mpsc::Sender<Event>,
    cancel: &CancellationToken,
) -> Outcome {
    let server = matches!(e, tiberius::error::Error::Server(_));
    let _ = send(tx, cancel, Err(query_error(&e))).await;
    if server {
        Outcome::Finished
    } else {
        Outcome::Broken
    }
}

/// The event stream of one statement, as the caller sees it.
pub(crate) struct ChannelStream {
    rx: mpsc::Receiver<Event>,
    shared: Arc<Shared>,
    ended: bool,
}

impl ChannelStream {
    pub(crate) fn new(rx: mpsc::Receiver<Event>, shared: Arc<Shared>) -> Self {
        Self {
            rx,
            shared,
            ended: false,
        }
    }

    async fn step(&mut self) -> Option<Event> {
        if self.ended {
            return None;
        }
        let event = self.rx.recv().await;
        match event {
            Some(Ok(QueryEvent::Done { .. }) | Err(_)) => self.end(),
            Some(Ok(_)) => {}
            None => {
                self.end();
                return Some(Err(if self.shared.was_cancelled() {
                    DriverError::Cancelled
                } else {
                    DriverError::Connection("the SQL Server connection was lost".to_owned())
                }));
            }
        }
        event
    }

    fn end(&mut self) {
        self.ended = true;
        self.shared.finish();
    }
}

impl Drop for ChannelStream {
    fn drop(&mut self) {
        self.shared.finish();
    }
}

impl QueryStream for ChannelStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Event>> {
        Box::pin(self.step())
    }
}
