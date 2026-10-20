// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::mem::size_of;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use sqler_driver_api::CancelFlag;
use sqler_driver_api::sqler_core::{Column, Row, Value};

/// One message from ingestion to the model.
#[derive(Debug)]
pub(super) enum Msg {
    Columns(Vec<Column>),
    Rows(Vec<Row>),
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
        !matches!(self, Self::Waiting | Self::Streaming)
    }
}

/// Creates a connected model and sink that share one cancel flag.
#[must_use]
pub fn grid_channel() -> (GridModel, GridSink) {
    let (tx, rx) = mpsc::channel();
    let cancel = CancelFlag::new();
    let model = GridModel {
        rx,
        cancel: cancel.clone(),
        state: IngestState::Waiting,
        columns: Vec::new(),
        chunks: Vec::new(),
        starts: Vec::new(),
        rows: 0,
    };
    (model, GridSink { tx, cancel })
}

/// The ingestion side of a grid channel. Cloneable; sends never block.
#[derive(Debug, Clone)]
pub struct GridSink {
    tx: Sender<Msg>,
    cancel: CancelFlag,
}

impl GridSink {
    /// Whether the model asked to cancel.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The shared cancel flag, for example to give to a driver session.
    #[must_use]
    pub fn cancel_flag(&self) -> CancelFlag {
        self.cancel.clone()
    }

    /// Sends a message; `false` when the model was dropped.
    pub(super) fn send(&self, msg: Msg) -> bool {
        self.tx.send(msg).is_ok()
    }
}

/// The results model, owned by the UI thread.
///
/// Rows are stored as batches with their start offsets, so growth never copies earlier rows and a
/// viewport read is a binary search plus a slice walk.
#[derive(Debug)]
pub struct GridModel {
    rx: Receiver<Msg>,
    cancel: CancelFlag,
    state: IngestState,
    columns: Vec<Column>,
    chunks: Vec<Vec<Row>>,
    /// `starts[i]` is the index of the first row of `chunks[i]`.
    starts: Vec<usize>,
    rows: usize,
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
            Msg::Rows(rows) => {
                if rows.is_empty() {
                    return;
                }
                self.starts.push(self.rows);
                self.rows += rows.len();
                self.chunks.push(rows);
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
    pub fn row(&self, index: usize) -> Option<&Row> {
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

    /// Cancels ingestion: sets the shared flag and, unless the result already ended, moves the
    /// state to `Cancelled` at once. Later messages are discarded.
    pub fn cancel(&mut self) {
        self.cancel.cancel();
        if !self.state.is_terminal() {
            self.state = IngestState::Cancelled;
        }
    }

    /// An estimate of the heap bytes held by the rows. Walks every row: not for per-frame use.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let mut total = self.chunks.capacity() * size_of::<Vec<Row>>()
            + self.starts.capacity() * size_of::<usize>()
            + self.columns.capacity() * size_of::<Column>();
        for chunk in &self.chunks {
            total += chunk.capacity() * size_of::<Row>();
            for row in chunk {
                total += row.0.capacity() * size_of::<Value>();
                for value in &row.0 {
                    total += match value {
                        Value::Text(s) | Value::Json(s) => s.capacity(),
                        Value::Bytes(b) => b.capacity(),
                        Value::Decimal(d) => d.as_str().len(),
                        // `Value` is non-exhaustive; the other variants own no heap data.
                        _ => 0,
                    };
                }
            }
        }
        total
    }
}

fn row_at<'a>(chunks: &'a [Vec<Row>], starts: &[usize], index: usize) -> Option<&'a Row> {
    let c = starts.partition_point(|&s| s <= index).checked_sub(1)?;
    chunks[c].get(index - starts[c])
}

/// A read-only window of consecutive rows of a [`GridModel`].
#[derive(Debug, Clone, Copy)]
pub struct Viewport<'a> {
    chunks: &'a [Vec<Row>],
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
    pub fn get(&self, i: usize) -> Option<&'a Row> {
        if i >= self.len {
            return None;
        }
        row_at(self.chunks, self.starts, self.first + i)
    }

    /// The rows of the window in order.
    pub fn iter(&self) -> impl Iterator<Item = &'a Row> + '_ {
        let (head, tail): (&'a [Row], &'a [Vec<Row>]) = if self.len == 0 {
            (&[], &[])
        } else {
            (
                &self.chunks[self.chunk][self.offset..],
                &self.chunks[self.chunk + 1..],
            )
        };
        head.iter().chain(tail.iter().flatten()).take(self.len)
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::DataType;

    use super::*;

    fn rows(from: i64, n: i64) -> Vec<Row> {
        (from..from + n).map(|i| Row(vec![Value::Int(i)])).collect()
    }

    fn cols() -> Vec<Column> {
        vec![Column::new("a", DataType::Integer)]
    }

    fn int(row: &Row) -> i64 {
        match row.0[0] {
            Value::Int(i) => i,
            _ => -1,
        }
    }

    /// Two chunks: rows 0..5 and 5..8.
    fn model_with_two_chunks() -> (GridModel, GridSink) {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        assert!(s.send(Msg::Rows(rows(0, 5))));
        assert!(s.send(Msg::Rows(rows(5, 3))));
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
        assert!(s.send(Msg::Rows(vec![])));
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
        assert!(s.send(Msg::Rows(rows(0, 10))));
        assert_eq!(m.poll(usize::MAX), 2);
        assert_eq!(m.state(), &IngestState::Streaming);
        assert_eq!(m.row_count(), 10);
        assert_eq!(m.viewport(8, 5).len(), 2);
        assert!(s.send(Msg::Rows(rows(10, 5))));
        assert_eq!(m.poll(usize::MAX), 1);
        assert_eq!(m.viewport(8, 5).len(), 5);
    }

    #[test]
    fn poll_respects_max_and_idle() {
        let (mut m, s) = grid_channel();
        assert_eq!(m.poll(usize::MAX), 0);
        assert_eq!(m.state(), &IngestState::Waiting);
        assert!(s.send(Msg::Columns(cols())));
        assert!(s.send(Msg::Rows(rows(0, 1))));
        assert!(s.send(Msg::Rows(rows(1, 1))));
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
        assert!(s.send(Msg::Rows(rows(0, 4))));
        m.poll(usize::MAX);
        m.cancel();
        assert_eq!(m.state(), &IngestState::Cancelled);
        assert!(s.is_cancelled());
        assert!(s.cancel_flag().is_cancelled());
        assert!(s.send(Msg::Rows(rows(4, 4))));
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
        assert!(s.send(Msg::Rows(rows(0, 2))));
        assert!(s.send(Msg::Failed("boom".to_owned())));
        m.poll(usize::MAX);
        assert_eq!(m.state(), &IngestState::Failed("boom".to_owned()));
        assert_eq!(m.row_count(), 2);
    }

    #[test]
    fn heap_bytes_grows_with_rows() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        m.poll(usize::MAX);
        let empty = m.heap_bytes();
        assert!(s.send(Msg::Rows(vec![Row(vec![Value::Text("x".repeat(100))])])));
        m.poll(usize::MAX);
        let one = m.heap_bytes();
        assert!(one >= empty + 100);
        assert!(s.send(Msg::Rows(rows(0, 1000))));
        m.poll(usize::MAX);
        assert!(m.heap_bytes() > one + 1000 * size_of::<Value>());
    }

    #[test]
    fn heap_bytes_counts_json_and_decimal() {
        let (mut m, s) = grid_channel();
        assert!(s.send(Msg::Columns(cols())));
        m.poll(usize::MAX);
        let empty = m.heap_bytes();
        let json = Value::Json("j".repeat(500));
        let dec = Value::Decimal("1".repeat(300).parse().unwrap());
        assert!(s.send(Msg::Rows(vec![Row(vec![json, dec])])));
        m.poll(usize::MAX);
        assert!(m.heap_bytes() >= empty + 800);
    }

    #[test]
    fn model_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<GridModel>();
        assert_send::<GridSink>();
    }
}
