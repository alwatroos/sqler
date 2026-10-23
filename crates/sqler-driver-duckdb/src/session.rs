// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `DuckDB` session and its worker thread.
//!
//! A `duckdb::Connection` is blocking and `!Sync`, and statements borrow it. So each session owns
//! one `std::thread` that owns the connection and takes commands over a channel. Rows travel
//! back over a bounded channel, so a slow consumer slows the worker down instead of filling
//! memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use duckdb::arrow::array::RecordBatch;
use duckdb::{Connection, InterruptHandle, params_from_iter};
use sqler_driver_api::sqler_core::{Column, ConnectionConfig, Query, ResultSet, Row, Value};
use sqler_driver_api::{
    BoxFuture, CancelHandle, Canceller, DriverError, Introspect, QueryStream, Session, collect,
};
use tokio::sync::{mpsc, oneshot};

use crate::convert;
use crate::database::{self, Opened};
use crate::error::{Scrub, query_error};
use crate::stream::DuckQueryStream;

/// Rows per `QueryEvent::Rows`.
const BATCH: usize = 500;
/// Batches buffered between the worker and the consumer.
const BUFFER: usize = 2;

/// What the worker sends back for each batch of rows.
pub(crate) type RowMessage = Result<Vec<Row>, DriverError>;

/// The first reply of a statement.
pub(crate) struct Started {
    pub(crate) columns: Vec<Column>,
    pub(crate) rows_affected: Option<u64>,
    pub(crate) rows: mpsc::Receiver<RowMessage>,
}

struct Job {
    sql: String,
    params: Vec<Value>,
    started: oneshot::Sender<Result<Started, DriverError>>,
}

enum Command {
    Run(Job),
    Close,
}

/// Cancel: sets the flag and interrupts the running statement.
struct DuckCanceller {
    flag: Arc<AtomicBool>,
    interrupt: Arc<InterruptHandle>,
}

impl Canceller for DuckCanceller {
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
        self.flag.store(true, Ordering::SeqCst);
        self.interrupt.interrupt();
        Box::pin(std::future::ready(Ok(())))
    }
}

/// One open `DuckDB` connection behind a worker thread.
pub(crate) struct DuckSession {
    commands: Option<std_mpsc::Sender<Command>>,
    thread: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    interrupt: Arc<InterruptHandle>,
}

impl DuckSession {
    pub(crate) async fn connect(config: &ConnectionConfig) -> Result<Self, DriverError> {
        let target = config.target.clone();
        let (commands, inbox) = std_mpsc::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("sqler-duckdb".to_owned())
            .spawn(move || match database::open(&target) {
                Ok(opened) => {
                    let interrupt = opened.conn.interrupt_handle();
                    if ready_tx.send(Ok(interrupt)).is_ok() {
                        work(&opened, &inbox);
                    }
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| DriverError::Backend(Box::new(e)))?;
        let interrupt = ready_rx.await.map_err(|_| worker_gone())??;
        Ok(Self {
            commands: Some(commands),
            thread: Some(thread),
            cancel: Arc::new(AtomicBool::new(false)),
            interrupt,
        })
    }

    /// Runs one statement and reads it to the end.
    pub(crate) async fn run_all(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<ResultSet, DriverError> {
        let query = Query::new(sql).with_params(params);
        let mut stream = self.start(&query).await?;
        collect(&mut stream).await
    }

    async fn start(&self, query: &Query) -> Result<DuckQueryStream, DriverError> {
        self.cancel.store(false, Ordering::SeqCst);
        let (started_tx, started_rx) = oneshot::channel();
        let job = Job {
            sql: query.sql.clone(),
            params: query.params.clone(),
            started: started_tx,
        };
        let commands = self.commands.as_ref().ok_or_else(worker_gone)?;
        commands
            .send(Command::Run(job))
            .map_err(|_| worker_gone())?;
        let reply = started_rx.await.map_err(|_| worker_gone())?;
        match reply {
            Ok(started) => Ok(DuckQueryStream::new(started, self.cancel.clone())),
            Err(_) if self.cancel.load(Ordering::SeqCst) => Err(DriverError::Cancelled),
            Err(e) => Err(e),
        }
    }
}

fn worker_gone() -> DriverError {
    DriverError::Backend("the DuckDB worker stopped".into())
}

impl Session for DuckSession {
    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::new(Arc::new(DuckCanceller {
            flag: self.cancel.clone(),
            interrupt: self.interrupt.clone(),
        }))
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        Some(self)
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(async move {
            let stream = self.start(query).await?;
            Ok(Box::new(stream) as Box<dyn QueryStream + 's>)
        })
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(async move {
            if let Some(commands) = self.commands.take() {
                let _ = commands.send(Command::Close);
            }
            let Some(thread) = self.thread.take() else {
                return Ok(());
            };
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|e| DriverError::Backend(Box::new(e)))?
                .map_err(|_| worker_gone())
        })
    }
}

/// The worker loop. Ends on `Close` or when the session is dropped.
fn work(opened: &Opened, inbox: &std_mpsc::Receiver<Command>) {
    let Opened { conn, scrub, .. } = &opened;
    while let Ok(command) = inbox.recv() {
        match command {
            Command::Run(job) => run_job(conn, scrub, job),
            Command::Close => break,
        }
    }
}

/// Whether the statement is an INSERT, UPDATE or DELETE (`DuckDB`'s DML types).
fn is_dml(sql: &str) -> bool {
    let mut rest = sql.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after
                .split_once('\n')
                .map_or("", |(_, tail)| tail)
                .trim_start();
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after
                .split_once("*/")
                .map_or("", |(_, tail)| tail)
                .trim_start();
        } else {
            break;
        }
    }
    let word: String = rest
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(word.as_str(), "insert" | "update" | "delete")
}

fn run_job(conn: &Connection, scrub: &Scrub, job: Job) {
    let Job {
        sql,
        params,
        started,
    } = job;
    let fail = |e: &duckdb::Error, started: oneshot::Sender<_>| {
        let _ = started.send(Err(query_error(e, scrub)));
    };
    let mut stmt = match conn.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => return fail(&e, started),
    };
    let values: Vec<duckdb::types::Value> = params.iter().map(convert::param).collect();
    let dml = is_dml(&sql);
    let mut changed = None;
    if dml {
        match stmt.execute(params_from_iter(values)) {
            Ok(n) => changed = Some(n as u64),
            Err(e) => return fail(&e, started),
        }
    } else {
        match stmt.stream_arrow(params_from_iter(values)) {
            Ok(arrow) => drop(arrow),
            Err(e) => return fail(&e, started),
        }
    }
    let schema = stmt.schema();
    let (tx, rx) = mpsc::channel(BUFFER);
    let affected_only = changed.is_some()
        && schema.fields().len() == 1
        && schema.field(0).name().eq_ignore_ascii_case("count");
    let reply = if affected_only {
        Started {
            columns: Vec::new(),
            rows_affected: changed,
            rows: rx,
        }
    } else {
        Started {
            columns: convert::columns(&schema),
            rows_affected: None,
            rows: rx,
        }
    };
    if started.send(Ok(reply)).is_err() || affected_only {
        return;
    }
    loop {
        match stmt.step() {
            Ok(Some(array)) => {
                let batch = RecordBatch::from(&array);
                match convert::rows(&batch) {
                    Ok(rows) => {
                        let mut rows = rows.into_iter();
                        loop {
                            let chunk: Vec<Row> = rows.by_ref().take(BATCH).collect();
                            if chunk.is_empty() {
                                break;
                            }
                            if tx.blocking_send(Ok(chunk)).is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.blocking_send(Err(e));
                        return;
                    }
                }
            }
            Ok(None) => return,
            Err(e) => {
                let _ = tx.blocking_send(Err(query_error(&e, scrub)));
                return;
            }
        }
    }
}

impl Drop for DuckSession {
    fn drop(&mut self) {
        // The channel closes, the worker leaves its loop and drops the connection.
        self.commands = None;
    }
}
