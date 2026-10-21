// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::future::{Future, poll_fn};
use std::mem::size_of;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Poll, Waker};

use sqler_driver_api::sqler_core::Column;
use sqler_driver_api::{CancelFlag, DriverError};

use super::store::{Chunk, RowRef};

/// One message from ingestion to the model.
#[derive(Debug)]
pub(super) enum Msg {
    Columns(Vec<Column>),
    Rows(Chunk),
    /// The page is full and more rows are known to exist.
    Paused,
    Done(Option<u64>),
    Cancelled,
    Failed(String),
}

/// Where ingestion stands, as seen by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IngestState {
    /// No columns have arrived yet.
    Waiting,
    /// Columns arrived; rows may still come.
    Streaming,
    /// The page is full and more rows are known to exist; [`GridModel::fetch_more`] continues.
    /// Not a terminal state.
    Paused,
    /// The result is complete.
    Done {
        /// Rows affected by the statement, if the backend reports it.
        rows_affected: Option<u64>,
    },
    /// Ingestion was cancelled.
    Cancelled,
    /// Ingestion failed; the text is the error message.
    Failed(String),
}

impl IngestState {
    fn is_terminal(&self) -> bool {
        !matches!(self, Self::Waiting | Self::Streaming | Self::Paused)
    }
}

/// How much one page of a result may hold.
///
/// Ingestion stops when a page is full (rows or bytes, whichever comes first) and continues
/// when [`GridModel::fetch_more`] grants the next page. The byte cap is checked row by row, so
/// it is overshot by at most one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridLimits {
    /// Rows per page.
    pub page_rows: usize,
    /// Bytes per page (a capacity estimate, see [`GridModel::heap_bytes`]).
    pub page_bytes: usize,
}

impl GridLimits {
    /// The byte cap of a page unless set otherwise: 64 MiB.
    pub const DEFAULT_PAGE_BYTES: usize = 64 * 1024 * 1024;

    /// No bound at all, for synthetic sources, benches and probes.
    pub const UNBOUNDED: Self = Self {
        page_rows: usize::MAX,
        page_bytes: usize::MAX,
    };

    /// Pages of `page_rows` rows (at least 1) and [`Self::DEFAULT_PAGE_BYTES`].
    #[must_use]
    pub fn rows(page_rows: usize) -> Self {
        Self {
            page_rows: page_rows.max(1),
            page_bytes: Self::DEFAULT_PAGE_BYTES,
        }
    }

    /// The limits the user configured (`result_page_rows`).
    #[must_use]
    pub fn from_settings(s: &sqler_config::Settings) -> Self {
        Self::rows(usize::try_from(s.result_page_rows).unwrap_or(usize::MAX))
    }
}

impl Default for GridLimits {
    fn default() -> Self {
        Self::rows(sqler_config::DEFAULT_RESULT_PAGE_ROWS as usize)
    }
}

/// The credit shared by model, sink and [`GridCancel`]. Locked only for O(1) updates.
#[derive(Debug)]
struct Gate {
    granted_rows: usize,
    granted_bytes: usize,
    sent_rows: usize,
    sent_bytes: usize,
    closed: bool,
    waker: Option<Waker>,
}

type SharedGate = Arc<Mutex<Gate>>;

fn lock(gate: &Mutex<Gate>) -> MutexGuard<'_, Gate> {
    gate.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Wakes the paused ingestion, if there is one. The waker runs outside the lock.
fn wake(gate: &Mutex<Gate>) {
    let waker = lock(gate).waker.take();
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// Creates a connected model and sink with [`GridLimits::default`].
#[must_use]
pub fn grid_channel() -> (GridModel, GridSink) {
    grid_channel_with(GridLimits::default())
}

/// Creates a connected model and sink that share one cancel flag and one page of credit.
#[must_use]
pub fn grid_channel_with(limits: GridLimits) -> (GridModel, GridSink) {
    let (tx, rx) = mpsc::channel();
    let cancel = CancelFlag::new();
    let gate = Arc::new(Mutex::new(Gate {
        granted_rows: limits.page_rows,
        granted_bytes: limits.page_bytes,
        sent_rows: 0,
        sent_bytes: 0,
        closed: false,
        waker: None,
    }));
    let model = GridModel {
        rx,
        cancel: cancel.clone(),
        gate: Arc::clone(&gate),
        limits,
        state: IngestState::Waiting,
        columns: Vec::new(),
        chunks: Vec::new(),
        starts: Vec::new(),
        rows: 0,
    };
    (model, GridSink { tx, cancel, gate })
}

/// The ingestion side of a grid channel. Cloneable; sends never block.
#[derive(Debug, Clone)]
pub struct GridSink {
    tx: Sender<Msg>,
    cancel: CancelFlag,
    gate: SharedGate,
}

impl GridSink {
    /// Whether the model asked to cancel.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The shared cancel flag, for example to give to a driver session.
    ///
    /// Setting only the flag does not wake an ingestion that is paused on a full page; use
    /// [`GridSink::canceller`] for that.
    #[must_use]
    pub fn cancel_flag(&self) -> CancelFlag {
        self.cancel.clone()
    }

    /// A handle that cancels ingestion and wakes it when it is paused.
    #[must_use]
    pub fn canceller(&self) -> GridCancel {
        GridCancel {
            cancel: self.cancel.clone(),
            gate: Arc::clone(&self.gate),
        }
    }

    /// Sends a message; `false` when the model was dropped.
    pub(super) fn send(&self, msg: Msg) -> bool {
        self.tx.send(msg).is_ok()
    }

    /// The credit now: `Some(Ok(rows))` when rows may be sent, `Some(Err(Cancelled))` when the model
    /// was dropped, and `None` when the page is full. The cancel flag is not looked at: a batch the
    /// stream already yielded is still delivered, and the loop checks the flag before it reads on.
    pub(super) fn try_credit(&self) -> Option<Result<usize, DriverError>> {
        Self::credit_of(&lock(&self.gate))
    }

    fn credit_of(gate: &Gate) -> Option<Result<usize, DriverError>> {
        if gate.closed {
            return Some(Err(DriverError::Cancelled));
        }
        (gate.sent_rows < gate.granted_rows && gate.sent_bytes < gate.granted_bytes)
            .then(|| Ok(gate.granted_rows - gate.sent_rows))
    }

    /// The bytes still free under the page's byte cap (0 when it is reached), or `None` when the
    /// sink has no byte cap ([`GridLimits::UNBOUNDED`]), which holds for the whole life of the sink.
    pub(super) fn byte_room(&self) -> Option<usize> {
        let gate = lock(&self.gate);
        (gate.granted_bytes != usize::MAX)
            .then(|| gate.granted_bytes.saturating_sub(gate.sent_bytes))
    }

    /// Waits for credit: resolves to the rows that may be sent, or to [`DriverError::Cancelled`]
    /// when the model was dropped or the cancel flag is set. Stays pending while the page is full;
    /// [`GridModel::fetch_more`], a cancel through the model or a [`GridCancel`], or dropping the
    /// model wake it.
    pub(super) fn credit(&self) -> impl Future<Output = Result<usize, DriverError>> + '_ {
        poll_fn(|cx| {
            let mut gate = lock(&self.gate);
            if self.cancel.is_cancelled() {
                return Poll::Ready(Err(DriverError::Cancelled));
            }
            if let Some(ready) = Self::credit_of(&gate) {
                return Poll::Ready(ready);
            }
            gate.waker = Some(cx.waker().clone());
            Poll::Pending
        })
    }

    /// Records rows and bytes handed to the model.
    pub(super) fn record(&self, rows: usize, bytes: usize) {
        let mut gate = lock(&self.gate);
        gate.sent_rows = gate.sent_rows.saturating_add(rows);
        gate.sent_bytes = gate.sent_bytes.saturating_add(bytes);
    }
}

/// Cancels one ingestion from any thread and wakes it if it waits for credit. Cloneable.
#[derive(Debug, Clone)]
pub struct GridCancel {
    cancel: CancelFlag,
    gate: SharedGate,
}

impl GridCancel {
    /// Sets the cancel flag and wakes a paused ingestion, which then ends as `Cancelled`.
    pub fn cancel(&self) {
        self.cancel.cancel();
        wake(&self.gate);
    }

    /// Whether the flag is set.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// The results model, owned by the UI thread.
///
/// Rows are stored compactly as chunks with their start offsets, so growth never copies earlier
/// rows and a viewport read is a binary search plus a walk. Dropping the model frees the rows and
/// wakes a paused ingestion, which then ends.
pub struct GridModel {
    rx: Receiver<Msg>,
    cancel: CancelFlag,
    gate: SharedGate,
    limits: GridLimits,
    state: IngestState,
    columns: Vec<Column>,
    chunks: Vec<Chunk>,
    /// `starts[i]` is the index of the first row of `chunks[i]`.
    starts: Vec<usize>,
    rows: usize,
}

/// The state, the columns and the sizes: never the cells, so a log line holds no row data.
impl std::fmt::Debug for GridModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GridModel")
            .field("state", &self.state)
            .field("limits", &self.limits)
            .field("columns", &self.columns)
            .field("rows", &self.rows)
            .field("chunks", &self.chunks.len())
            .finish_non_exhaustive()
    }
}

impl Drop for GridModel {
    fn drop(&mut self) {
        lock(&self.gate).closed = true;
        wake(&self.gate);
    }
}

impl GridModel {
    /// Applies up to `max_messages` pending messages without ever waiting; returns how many were
    /// applied. After a cancel or a terminal state, remaining messages are discarded and not
    /// counted. If the sink is dropped before a terminal message, the state becomes `Failed`.
    pub fn poll(&mut self, max_messages: usize) -> usize {
        let mut applied = 0;
        while applied < max_messages {
            if self.state.is_terminal() {
                while self.rx.try_recv().is_ok() {}
                break;
            }
            match self.rx.try_recv() {
                Ok(msg) => {
                    self.apply(msg);
                    applied += 1;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.state = IngestState::Failed("ingestion ended unexpectedly".to_owned());
                    break;
                }
            }
        }
        applied
    }

    fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Columns(columns) => {
                self.columns = columns;
                if self.state == IngestState::Waiting {
                    self.state = IngestState::Streaming;
                }
            }
            Msg::Rows(chunk) => {
                if chunk.rows() == 0 {
                    return;
                }
                self.starts.push(self.rows);
                self.rows += chunk.rows();
                self.chunks.push(chunk);
            }
            Msg::Paused => {
                if self.state == IngestState::Streaming {
                    self.state = IngestState::Paused;
                }
            }
            Msg::Done(rows_affected) => self.state = IngestState::Done { rows_affected },
            Msg::Cancelled => self.state = IngestState::Cancelled,
            Msg::Failed(text) => self.state = IngestState::Failed(text),
        }
    }

    /// The ingestion state.
    #[must_use]
    pub fn state(&self) -> &IngestState {
        &self.state
    }

    /// The page limits this model was created with.
    #[must_use]
    pub fn limits(&self) -> GridLimits {
        self.limits
    }

    /// The result columns (empty until they arrive).
    #[must_use]
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// Number of rows received so far.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows
    }

    /// The row at `index`, if received.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<RowRef<'_>> {
        row_at(&self.chunks, &self.starts, index)
    }

    /// The rows `first..first + count`, clamped to the rows received so far.
    #[must_use]
    pub fn viewport(&self, first: usize, count: usize) -> Viewport<'_> {
        let first = first.min(self.rows);
        let len = count.min(self.rows - first);
        let (chunk, offset) = if len == 0 {
            (0, 0)
        } else {
            let c = self.starts.partition_point(|&s| s <= first) - 1;
            (c, first - self.starts[c])
        };
        Viewport {
            chunks: &self.chunks,
            starts: &self.starts,
            first,
            len,
            chunk,
            offset,
        }
    }

    /// Grants the next page and resumes ingestion. Only a paused model can fetch more: returns
    /// `true` and moves to `Streaming`; in any other state it returns `false` and changes
    /// nothing.
    pub fn fetch_more(&mut self) -> bool {
        if self.state != IngestState::Paused {
            return false;
        }
        {
            let mut gate = lock(&self.gate);
            gate.granted_rows = gate.sent_rows.saturating_add(self.limits.page_rows);
            gate.granted_bytes = gate.sent_bytes.saturating_add(self.limits.page_bytes);
        }
        self.state = IngestState::Streaming;
        wake(&self.gate);
        true
    }

    /// Cancels ingestion: sets the shared flag, wakes a paused ingestion and, unless the result
    /// already ended, moves the state to `Cancelled` at once. Later messages are discarded.
    pub fn cancel(&mut self) {
        self.cancel.cancel();
        if !self.state.is_terminal() {
            self.state = IngestState::Cancelled;
        }
        wake(&self.gate);
    }

    /// An estimate of the heap bytes held by the rows. Sums chunk capacities: not for per-frame
    /// use.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.chunks.capacity() * size_of::<Chunk>()
            + self.starts.capacity() * size_of::<usize>()
            + self.columns.capacity() * size_of::<Column>()
            + self.chunks.iter().map(Chunk::heap_bytes).sum::<usize>()
    }
}

fn row_at<'a>(chunks: &'a [Chunk], starts: &[usize], index: usize) -> Option<RowRef<'a>> {
    let c = starts.partition_point(|&s| s <= index).checked_sub(1)?;
    chunks[c].row(index - starts[c])
}

/// A read-only window of consecutive rows of a [`GridModel`].
#[derive(Debug, Clone, Copy)]
pub struct Viewport<'a> {
    chunks: &'a [Chunk],
    starts: &'a [usize],
    first: usize,
    len: usize,
    chunk: usize,
    offset: usize,
}

impl<'a> Viewport<'a> {
    /// Index of the first row in the window (clamped to the row count).
    #[must_use]
    pub fn first(&self) -> usize {
        self.first
    }

    /// Number of rows in the window.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the window has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The `i`-th row of the window.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<RowRef<'a>> {
        if i >= self.len {
            return None;
        }
        row_at(self.chunks, self.starts, self.first + i)
    }

    /// The rows of the window in order.
    pub fn iter(&self) -> impl Iterator<Item = RowRef<'a>> + 'a {
        let (chunks, offset): (&'a [Chunk], usize) = if self.len == 0 {
            (&[], 0)
        } else {
            (&self.chunks[self.chunk..], self.offset)
        };
        chunks
            .iter()
            .enumerate()
            .flat_map(move |(i, c)| {
                let from = if i == 0 { offset } else { 0 };
                (from..c.rows()).map(move |r| RowRef::new(c, r))
            })
            .take(self.len)
    }
}

#[cfg(test)]
mod tests {
    use std::task::Context;

    use sqler_driver_api::sqler_core::{DataType, Row, Value};

    use super::*;

    fn rows(from: i64, n: i64) -> Msg {
        let rows = (from..from + n).map(|i| Row(vec![Value::Int(i)])).collect();
        Msg::Rows(Chunk::from_rows(rows, 1))
    }

    fn cols() -> Vec<Column> {
        vec![Column::new("a", DataType::Integer)]
    }

    fn int(row: RowRef<'_>) -> i64 {
        match row.get(0) {
            Some(super::super::CellRef::Int(i)) => i,
            _ => -1,
        }
    }

    /// Two chunks: rows 0..5 and 5..8.
    fn model_with_two_chunks() -> (GridModel, GridSink) {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        assert!(s.send(rows(0, 5)));
        assert!(s.send(rows(5, 3)));
        assert!(s.send(Msg::Done(None)));
        assert_eq!(m.poll(usize::MAX), 4);
        (m, s)
    }

    #[test]
    fn viewport_bounds() {
        let (m, _s) = model_with_two_chunks();
        assert_eq!(m.row_count(), 8);
        let v = m.viewport(2, 3);
        assert_eq!((v.first(), v.len()), (2, 3));
        let v = m.viewport(6, 10);
        assert_eq!((v.first(), v.len()), (6, 2));
        let v = m.viewport(100, 5);
        assert_eq!((v.first(), v.len()), (8, 0));
        assert!(v.is_empty());
        assert!(v.get(0).is_none());
        assert_eq!(v.iter().count(), 0);
        let v = m.viewport(3, 0);
        assert!(v.is_empty());
        assert_eq!(v.iter().count(), 0);
        let v = m.viewport(8, 1);
        assert!(v.is_empty());
        assert!(m.row(8).is_none());
        assert_eq!(int(m.row(7).unwrap()), 7);
    }

    #[test]
    fn viewport_spans_chunks() {
        let (m, _s) = model_with_two_chunks();
        let v = m.viewport(3, 4);
        let got: Vec<i64> = v.iter().map(int).collect();
        assert_eq!(got, [3, 4, 5, 6]);
        assert_eq!(int(v.get(1).unwrap()), 4);
        assert_eq!(int(v.get(2).unwrap()), 5);
        assert!(v.get(4).is_none());
        let got: Vec<i64> = m.viewport(0, 100).iter().map(int).collect();
        assert_eq!(got, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn empty_results() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(vec![])));
        assert!(s.send(Msg::Done(Some(3))));
        m.poll(usize::MAX);
        assert_eq!(
            m.state(),
            &IngestState::Done {
                rows_affected: Some(3)
            }
        );
        assert_eq!(m.columns().len(), 0);
        assert_eq!(m.row_count(), 0);
        assert!(m.viewport(0, 10).is_empty());

        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        assert!(s.send(Msg::Rows(Chunk::from_rows(vec![], 1))));
        assert!(s.send(Msg::Done(None)));
        m.poll(usize::MAX);
        assert_eq!(m.columns().len(), 1);
        assert_eq!(m.row_count(), 0);
        assert!(m.viewport(0, 10).is_empty());
        assert!(matches!(m.state(), IngestState::Done { .. }));
    }

    #[test]
    fn partial_ingestion() {
        let (mut m, s) = grid_channel();
        assert_eq!(m.state(), &IngestState::Waiting);
        assert!(s.send(Msg::Columns(cols())));
        assert!(s.send(rows(0, 10)));
        assert_eq!(m.poll(usize::MAX), 2);
        assert_eq!(m.state(), &IngestState::Streaming);
        assert_eq!(m.row_count(), 10);
        assert_eq!(m.viewport(8, 5).len(), 2);
        assert!(s.send(rows(10, 5)));
        assert_eq!(m.poll(usize::MAX), 1);
        assert_eq!(m.viewport(8, 5).len(), 5);
    }

    #[test]
    fn poll_respects_max_and_idle() {
        let (mut m, s) = grid_channel();
        assert_eq!(m.poll(usize::MAX), 0);
        assert_eq!(m.state(), &IngestState::Waiting);
        assert!(s.send(Msg::Columns(cols())));
        assert!(s.send(rows(0, 1)));
        assert!(s.send(rows(1, 1)));
        assert_eq!(m.poll(2), 2);
        assert_eq!(m.row_count(), 1);
        assert_eq!(m.poll(0), 0);
        assert_eq!(m.poll(usize::MAX), 1);
        assert_eq!(m.poll(usize::MAX), 0);
    }

    #[test]
    fn cancel_discards_later_batches() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        assert!(s.send(rows(0, 4)));
        m.poll(usize::MAX);
        m.cancel();
        assert_eq!(m.state(), &IngestState::Cancelled);
        assert!(s.is_cancelled());
        assert!(s.cancel_flag().is_cancelled());
        assert!(s.send(rows(4, 4)));
        assert!(s.send(Msg::Done(None)));
        assert_eq!(m.poll(usize::MAX), 0);
        assert_eq!(m.row_count(), 4);
        assert_eq!(m.state(), &IngestState::Cancelled);
    }

    #[test]
    fn cancel_after_done_keeps_done() {
        let (mut m, s) = model_with_two_chunks();
        m.cancel();
        assert!(matches!(m.state(), IngestState::Done { .. }));
        assert!(s.is_cancelled());
    }

    #[test]
    fn dropped_sink_without_terminal_fails() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        drop(s);
        m.poll(usize::MAX);
        assert!(matches!(m.state(), IngestState::Failed(_)));
    }

    #[test]
    fn failed_state_keeps_rows() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        assert!(s.send(rows(0, 2)));
        assert!(s.send(Msg::Failed("boom".to_owned())));
        m.poll(usize::MAX);
        assert_eq!(m.state(), &IngestState::Failed("boom".to_owned()));
        assert_eq!(m.row_count(), 2);
    }

    #[test]
    fn debug_output_holds_no_cell_data() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        let secret = "SECRET-CELL-VALUE";
        let row = vec![Row(vec![Value::Text(secret.to_owned())])];
        assert!(s.send(Msg::Rows(Chunk::from_rows(row, 1))));
        m.poll(usize::MAX);
        assert_eq!(m.row_count(), 1);
        let text = format!("{m:?}");
        assert!(
            text.contains("GridModel") && text.contains("rows: 1"),
            "{text}"
        );
        assert!(!text.contains(secret), "{text}");
    }

    #[test]
    fn heap_bytes_grows_with_rows() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        m.poll(usize::MAX);
        let empty = m.heap_bytes();
        let one_row = vec![Row(vec![Value::Text("x".repeat(100))])];
        assert!(s.send(Msg::Rows(Chunk::from_rows(one_row, 1))));
        m.poll(usize::MAX);
        let one = m.heap_bytes();
        assert!(one >= empty + 100);
        assert!(s.send(rows(0, 1000)));
        m.poll(usize::MAX);
        assert!(m.heap_bytes() >= one + 1000 * 16);
    }

    #[test]
    fn heap_bytes_counts_json_and_decimal() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        m.poll(usize::MAX);
        let empty = m.heap_bytes();
        let json = Value::Json("j".repeat(500));
        let dec = Value::Decimal("1".repeat(300).parse().unwrap());
        let chunk = Chunk::from_rows(vec![Row(vec![json, dec])], 2);
        assert!(s.send(Msg::Rows(chunk)));
        m.poll(usize::MAX);
        assert!(m.heap_bytes() >= empty + 800);
    }

    #[test]
    fn model_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<GridModel>();
        assert_send::<GridSink>();
        assert_send::<GridCancel>();
    }

    #[test]
    fn byte_room_is_none_for_unbounded_sinks_even_after_records() {
        let (_model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
        assert_eq!(sink.byte_room(), None);
        sink.record(10, 4_096);
        assert_eq!(sink.byte_room(), None);
        let (model, sink) = grid_channel_with(GridLimits {
            page_rows: 100,
            page_bytes: 1_000,
        });
        assert_eq!(sink.byte_room(), Some(1_000));
        sink.record(1, 400);
        assert_eq!(sink.byte_room(), Some(600));
        sink.record(1, 900);
        assert_eq!(sink.byte_room(), Some(0));
        drop(model);
    }

    #[test]
    fn limits_constructors() {
        assert_eq!(GridLimits::rows(0).page_rows, 1);
        assert_eq!(
            GridLimits::rows(7).page_bytes,
            GridLimits::DEFAULT_PAGE_BYTES
        );
        assert_eq!(GridLimits::default().page_rows, 100_000);
        assert_eq!(GridLimits::UNBOUNDED.page_rows, usize::MAX);
        let mut settings = sqler_config::Settings::default();
        settings.result_page_rows = 2_500;
        assert_eq!(
            GridLimits::from_settings(&settings),
            GridLimits::rows(2_500)
        );
        let (m, _s) = grid_channel_with(GridLimits::rows(3));
        assert_eq!(m.limits(), GridLimits::rows(3));
    }

    /// Polls the credit future once with a counting waker.
    fn poll_credit(s: &GridSink) -> (Poll<Result<usize, DriverError>>, Arc<WakeCount>) {
        let count = Arc::new(WakeCount::default());
        let waker = Waker::from(Arc::clone(&count));
        let mut cx = Context::from_waker(&waker);
        let mut fut = std::pin::pin!(s.credit());
        let res = fut.as_mut().poll(&mut cx);
        (res, count)
    }

    #[derive(Default)]
    struct WakeCount(std::sync::atomic::AtomicUsize);

    impl WakeCount {
        fn get(&self) -> usize {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl std::task::Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn credit_runs_out_pauses_and_fetch_more_resumes() {
        let (mut m, s) = grid_channel_with(GridLimits::rows(4));
        assert!(s.send(Msg::Columns(cols())));
        m.poll(usize::MAX);
        assert!(matches!(poll_credit(&s).0, Poll::Ready(Ok(4))));
        s.record(3, 30);
        assert!(matches!(poll_credit(&s).0, Poll::Ready(Ok(1))));
        s.record(1, 10);
        let (res, count) = poll_credit(&s);
        assert!(res.is_pending());
        assert!(s.send(Msg::Paused));
        m.poll(usize::MAX);
        assert_eq!(m.state(), &IngestState::Paused);
        assert!(m.fetch_more());
        assert_eq!(m.state(), &IngestState::Streaming);
        assert_eq!(count.get(), 1);
        assert!(matches!(poll_credit(&s).0, Poll::Ready(Ok(4))));
        // Not paused any more: nothing to fetch.
        assert!(!m.fetch_more());
    }

    #[test]
    fn byte_cap_pauses_too() {
        let limits = GridLimits {
            page_rows: 1_000,
            page_bytes: 100,
        };
        let (_m, s) = grid_channel_with(limits);
        s.record(2, 150);
        assert!(poll_credit(&s).0.is_pending());
    }

    #[test]
    fn fetch_more_outside_paused_is_false() {
        let (mut m, s) = grid_channel_with(GridLimits::rows(2));
        assert!(!m.fetch_more());
        assert!(s.send(Msg::Columns(cols())));
        m.poll(usize::MAX);
        assert!(!m.fetch_more());
        assert!(s.send(Msg::Done(None)));
        m.poll(usize::MAX);
        assert!(!m.fetch_more());
        let (mut m, _s) = grid_channel();
        m.cancel();
        assert!(!m.fetch_more());
    }

    #[test]
    fn cancel_while_paused_wakes() {
        let (mut m, s) = grid_channel_with(GridLimits::rows(1));
        s.record(1, 1);
        let (res, count) = poll_credit(&s);
        assert!(res.is_pending());
        m.cancel();
        assert_eq!(count.get(), 1);
        assert!(matches!(
            poll_credit(&s).0,
            Poll::Ready(Err(DriverError::Cancelled))
        ));
    }

    #[test]
    fn canceller_wakes_but_the_bare_flag_does_not() {
        let (_m, s) = grid_channel_with(GridLimits::rows(1));
        s.record(1, 1);
        let (res, count) = poll_credit(&s);
        assert!(res.is_pending());
        s.cancel_flag().cancel();
        assert_eq!(count.get(), 0);
        let canceller = s.canceller();
        assert!(canceller.is_cancelled());
        let (res, count) = poll_credit(&s);
        assert!(matches!(res, Poll::Ready(Err(DriverError::Cancelled))));
        assert_eq!(count.get(), 0);

        let (_m, s) = grid_channel_with(GridLimits::rows(1));
        s.record(1, 1);
        let canceller = s.canceller();
        let (res, count) = poll_credit(&s);
        assert!(res.is_pending());
        canceller.cancel();
        assert_eq!(count.get(), 1);
        assert!(s.is_cancelled());
    }

    #[test]
    fn drop_while_paused_wakes() {
        let (m, s) = grid_channel_with(GridLimits::rows(1));
        s.record(1, 1);
        let (res, count) = poll_credit(&s);
        assert!(res.is_pending());
        drop(m);
        assert_eq!(count.get(), 1);
        assert!(matches!(
            poll_credit(&s).0,
            Poll::Ready(Err(DriverError::Cancelled))
        ));
    }
}
