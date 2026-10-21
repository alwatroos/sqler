// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The export pipeline: one row *producer* and one *writer*, joined by a bounded channel.
//!
//! The writer ([`write_feed`]) runs on a blocking thread, drives an `sqler_export::Exporter` into
//! a hidden partial file ([`PartialFile`]) and renames it over the target only after `finish`. The
//! producer is either a fresh run of the statement ([`fresh_rows`], through [`pump_stream`]) or a
//! copy of the rows the grid holds ([`LoadedCursor`]). At most [`FEED_CAPACITY`] batches wait in
//! the channel, so memory does not depend on the row count.
//!
//! No text made here holds the target path, the statement or a credential. `Driver` and `Connect`
//! carry the text the driver or the connect pipeline shows the user (as the query tab does),
//! built from their `Display` only and through [`driver_text`] and [`connect_text`], which swap
//! every variant that can echo the target, a path or a secret for a fixed text.

use std::sync::Arc;

use sqler_driver_api::sqler_core::{Column, Query, Row};
use sqler_driver_api::{DriverError, QueryEvent, QueryStream};
use sqler_export::{
    CancelToken, ExportControl, ExportError, ExportOptions, Exporter, Progress, Registry, Sink,
};
use tokio::sync::{mpsc, watch};

use super::file::PartialFile;
use super::plan::LoadedRows;
use crate::connections::{ConnectFailure, ConnectRequest, Connector};
use crate::grid::GridModel;

use std::path::PathBuf;

/// Batches that wait between the producer and the writer; the memory bound of an export.
pub const FEED_CAPACITY: usize = 4;

/// One message from the producer to the writer, in this order: `Columns`, `Rows`..., then `Done`
/// or `Failed`.
#[derive(Debug)]
pub enum Feed {
    /// The columns of the result. Empty for a statement without a result set.
    Columns(Vec<Column>),
    /// A batch of rows.
    Rows(Vec<Row>),
    /// The result ended.
    Done,
    /// The producer failed; the export fails with this.
    Failed(ExportFailure),
}

/// Why an export did not produce a file. The text is for the user and never holds the target
/// path or the statement.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExportFailure {
    /// The user cancelled (or the tab was closed).
    #[error("cancelled")]
    Cancelled,
    /// The short-lived session could not be opened.
    #[error("cannot connect: {0}")]
    Connect(String),
    /// The database failed while the statement ran.
    #[error("{0}")]
    Driver(String),
    /// The file could not be created.
    #[error("{0}")]
    Create(String),
    /// Writing, syncing or renaming the file failed.
    #[error("{0}")]
    Write(String),
    /// The format refused the options or a value.
    #[error("{0}")]
    Format(String),
    /// The statement returned no result set.
    #[error("the statement returned no result set; there is nothing to export")]
    NoResultSet,
    /// The rows stopped coming before the result was complete.
    #[error("the rows stopped coming before the result was complete")]
    SourceEnded,
    /// The result the dialog described is not the one that is there now.
    #[error("The result changed while the dialog was open; export cancelled.")]
    ResultChanged,
    /// The export task ended without a result.
    #[error("the export ended unexpectedly")]
    Lost,
}

/// How a finished export ended.
pub type ExportOutcome = Result<ExportSummary, ExportFailure>;

/// What a finished export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    /// Rows written.
    pub rows: u64,
    /// Bytes written.
    pub bytes: u64,
    /// The file.
    pub path: PathBuf,
}

/// Stops an export: a flag for blocking code and a signal for async code. Clones share both.
#[derive(Clone, Debug)]
pub struct ExportStop {
    token: CancelToken,
    signal: Arc<watch::Sender<bool>>,
}

impl Default for ExportStop {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportStop {
    /// A stop that is not raised.
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: CancelToken::new(),
            signal: Arc::new(watch::channel(false).0),
        }
    }

    /// Raises the stop for every clone.
    pub fn cancel(&self) {
        self.token.cancel();
        self.signal.send_replace(true);
    }

    /// Whether the stop was raised.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// The flag the writer checks.
    #[must_use]
    pub fn token(&self) -> CancelToken {
        self.token.clone()
    }

    /// Resolves once the stop is raised.
    pub async fn cancelled(&self) {
        let mut rx = self.signal.subscribe();
        // The sender lives as long as `self`, so the wait cannot fail.
        let _ = rx.wait_for(|raised| *raised).await;
    }
}

/// What the writer needs to know.
#[derive(Debug, Clone)]
pub struct WriteSpec {
    /// The id of the format in the registry.
    pub format_id: String,
    /// The options of the format.
    pub options: ExportOptions,
    /// The target file.
    pub path: PathBuf,
    /// The job; it names the partial file.
    pub job: u64,
}

fn export_failure(err: ExportError) -> ExportFailure {
    match err {
        ExportError::Cancelled => ExportFailure::Cancelled,
        ExportError::Io(err) => ExportFailure::Write(format!("write failed: {err}")),
        // Their texts carry the option and identifier values.
        ExportError::InvalidOptions(_) => {
            ExportFailure::Format("the export options cannot be used with this format".to_owned())
        }
        ExportError::Sql(_) => ExportFailure::Format(
            "the table or schema name cannot be used as an SQL identifier".to_owned(),
        ),
        other => ExportFailure::Format(other.to_string()),
    }
}

fn protocol(msg: &str) -> ExportFailure {
    ExportFailure::Driver(format!("driver protocol error: {msg}"))
}

/// Writes what the producer sends into the target file. Blocks: run it on a blocking thread.
///
/// The partial file is created before the first row, so a bad target fails at once. It is renamed
/// over the target after the last row and deleted on every other path.
///
/// # Errors
///
/// An [`ExportFailure`]; `Cancelled` when `stop` was raised.
pub fn write_feed(
    rx: mpsc::Receiver<Feed>,
    spec: WriteSpec,
    stop: &ExportStop,
    progress: Box<dyn FnMut(Progress) + Send>,
) -> ExportOutcome {
    write_with(rx, spec, stop, progress, |file| Box::new(file))
}

/// [`write_feed`] with a wrapper around the file handle (tests make the sink fail).
fn write_with(
    mut rx: mpsc::Receiver<Feed>,
    spec: WriteSpec,
    stop: &ExportStop,
    progress: Box<dyn FnMut(Progress) + Send>,
    wrap: impl FnOnce(std::fs::File) -> Sink<'static>,
) -> ExportOutcome {
    let registry = Registry::with_builtin();
    let Some(format) = registry.get(&spec.format_id) else {
        return Err(ExportFailure::Format(
            "this format is not available in this build".to_owned(),
        ));
    };
    if stop.is_cancelled() {
        return Err(ExportFailure::Cancelled);
    }
    // Fail fast: a bad target is reported before any row is read.
    let (partial, file) = PartialFile::create(&spec.path, spec.job)
        .map_err(|err| ExportFailure::Create(format!("cannot create the file: {err}")))?;
    let control = ExportControl {
        cancel: stop.token(),
        progress: Some(progress),
    };
    // Declared after `partial`, so it drops (and closes the file) first.
    let mut exporter =
        Exporter::new(format, &spec.options, wrap(file), control).map_err(export_failure)?;
    let mut began = false;
    loop {
        let Some(feed) = rx.blocking_recv() else {
            return Err(if stop.is_cancelled() {
                ExportFailure::Cancelled
            } else {
                ExportFailure::SourceEnded
            });
        };
        match feed {
            Feed::Columns(columns) => {
                if began {
                    return Err(protocol("second Columns event"));
                }
                if columns.is_empty() {
                    return Err(ExportFailure::NoResultSet);
                }
                exporter.begin(&columns).map_err(export_failure)?;
                began = true;
            }
            Feed::Rows(rows) => {
                if !began {
                    return Err(protocol("Rows before Columns"));
                }
                exporter.write_batch(&rows).map_err(export_failure)?;
            }
            Feed::Done => {
                if !began {
                    return Err(protocol("Done before Columns"));
                }
                let summary = exporter.finish().map_err(export_failure)?;
                partial
                    .commit()
                    .map_err(|err| ExportFailure::Write(format!("cannot write the file: {err}")))?;
                return Ok(ExportSummary {
                    rows: summary.rows,
                    bytes: summary.bytes,
                    path: spec.path,
                });
            }
            Feed::Failed(failure) => return Err(failure),
        }
    }
}

/// Sends one message, giving up when the stop is raised or the writer is gone.
async fn send(tx: &mpsc::Sender<Feed>, stop: &ExportStop, feed: Feed) -> Result<(), ExportFailure> {
    tokio::select! {
        sent = tx.send(feed) => sent.map_err(|_| {
            if stop.is_cancelled() { ExportFailure::Cancelled } else { ExportFailure::SourceEnded }
        }),
        () = stop.cancelled() => Err(ExportFailure::Cancelled),
    }
}

fn driver_failure(err: &DriverError) -> ExportFailure {
    match err {
        DriverError::Cancelled => ExportFailure::Cancelled,
        other => ExportFailure::Driver(driver_text(other)),
    }
}

/// The user text of a driver error. The database's own message is kept (`syntax error at row
/// 40,000` is what the user needs); a target that a driver could not parse and the library
/// errors that are not a database message get a fixed text, because only those can echo the
/// target.
#[must_use]
pub fn driver_text(err: &DriverError) -> String {
    match err {
        DriverError::Connection(text) if text.starts_with("invalid ") => {
            "the connection target is invalid".to_owned()
        }
        DriverError::Backend(_) => "the database library reported an error".to_owned(),
        other => other.to_string(),
    }
}

/// The user text of a connect failure. `Tunnel` (its text can hold a key or `known_hosts` path),
/// `Target` and `Secret` get a fixed text; the rest is the `Display` of the variant, which holds
/// no path, URL or secret (a host key prompt names the SSH `host:port`).
#[must_use]
pub fn connect_text(failure: &ConnectFailure) -> String {
    match failure {
        ConnectFailure::Tunnel(_) => "SSH tunnel failed".to_owned(),
        ConnectFailure::Target(_) => "the connection target is invalid".to_owned(),
        ConnectFailure::Secret(_) => "the secret store failed".to_owned(),
        ConnectFailure::Driver(err) => driver_text(err),
        other => other.to_string(),
    }
}

/// Moves the events of one statement's `stream` into the channel, one batch at a time. A full
/// channel makes it wait, so the stream is read no faster than the file is written.
///
/// # Errors
///
/// `Cancelled` when `stop` is raised (also while it waits for room), `Driver` for a driver error
/// or a stream that breaks the protocol (a second `Columns`, rows before `Columns`, an end before
/// `Done`), `SourceEnded` when the writer is gone.
pub async fn pump_stream(
    stream: &mut (dyn QueryStream + '_),
    tx: &mpsc::Sender<Feed>,
    stop: &ExportStop,
) -> Result<(), ExportFailure> {
    let mut columns: Option<usize> = None;
    loop {
        if stop.is_cancelled() {
            return Err(ExportFailure::Cancelled);
        }
        let Some(event) = stream.next_event().await else {
            return Err(protocol("stream ended before Done"));
        };
        match event.map_err(|err| driver_failure(&err))? {
            QueryEvent::Columns(cols) => {
                if columns.is_some() {
                    return Err(protocol("second Columns event"));
                }
                columns = Some(cols.len());
                send(tx, stop, Feed::Columns(cols)).await?;
            }
            QueryEvent::Rows(batch) => {
                match columns {
                    None => return Err(protocol("Rows before Columns")),
                    Some(0) if batch.is_empty() => continue,
                    Some(0) => return Err(protocol("Rows for a statement without columns")),
                    Some(_) if batch.is_empty() => continue,
                    Some(_) => {}
                }
                send(tx, stop, Feed::Rows(batch)).await?;
            }
            QueryEvent::Done { .. } => {
                if columns.is_none() {
                    return Err(protocol("Done before Columns"));
                }
                return send(tx, stop, Feed::Done).await;
            }
            _ => {}
        }
    }
}

/// Logs a failed close without the driver's text (it can carry the target).
async fn close_session(session: Box<dyn sqler_driver_api::Session>) {
    if session.close().await.is_err() {
        eprintln!("sqler: failed to close an export session");
    }
}

/// Runs `query` on a short-lived session and pumps its rows into `tx`. The session is closed on
/// every path. A failure is sent as [`Feed::Failed`]; when `stop` is raised the driver is asked to
/// cancel before the stream is dropped.
pub async fn fresh_rows(
    connector: Arc<dyn Connector>,
    request: ConnectRequest,
    query: Query,
    tx: mpsc::Sender<Feed>,
    stop: ExportStop,
) {
    if let Err(failure) = fresh_run(connector, request, &query, &tx, &stop).await {
        // The writer may be gone already; the failure is for it only.
        let _ = send(&tx, &stop, Feed::Failed(failure)).await;
    }
}

async fn fresh_run(
    connector: Arc<dyn Connector>,
    request: ConnectRequest,
    query: &Query,
    tx: &mpsc::Sender<Feed>,
    stop: &ExportStop,
) -> Result<(), ExportFailure> {
    if stop.is_cancelled() {
        return Err(ExportFailure::Cancelled);
    }
    // The writer fails fast on a bad target: then nothing is connected.
    if tx.is_closed() {
        return Err(ExportFailure::SourceEnded);
    }
    let connected = tokio::select! {
        session = connector.connect(request) => session,
        () = stop.cancelled() => return Err(ExportFailure::Cancelled),
        () = tx.closed() => return Err(ExportFailure::SourceEnded),
    };
    let mut session = connected.map_err(|err| ExportFailure::Connect(connect_text(&err)))?;
    let handle = session.cancel_handle();
    let result = async {
        // The `SQLite` driver resets its cancel flag in `execute`: a stop before it ends here.
        if stop.is_cancelled() {
            return Err(ExportFailure::Cancelled);
        }
        let executed = tokio::select! {
            executed = session.execute(query) => executed,
            () = stop.cancelled() => {
                let _ = handle.cancel().await;
                return Err(ExportFailure::Cancelled);
            }
            () = tx.closed() => {
                let _ = handle.cancel().await;
                return Err(ExportFailure::SourceEnded);
            }
        };
        let mut stream = executed.map_err(|err| driver_failure(&err))?;
        tokio::select! {
            pumped = pump_stream(&mut *stream, tx, stop) => pumped,
            () = stop.cancelled() => {
                // Ask the database to stop before the stream is dropped.
                let _ = handle.cancel().await;
                Err(ExportFailure::Cancelled)
            }
        }
    }
    .await;
    close_session(session).await;
    result
}

/// Where a [`LoadedCursor`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Columns,
    Rows,
    Done,
    Finished,
}

/// Copies the rows a grid model holds into [`Feed`] messages, a few at a time, so the UI thread
/// can do it in short slices. The model belongs to the UI thread: the cursor borrows it per call.
#[derive(Debug)]
pub struct LoadedCursor {
    rows: LoadedRows,
    row_count: usize,
    next: usize,
    stage: Stage,
}

impl LoadedCursor {
    /// A cursor over `rows`. For [`LoadedRows::All`] it covers `0..row_count`, fixed now: rows
    /// that arrive later are not exported.
    #[must_use]
    pub fn new(rows: LoadedRows, row_count: usize) -> Self {
        Self {
            rows,
            row_count,
            next: 0,
            stage: Stage::Columns,
        }
    }

    /// The failure message that ends the copy.
    fn fail(&mut self) -> Feed {
        self.stage = Stage::Finished;
        Feed::Failed(ExportFailure::ResultChanged)
    }

    /// The next message: `Columns`, then `Rows` with at most `max_rows` rows each, then `Done`,
    /// then `None`. A picked row or column that the model does not have (the result changed)
    /// ends the copy with `Failed(ResultChanged)`; nothing is skipped.
    pub fn next_feed(&mut self, model: &GridModel, max_rows: usize) -> Option<Feed> {
        let max_rows = max_rows.max(1);
        match self.stage {
            Stage::Columns => {
                self.stage = Stage::Rows;
                let columns = match &self.rows {
                    LoadedRows::All => model.columns().to_vec(),
                    LoadedRows::Picked { columns, .. } => {
                        let picked: Option<Vec<Column>> = columns
                            .iter()
                            .map(|&c| model.columns().get(c).cloned())
                            .collect();
                        match picked {
                            Some(picked) => picked,
                            None => return Some(self.fail()),
                        }
                    }
                };
                Some(Feed::Columns(columns))
            }
            Stage::Rows => {
                let total = match &self.rows {
                    LoadedRows::All => self.row_count,
                    LoadedRows::Picked { rows, .. } => rows.len(),
                };
                let end = total.min(self.next.saturating_add(max_rows));
                let mut batch = Vec::with_capacity(end.saturating_sub(self.next));
                for i in self.next..end {
                    // A row or column that is not there any more is an error, never skipped.
                    let row = match &self.rows {
                        LoadedRows::All => model.row(i).map(|r| r.to_row()),
                        LoadedRows::Picked { rows, columns } => {
                            rows.get(i).and_then(|&r| model.row(r)).and_then(|r| {
                                columns
                                    .iter()
                                    .map(|&c| r.get(c).map(|cell| cell.to_value()))
                                    .collect::<Option<Vec<_>>>()
                                    .map(Row)
                            })
                        }
                    };
                    match row {
                        Some(row) => batch.push(row),
                        None => return Some(self.fail()),
                    }
                }
                self.next = end;
                if end >= total {
                    self.stage = Stage::Done;
                }
                if batch.is_empty() {
                    return self.next_feed(model, max_rows);
                }
                Some(Feed::Rows(batch))
            }
            Stage::Done => {
                self.stage = Stage::Finished;
                Some(Feed::Done)
            }
            Stage::Finished => None,
        }
    }
}

/// A sink that wraps another and fails after `limit` bytes. For tests.
#[cfg(test)]
struct FailAfter<W> {
    inner: W,
    left: usize,
}

#[cfg(test)]
impl<W: std::io::Write> std::io::Write for FailAfter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::other("disk full"));
        }
        let n = buf.len().min(self.left);
        self.left -= n;
        self.inner.write(&buf[..n])
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use sqler_driver_api::BoxFuture;
    use sqler_driver_api::sqler_core::{DataType, Value};
    use tokio::runtime::{Builder, Runtime};

    use super::*;
    use crate::export::file::scratch::Scratch;
    use crate::grid::{GridLimits, SyntheticSource, grid_channel_with, ingest};

    fn runtime() -> Runtime {
        Builder::new_current_thread().build().unwrap()
    }

    fn cols() -> Vec<Column> {
        vec![
            Column::new("id", DataType::Integer),
            Column::new("name", DataType::Text),
        ]
    }

    fn rows(from: i64, n: i64) -> Vec<Row> {
        (from..from + n)
            .map(|i| Row(vec![Value::Int(i), Value::Text(format!("n{i}"))]))
            .collect()
    }

    fn spec(dir: &Scratch, name: &str) -> WriteSpec {
        WriteSpec {
            format_id: "csv".to_owned(),
            options: ExportOptions::default(),
            path: dir.0.join(name),
            job: 1,
        }
    }

    fn no_progress() -> Box<dyn FnMut(Progress) + Send> {
        Box::new(|_| {})
    }

    /// Sends the messages from a thread and writes on this one.
    fn run_feed(spec: WriteSpec, stop: &ExportStop, feeds: Vec<Feed>) -> ExportOutcome {
        let (tx, rx) = mpsc::channel(feeds.len().max(1));
        for feed in feeds {
            tx.try_send(feed).unwrap();
        }
        drop(tx);
        write_feed(rx, spec, stop, no_progress())
    }

    #[test]
    fn a_complete_feed_writes_the_file() {
        let dir = Scratch::new("job-ok");
        let spec = spec(&dir, "out.csv");
        let path = spec.path.clone();
        let summary = run_feed(
            spec,
            &ExportStop::new(),
            vec![
                Feed::Columns(cols()),
                Feed::Rows(rows(0, 2)),
                Feed::Rows(rows(2, 1)),
                Feed::Done,
            ],
        )
        .unwrap();
        assert_eq!(summary.rows, 3);
        assert_eq!(summary.path, path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "id,name\r\n0,n0\r\n1,n1\r\n2,n2\r\n"
        );
        assert_eq!(summary.bytes, std::fs::metadata(&path).unwrap().len());
        assert_eq!(dir.names(), ["out.csv"]);
    }

    #[test]
    fn protocol_violations_leave_nothing_behind() {
        let dir = Scratch::new("job-protocol");
        let stop = ExportStop::new();
        let err = run_feed(
            spec(&dir, "a.csv"),
            &stop,
            vec![Feed::Columns(vec![]), Feed::Done],
        )
        .unwrap_err();
        assert_eq!(err, ExportFailure::NoResultSet);
        let err = run_feed(spec(&dir, "a.csv"), &stop, vec![Feed::Rows(rows(0, 1))]).unwrap_err();
        assert!(matches!(err, ExportFailure::Driver(_)), "{err:?}");
        let err = run_feed(
            spec(&dir, "a.csv"),
            &stop,
            vec![Feed::Columns(cols()), Feed::Columns(cols())],
        )
        .unwrap_err();
        assert!(matches!(err, ExportFailure::Driver(_)), "{err:?}");
        let err = run_feed(spec(&dir, "a.csv"), &stop, vec![Feed::Done]).unwrap_err();
        assert!(matches!(err, ExportFailure::Driver(_)), "{err:?}");
        // A row of the wrong width.
        let err = run_feed(
            spec(&dir, "a.csv"),
            &stop,
            vec![
                Feed::Columns(cols()),
                Feed::Rows(vec![Row(vec![Value::Null])]),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, ExportFailure::Format(_)), "{err:?}");
        dir.assert_empty();
    }

    #[test]
    fn a_closed_channel_is_source_ended_or_cancelled() {
        let dir = Scratch::new("job-closed");
        let stop = ExportStop::new();
        let err = run_feed(spec(&dir, "a.csv"), &stop, vec![Feed::Columns(cols())]).unwrap_err();
        assert_eq!(err, ExportFailure::SourceEnded);
        stop.cancel();
        let err = run_feed(spec(&dir, "a.csv"), &stop, vec![Feed::Columns(cols())]).unwrap_err();
        assert_eq!(err, ExportFailure::Cancelled);
        dir.assert_empty();
    }

    #[test]
    fn a_failed_producer_fails_the_export() {
        let dir = Scratch::new("job-failed");
        let err = run_feed(
            spec(&dir, "a.csv"),
            &ExportStop::new(),
            vec![
                Feed::Columns(cols()),
                Feed::Rows(rows(0, 5)),
                Feed::Failed(ExportFailure::Driver("boom".to_owned())),
            ],
        )
        .unwrap_err();
        assert_eq!(err, ExportFailure::Driver("boom".to_owned()));
        dir.assert_empty();
    }

    #[test]
    fn a_failed_run_leaves_the_existing_target_alone() {
        let dir = Scratch::new("job-keep");
        std::fs::write(dir.0.join("out.csv"), b"old").unwrap();
        let err = run_feed(
            spec(&dir, "out.csv"),
            &ExportStop::new(),
            vec![Feed::Columns(cols()), Feed::Failed(ExportFailure::Lost)],
        )
        .unwrap_err();
        assert_eq!(err, ExportFailure::Lost);
        assert_eq!(std::fs::read(dir.0.join("out.csv")).unwrap(), b"old");
        assert_eq!(dir.names(), ["out.csv"]);
    }

    #[test]
    fn cancel_mid_way_deletes_the_partial_file() {
        let dir = Scratch::new("job-cancel");
        let stop = ExportStop::new();
        let (tx, rx) = mpsc::channel(FEED_CAPACITY);
        let seen = Arc::new(AtomicU64::new(0));
        let progress: Box<dyn FnMut(Progress) + Send> = {
            let (seen, stop) = (Arc::clone(&seen), stop.clone());
            Box::new(move |p| {
                seen.store(p.rows, Ordering::SeqCst);
                stop.cancel();
            })
        };
        let spec = spec(&dir, "out.csv");
        let writer = {
            let stop = stop.clone();
            thread::spawn(move || write_feed(rx, spec, &stop, progress))
        };
        tx.blocking_send(Feed::Columns(cols())).unwrap();
        tx.blocking_send(Feed::Rows(rows(0, 10))).unwrap();
        // The writer raised the stop after the first batch; the next one is refused.
        let _ = tx.blocking_send(Feed::Rows(rows(10, 10)));
        let _ = tx.blocking_send(Feed::Done);
        let err = writer.join().unwrap().unwrap_err();
        assert_eq!(err, ExportFailure::Cancelled);
        assert!(seen.load(Ordering::SeqCst) >= 10);
        dir.assert_empty();
    }

    #[test]
    fn a_stop_before_the_start_creates_no_file() {
        let dir = Scratch::new("job-early");
        let stop = ExportStop::new();
        stop.cancel();
        let err = run_feed(spec(&dir, "out.csv"), &stop, vec![Feed::Columns(cols())]).unwrap_err();
        assert_eq!(err, ExportFailure::Cancelled);
        dir.assert_empty();
    }

    #[test]
    fn a_failing_sink_is_a_write_failure_and_the_partial_is_gone() {
        let dir = Scratch::new("job-sink");
        let spec = spec(&dir, "out.csv");
        let (tx, rx) = mpsc::channel(8);
        tx.try_send(Feed::Columns(cols())).unwrap();
        // More than the 64 KiB buffer, so the sink is reached before `finish`.
        for i in 0..4 {
            tx.try_send(Feed::Rows(rows(i * 2000, 2000))).unwrap();
        }
        drop(tx);
        let err = write_with(rx, spec, &ExportStop::new(), no_progress(), |file| {
            Box::new(FailAfter {
                inner: file,
                left: 1000,
            })
        })
        .unwrap_err();
        let ExportFailure::Write(text) = &err else {
            panic!("expected Write, got {err:?}");
        };
        assert!(text.contains("disk full"), "{text}");
        assert!(!text.contains("sqler-ui-export"), "{text}");
        dir.assert_empty();
    }

    #[test]
    fn an_unwritable_target_fails_before_any_row() {
        let dir = Scratch::new("job-create");
        let mut spec = spec(&dir, "out.csv");
        spec.path = dir.0.join("missing").join("out.csv");
        let (_tx, rx) = mpsc::channel(1);
        let err = write_feed(rx, spec, &ExportStop::new(), no_progress()).unwrap_err();
        let ExportFailure::Create(text) = &err else {
            panic!("expected Create, got {err:?}");
        };
        assert!(text.starts_with("cannot create the file"), "{text}");
        assert!(!text.contains("missing"), "{text}");
        // A folder as the target.
        let mut spec2 = self::spec(&dir, "x");
        spec2.path = dir.0.clone();
        let (_tx2, rx2) = mpsc::channel(1);
        assert!(matches!(
            write_feed(rx2, spec2, &ExportStop::new(), no_progress()),
            Err(ExportFailure::Create(_))
        ));
    }

    #[test]
    fn an_unknown_format_is_refused() {
        let dir = Scratch::new("job-format");
        let mut spec = spec(&dir, "out.x");
        spec.format_id = "nope".to_owned();
        let (_tx, rx) = mpsc::channel(1);
        let err = write_feed(rx, spec, &ExportStop::new(), no_progress()).unwrap_err();
        assert!(matches!(err, ExportFailure::Format(_)));
        dir.assert_empty();
    }

    #[test]
    fn invalid_options_do_not_echo_their_values() {
        let dir = Scratch::new("job-options");
        let mut spec = spec(&dir, "out.csv");
        spec.options.csv.null_text = "secret,value".to_owned();
        let (_tx, rx) = mpsc::channel(1);
        let err = write_feed(rx, spec, &ExportStop::new(), no_progress()).unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("secret"), "{text}");
        dir.assert_empty();
    }

    #[test]
    fn the_stop_wakes_async_waiters() {
        let stop = ExportStop::new();
        let waiter = {
            let stop = stop.clone();
            thread::spawn(move || runtime().block_on(stop.cancelled()))
        };
        thread::sleep(std::time::Duration::from_millis(20));
        assert!(!stop.is_cancelled());
        stop.cancel();
        waiter.join().unwrap();
        assert!(stop.token().is_cancelled());
        // Already raised: resolves at once.
        runtime().block_on(stop.cancelled());
    }

    /// A stream of `batches` batches of `size` rows that counts the rows it yields.
    struct Counting {
        stage: usize,
        batches: usize,
        size: usize,
        produced: Arc<AtomicU64>,
    }

    impl QueryStream for Counting {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            let stage = self.stage;
            self.stage += 1;
            let event = if stage == 0 {
                Some(Ok(QueryEvent::Columns(cols())))
            } else if stage <= self.batches {
                self.produced.fetch_add(self.size as u64, Ordering::SeqCst);
                let first = i64::try_from((stage - 1) * self.size).unwrap();
                Some(Ok(QueryEvent::Rows(rows(
                    first,
                    i64::try_from(self.size).unwrap(),
                ))))
            } else if stage == self.batches + 1 {
                Some(Ok(QueryEvent::Done {
                    rows_affected: None,
                }))
            } else {
                None
            };
            Box::pin(std::future::ready(event))
        }
    }

    #[test]
    fn memory_does_not_depend_on_the_row_count() {
        let dir = Scratch::new("job-memory");
        let produced = Arc::new(AtomicU64::new(0));
        let worst = Arc::new(AtomicU64::new(0));
        let bound = ((FEED_CAPACITY + 2) * 1000) as u64;
        let (tx, rx) = mpsc::channel(FEED_CAPACITY);
        let stop = ExportStop::new();
        let progress: Box<dyn FnMut(Progress) + Send> = {
            let (produced, worst) = (Arc::clone(&produced), Arc::clone(&worst));
            Box::new(move |p| {
                // A slow writer, so the producer is always ahead.
                thread::sleep(std::time::Duration::from_micros(200));
                let ahead = produced.load(Ordering::SeqCst).saturating_sub(p.rows);
                worst.fetch_max(ahead, Ordering::SeqCst);
                assert!(ahead <= bound, "{ahead} rows in flight, bound {bound}");
            })
        };
        let spec = spec(&dir, "big.csv");
        let writer = {
            let stop = stop.clone();
            thread::spawn(move || write_feed(rx, spec, &stop, progress))
        };
        let mut stream = Counting {
            stage: 0,
            batches: 1000,
            size: 1000,
            produced: Arc::clone(&produced),
        };
        runtime()
            .block_on(pump_stream(&mut stream, &tx, &stop))
            .unwrap();
        drop(tx);
        let summary = writer.join().unwrap().unwrap();
        assert_eq!(summary.rows, 1_000_000);
        assert_eq!(produced.load(Ordering::SeqCst), 1_000_000);
        // The producer really was ahead (the bound is not trivially met).
        assert!(worst.load(Ordering::SeqCst) >= 1000);
    }

    #[test]
    fn pump_stream_stops_when_the_channel_is_full_and_the_stop_is_raised() {
        let (tx, mut rx) = mpsc::channel(1);
        let stop = ExportStop::new();
        let mut stream = Counting {
            stage: 0,
            batches: 100,
            size: 10,
            produced: Arc::new(AtomicU64::new(0)),
        };
        let raiser = {
            let stop = stop.clone();
            thread::spawn(move || {
                thread::sleep(std::time::Duration::from_millis(30));
                stop.cancel();
            })
        };
        let err = runtime()
            .block_on(pump_stream(&mut stream, &tx, &stop))
            .unwrap_err();
        raiser.join().unwrap();
        assert_eq!(err, ExportFailure::Cancelled);
        assert!(rx.try_recv().is_ok());
    }

    /// A stream from a list of events.
    struct Script(std::vec::IntoIter<Result<QueryEvent, DriverError>>);

    impl QueryStream for Script {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            Box::pin(std::future::ready(self.0.next()))
        }
    }

    fn pump(
        events: Vec<Result<QueryEvent, DriverError>>,
    ) -> (Result<(), ExportFailure>, Vec<Feed>) {
        let (tx, mut rx) = mpsc::channel(32);
        let mut stream = Script(events.into_iter());
        let result = runtime().block_on(pump_stream(&mut stream, &tx, &ExportStop::new()));
        drop(tx);
        let mut fed = Vec::new();
        while let Ok(feed) = rx.try_recv() {
            fed.push(feed);
        }
        (result, fed)
    }

    #[test]
    fn pump_stream_checks_the_protocol() {
        let columns = || Ok(QueryEvent::Columns(cols()));
        let done = || {
            Ok(QueryEvent::Done {
                rows_affected: None,
            })
        };
        let batch = || Ok(QueryEvent::Rows(rows(0, 2)));

        let (r, fed) = pump(vec![columns(), batch(), done()]);
        assert!(r.is_ok());
        assert!(matches!(
            fed[..],
            [Feed::Columns(_), Feed::Rows(_), Feed::Done]
        ));

        for events in [
            vec![columns(), columns()],
            vec![batch()],
            vec![done()],
            vec![columns(), batch()],
        ] {
            let (r, _) = pump(events);
            assert!(matches!(r, Err(ExportFailure::Driver(_))), "{r:?}");
        }
        // Empty batches are skipped.
        let (r, fed) = pump(vec![columns(), Ok(QueryEvent::Rows(vec![])), done()]);
        assert!(r.is_ok());
        assert_eq!(fed.len(), 2);
        // A driver error ends it, a cancel is a cancel.
        let (r, _) = pump(vec![columns(), Err(DriverError::Query("bad".into()))]);
        assert!(matches!(r, Err(ExportFailure::Driver(t)) if t.contains("bad")));
        let (r, _) = pump(vec![columns(), Err(DriverError::Cancelled)]);
        assert_eq!(r, Err(ExportFailure::Cancelled));
        // A statement without a result set is passed on; the writer decides.
        let (r, fed) = pump(vec![Ok(QueryEvent::Columns(vec![])), done()]);
        assert!(r.is_ok());
        assert!(matches!(&fed[0], Feed::Columns(c) if c.is_empty()));
    }

    fn model(rows: usize, columns: usize) -> GridModel {
        let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
        let mut source = SyntheticSource::new(rows, columns, 7);
        runtime().block_on(ingest(&mut source, sink)).unwrap();
        model.poll(usize::MAX);
        assert_eq!(model.row_count(), rows);
        model
    }

    fn drain(cursor: &mut LoadedCursor, model: &GridModel, max: usize) -> Vec<Feed> {
        std::iter::from_fn(|| cursor.next_feed(model, max)).collect()
    }

    #[test]
    fn the_cursor_copies_all_rows_in_slices() {
        let model = model(25, 3);
        let mut cursor = LoadedCursor::new(LoadedRows::All, model.row_count());
        let feeds = drain(&mut cursor, &model, 10);
        // Columns, 10 + 10 + 5 rows, Done.
        assert_eq!(feeds.len(), 5);
        assert!(matches!(&feeds[0], Feed::Columns(c) if c.len() == 3));
        let sizes: Vec<usize> = feeds
            .iter()
            .filter_map(|f| {
                if let Feed::Rows(r) = f {
                    Some(r.len())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sizes, [10, 10, 5]);
        assert!(matches!(feeds[4], Feed::Done));
        let Feed::Rows(first) = &feeds[1] else {
            panic!("rows")
        };
        assert_eq!(first[3].0[0], Value::Int(3));
        assert!(cursor.next_feed(&model, 10).is_none());
    }

    #[test]
    fn the_cursor_is_fixed_at_the_row_count_it_was_given() {
        let model = model(25, 2);
        let mut cursor = LoadedCursor::new(LoadedRows::All, 12);
        let total: usize = drain(&mut cursor, &model, 100)
            .iter()
            .map(|f| if let Feed::Rows(r) = f { r.len() } else { 0 })
            .sum();
        assert_eq!(total, 12);
        // An empty result still gives Columns and Done.
        let empty = self::model(0, 2);
        let feeds = drain(&mut LoadedCursor::new(LoadedRows::All, 0), &empty, 10);
        assert!(matches!(feeds[..], [Feed::Columns(_), Feed::Done]));
    }

    #[test]
    fn the_cursor_projects_a_selection_in_display_order() {
        let model = model(10, 4);
        let picked = LoadedRows::Picked {
            rows: vec![5, 2, 9],
            columns: vec![3, 0],
        };
        let mut cursor = LoadedCursor::new(picked, model.row_count());
        let feeds = drain(&mut cursor, &model, 2);
        let Feed::Columns(c) = &feeds[0] else {
            panic!("columns")
        };
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["c3", "c0"]);
        let rows: Vec<Row> = feeds
            .into_iter()
            .filter_map(|f| if let Feed::Rows(r) = f { Some(r) } else { None })
            .flatten()
            .collect();
        assert_eq!(rows.len(), 3);
        // c3 = Bool(i is even), c0 = i.
        assert_eq!(rows[0].0, [Value::Bool(false), Value::Int(5)]);
        assert_eq!(rows[1].0, [Value::Bool(true), Value::Int(2)]);
        assert_eq!(rows[2].0, [Value::Bool(false), Value::Int(9)]);
    }

    #[test]
    fn a_selection_that_no_longer_fits_the_model_fails_instead_of_dropping_rows() {
        let model = model(10, 4);
        for picked in [
            // A row that is gone.
            LoadedRows::Picked {
                rows: vec![1, 10, 2],
                columns: vec![0],
            },
            // A column that is gone.
            LoadedRows::Picked {
                rows: vec![1],
                columns: vec![0, 4],
            },
        ] {
            let mut cursor = LoadedCursor::new(picked, model.row_count());
            let feeds = drain(&mut cursor, &model, 100);
            let last = feeds.last().unwrap();
            assert!(
                matches!(last, Feed::Failed(ExportFailure::ResultChanged)),
                "{last:?}"
            );
            assert!(!feeds.iter().any(|f| matches!(f, Feed::Done)));
            assert!(cursor.next_feed(&model, 10).is_none());
        }
        // Fewer rows than the cursor was told about.
        let mut cursor = LoadedCursor::new(LoadedRows::All, 30);
        let feeds = drain(&mut cursor, &model, 100);
        assert!(matches!(
            feeds.last(),
            Some(Feed::Failed(ExportFailure::ResultChanged))
        ));
    }

    #[test]
    fn the_failure_texts_name_no_target() {
        for failure in [
            ExportFailure::Cancelled,
            ExportFailure::NoResultSet,
            ExportFailure::SourceEnded,
            ExportFailure::Lost,
        ] {
            assert_ne!(failure.to_string(), "");
        }
        assert_eq!(
            ExportFailure::Connect("x".into()).to_string(),
            "cannot connect: x"
        );
    }

    #[test]
    fn connect_and_driver_texts_hold_no_target_path_or_secret() {
        use sqler_config::{SecretError, SecretKind};
        let secret = "hunter2-secret";
        let hostile = [
            ConnectFailure::Tunnel(format!("private key /home/u/.ssh/{secret}: bad")),
            ConnectFailure::Target(format!("postgres://u:{secret}@h/d")),
            ConnectFailure::Driver(DriverError::Connection(format!(
                "invalid PostgreSQL target: postgres://u:{secret}@h/d"
            ))),
            ConnectFailure::Driver(DriverError::Backend(
                format!("bad url postgres://u:{secret}@h/d").into(),
            )),
            ConnectFailure::Secret(SecretError::InvalidRef {
                input: secret.to_owned(),
                reason: "bad",
            }),
        ];
        for failure in &hostile {
            let text = connect_text(failure);
            assert!(!text.contains(secret), "{failure:?} -> {text}");
            assert!(!text.contains("postgres://"), "{text}");
            assert!(!text.contains(".ssh"), "{text}");
        }
        // The user-facing texts stay.
        assert_eq!(
            connect_text(&ConnectFailure::Driver(DriverError::Connection(
                "refused".into()
            ))),
            "connection failed: refused"
        );
        assert_eq!(
            driver_text(&DriverError::Query("syntax error at row 40,000".into())),
            "query failed: syntax error at row 40,000"
        );
        assert!(
            connect_text(&ConnectFailure::MissingSecret {
                kind: SecretKind::DatabasePassword
            })
            .contains("keychain")
        );
    }
}
