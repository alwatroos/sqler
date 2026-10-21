// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Compact row storage for the results model.
//!
//! A [`Chunk`] holds the rows of one ingested batch in a single row-major `Vec<Cell>`. A cell is
//! 16 bytes; text, JSON and binary payloads live in per-chunk arenas, and the rare variants
//! (decimal, dates and times, values past the 4 GiB arena limit) sit in a side `Vec<Value>`.
//! Readers get [`CellRef`]s that borrow from the chunk, so reading allocates nothing per cell.

use std::fmt;
use std::mem::size_of;

use sqler_driver_api::sqler_core::{Row, Value};

/// A byte range inside one of the chunk arenas.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: u32,
    len: u32,
}

/// One stored cell: 16 bytes.
#[derive(Debug, Clone, Copy)]
enum Cell {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A range of [`Chunk::text`].
    Text(Span),
    /// A range of [`Chunk::text`].
    Json(Span),
    /// A range of [`Chunk::bytes`].
    Bytes(Span),
    /// An index into [`Chunk::other`].
    Other(u32),
}

/// A borrowed view of one cell. Reading a cell never allocates.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum CellRef<'a> {
    /// SQL `NULL`.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit integer.
    Int(i64),
    /// 64-bit float.
    Float(f64),
    /// Text.
    Text(&'a str),
    /// JSON document as raw text.
    Json(&'a str),
    /// Binary data.
    Bytes(&'a [u8]),
    /// Any other value (decimal, date, time, timestamp), kept as it arrived.
    Value(&'a Value),
}

impl CellRef<'_> {
    /// An owned copy of the cell.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match *self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(b),
            Self::Int(i) => Value::Int(i),
            Self::Float(x) => Value::Float(x),
            Self::Text(s) => Value::Text(s.to_owned()),
            Self::Json(s) => Value::Json(s.to_owned()),
            Self::Bytes(b) => Value::Bytes(b.to_vec()),
            Self::Value(v) => v.clone(),
        }
    }

    /// Whether the cell is SQL `NULL`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

/// Prints exactly what the `Display` of the equivalent [`Value`] prints.
impl fmt::Display for CellRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(x) => write!(f, "{x}"),
            Self::Text(s) | Self::Json(s) => f.write_str(s),
            Self::Bytes(bytes) => {
                f.write_str("0x")?;
                for b in *bytes {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
            Self::Value(v) => fmt::Display::fmt(v, f),
        }
    }
}

/// A borrowed view of one row of the model.
#[derive(Debug, Clone, Copy)]
pub struct RowRef<'a> {
    chunk: &'a Chunk,
    row: usize,
}

impl<'a> RowRef<'a> {
    pub(super) fn new(chunk: &'a Chunk, row: usize) -> Self {
        Self { chunk, row }
    }

    /// Number of cells.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunk.width
    }

    /// Whether the row has no cells.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunk.width == 0
    }

    /// The cell in column `col`.
    #[must_use]
    pub fn get(&self, col: usize) -> Option<CellRef<'a>> {
        if col >= self.chunk.width {
            return None;
        }
        let cell = self.chunk.cells.get(self.row * self.chunk.width + col)?;
        Some(self.chunk.resolve(cell))
    }

    /// The cells in column order.
    pub fn iter(&self) -> impl Iterator<Item = CellRef<'a>> + 'a {
        let chunk = self.chunk;
        let start = (self.row * chunk.width).min(chunk.cells.len());
        let end = (start + chunk.width).min(chunk.cells.len());
        chunk.cells[start..end].iter().map(|c| chunk.resolve(c))
    }

    /// An owned copy of the row.
    #[must_use]
    pub fn to_row(&self) -> Row {
        Row(self.iter().map(|c| c.to_value()).collect())
    }
}

/// The rows of one ingested batch.
pub(super) struct Chunk {
    cells: Vec<Cell>,
    text: String,
    bytes: Vec<u8>,
    other: Vec<Value>,
    rows: usize,
    width: usize,
}

/// Sizes only: a log line never carries cell data.
impl std::fmt::Debug for Chunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chunk")
            .field("rows", &self.rows)
            .field("width", &self.width)
            .field("text_bytes", &self.text.len())
            .field("binary_bytes", &self.bytes.len())
            .field("other_values", &self.other.len())
            .finish_non_exhaustive()
    }
}

/// Whether a payload of `len` bytes still fits an arena that holds `used` bytes, with every
/// offset and length representable as `u32`.
fn fits(used: usize, len: usize) -> bool {
    u32::try_from(used)
        .ok()
        .zip(u32::try_from(len).ok())
        .is_some_and(|(u, l)| u.checked_add(l).is_some())
}

/// The heap a row will cost once packed into a chunk of `width` columns: its cells plus its text,
/// byte and side-table payloads. It matches [`Chunk::heap_bytes`] for exactly sized arenas, so the
/// byte cap can be applied inside a batch.
pub(super) fn packed_cost(row: &Row, width: usize) -> usize {
    let mut total = width * size_of::<Cell>();
    for v in row.0.iter().take(width) {
        total += match v {
            Value::Text(s) | Value::Json(s) => s.len(),
            Value::Bytes(b) => b.len(),
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => 0,
            Value::Decimal(d) => size_of::<Value>() + d.as_str().len(),
            _ => size_of::<Value>(),
        };
    }
    total
}

impl Chunk {
    /// Packs `rows` (each `width` values wide) into one chunk. The arenas are sized exactly.
    pub(super) fn from_rows(rows: Vec<Row>, width: usize) -> Self {
        // Pre-pass: the exact arena sizes, with the same overflow rule as the fill below.
        let (mut text_len, mut bytes_len, mut other_len) = (0usize, 0usize, 0usize);
        for row in &rows {
            for v in row.0.iter().take(width) {
                match v {
                    Value::Text(s) | Value::Json(s) if fits(text_len, s.len()) => {
                        text_len += s.len();
                    }
                    Value::Bytes(b) if fits(bytes_len, b.len()) => bytes_len += b.len(),
                    Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => {}
                    _ => other_len += 1,
                }
            }
        }
        let count = rows.len();
        let mut chunk = Self {
            cells: Vec::with_capacity(count * width),
            text: String::with_capacity(text_len),
            bytes: Vec::with_capacity(bytes_len),
            other: Vec::with_capacity(other_len),
            rows: count,
            width,
        };
        for row in rows {
            let mut values = row.0.into_iter();
            for _ in 0..width {
                let cell = match values.next() {
                    Some(v) => chunk.push_value(v),
                    None => Cell::Null,
                };
                chunk.cells.push(cell);
            }
        }
        chunk
    }

    fn push_value(&mut self, v: Value) -> Cell {
        match v {
            Value::Null => Cell::Null,
            Value::Bool(b) => Cell::Bool(b),
            Value::Int(i) => Cell::Int(i),
            Value::Float(x) => Cell::Float(x),
            Value::Text(s) if fits(self.text.len(), s.len()) => {
                Cell::Text(Self::span(&mut self.text, &s))
            }
            Value::Json(s) if fits(self.text.len(), s.len()) => {
                Cell::Json(Self::span(&mut self.text, &s))
            }
            Value::Bytes(b) if fits(self.bytes.len(), b.len()) => {
                let start = self.bytes.len();
                self.bytes.extend_from_slice(&b);
                Cell::Bytes(Span {
                    start: u32::try_from(start).unwrap_or(u32::MAX),
                    len: u32::try_from(b.len()).unwrap_or(u32::MAX),
                })
            }
            other => {
                let ix = u32::try_from(self.other.len()).unwrap_or(u32::MAX);
                self.other.push(other);
                Cell::Other(ix)
            }
        }
    }

    fn span(arena: &mut String, s: &str) -> Span {
        let start = arena.len();
        arena.push_str(s);
        Span {
            start: u32::try_from(start).unwrap_or(u32::MAX),
            len: u32::try_from(s.len()).unwrap_or(u32::MAX),
        }
    }

    fn resolve(&self, cell: &Cell) -> CellRef<'_> {
        match *cell {
            Cell::Null => CellRef::Null,
            Cell::Bool(b) => CellRef::Bool(b),
            Cell::Int(i) => CellRef::Int(i),
            Cell::Float(x) => CellRef::Float(x),
            Cell::Text(s) => CellRef::Text(self.text_at(s)),
            Cell::Json(s) => CellRef::Json(self.text_at(s)),
            Cell::Bytes(s) => CellRef::Bytes(
                self.bytes
                    .get(s.start as usize..s.start as usize + s.len as usize)
                    .unwrap_or_default(),
            ),
            Cell::Other(ix) => self
                .other
                .get(ix as usize)
                .map_or(CellRef::Null, CellRef::Value),
        }
    }

    fn text_at(&self, s: Span) -> &str {
        self.text
            .get(s.start as usize..s.start as usize + s.len as usize)
            .unwrap_or_default()
    }

    /// Number of rows.
    pub(super) fn rows(&self) -> usize {
        self.rows
    }

    /// The row at `index` within this chunk.
    pub(super) fn row(&self, index: usize) -> Option<RowRef<'_>> {
        (index < self.rows).then(|| RowRef::new(self, index))
    }

    /// A capacity estimate of the heap held by this chunk.
    pub(super) fn heap_bytes(&self) -> usize {
        let mut total = self.cells.capacity() * size_of::<Cell>()
            + self.text.capacity()
            + self.bytes.capacity()
            + self.other.capacity() * size_of::<Value>();
        for v in &self.other {
            total += match v {
                Value::Text(s) | Value::Json(s) => s.capacity(),
                Value::Bytes(b) => b.capacity(),
                Value::Decimal(d) => d.as_str().len(),
                // `Value` is non-exhaustive; the other variants own no heap data.
                _ => 0,
            };
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{Date, Decimal, Time, Timestamp, TimestampTz};
    use sqler_driver_api::{QueryEvent, QueryStream};

    use super::super::synthetic::SyntheticSource;
    use super::*;

    #[test]
    fn cell_is_16_bytes() {
        assert_eq!(size_of::<Cell>(), 16);
    }

    fn all_values() -> Vec<Value> {
        let date: Date = "2024-02-29".parse().unwrap();
        let time: Time = "03:04:05.5".parse().unwrap();
        let ts: Timestamp = "2024-02-29 03:04:05.5".parse().unwrap();
        let tz: TimestampTz = "2024-02-29 03:04:05.5+01:00".parse().unwrap();
        let dec: Decimal = "-12.50".parse().unwrap();
        vec![
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(i64::MIN),
            Value::Float(1.5),
            Value::Float(f64::INFINITY),
            Value::Text(String::new()),
            Value::Text("héllo\nworld".to_owned()),
            Value::Json("{\"a\": [1, 2]}".to_owned()),
            Value::Bytes(Vec::new()),
            Value::Bytes(vec![0xde, 0xad, 0x00]),
            Value::Decimal(dec),
            Value::Date(date),
            Value::Time(time),
            Value::Timestamp(ts),
            Value::TimestampTz(tz),
        ]
    }

    #[test]
    fn round_trips_every_variant() {
        let values = all_values();
        let width = values.len();
        let chunk = Chunk::from_rows(vec![Row(values.clone()), Row(values.clone())], width);
        assert_eq!(chunk.rows(), 2);
        for r in 0..2 {
            let row = chunk.row(r).unwrap();
            assert_eq!(row.len(), width);
            assert!(!row.is_empty());
            for (c, v) in values.iter().enumerate() {
                let cell = row.get(c).unwrap();
                assert_eq!(&cell.to_value(), v, "{v:?}");
                assert_eq!(cell.to_string(), v.to_string(), "{v:?}");
                assert_eq!(cell.is_null(), matches!(v, Value::Null));
            }
            assert!(row.get(width).is_none());
            assert_eq!(row.iter().count(), width);
            assert_eq!(row.to_row(), Row(values.clone()));
        }
        assert!(chunk.row(2).is_none());
    }

    #[test]
    fn variants_land_in_the_right_arenas() {
        let chunk = Chunk::from_rows(
            vec![Row(vec![
                Value::Text("ab".into()),
                Value::Json("{}".into()),
                Value::Bytes(vec![1, 2, 3]),
                Value::Decimal("1.5".parse().unwrap()),
            ])],
            4,
        );
        let row = chunk.row(0).unwrap();
        assert!(matches!(row.get(0), Some(CellRef::Text("ab"))));
        assert!(matches!(row.get(1), Some(CellRef::Json("{}"))));
        assert!(matches!(row.get(2), Some(CellRef::Bytes([1, 2, 3]))));
        assert!(matches!(row.get(3), Some(CellRef::Value(_))));
        assert_eq!(chunk.text.len(), 4);
        assert_eq!(chunk.text.capacity(), 4);
        assert_eq!(chunk.bytes.capacity(), 3);
        assert_eq!(chunk.other.capacity(), 1);
    }

    #[test]
    fn arena_overflow_falls_back_to_the_side_table() {
        assert!(fits(0, 10));
        assert!(fits(u32::MAX as usize - 10, 10));
        assert!(!fits(u32::MAX as usize - 9, 10));
        assert!(!fits(usize::MAX, 1));
        assert!(!fits(0, usize::MAX));
    }

    #[test]
    fn short_and_empty_rows_are_padded() {
        let chunk = Chunk::from_rows(vec![Row(vec![Value::Int(1)])], 2);
        let row = chunk.row(0).unwrap();
        assert!(matches!(row.get(0), Some(CellRef::Int(1))));
        assert!(row.get(1).unwrap().is_null());
        let chunk = Chunk::from_rows(vec![Row(vec![]), Row(vec![])], 0);
        assert_eq!(chunk.rows(), 2);
        let row = chunk.row(1).unwrap();
        assert!(row.is_empty());
        assert_eq!(row.iter().count(), 0);
        assert!(row.get(0).is_none());
    }

    #[test]
    fn heap_bytes_counts_arenas_and_side_table() {
        let chunk = Chunk::from_rows(
            vec![Row(vec![
                Value::Text("x".repeat(100)),
                Value::Bytes(vec![0; 50]),
                Value::Decimal("1".repeat(300).parse().unwrap()),
            ])],
            3,
        );
        assert!(chunk.heap_bytes() >= 3 * 16 + 100 + 50 + 300);
    }

    /// The old per-row estimate: `Vec<Row>` of `Vec<Value>` with owned strings.
    fn old_estimate(rows: &[Row]) -> usize {
        let mut total = std::mem::size_of_val(rows);
        for row in rows {
            total += row.0.len() * size_of::<Value>();
            for v in &row.0 {
                if let Value::Text(s) = v {
                    total += s.capacity();
                }
            }
        }
        total
    }

    #[test]
    fn synthetic_rows_cost_less_than_half_the_old_layout() {
        let mut src = SyntheticSource::new(1024, 6, 1024);
        let mut batch = Vec::new();
        futures_lite_block(&mut src, &mut batch);
        assert_eq!(batch.len(), 1024);
        let old = old_estimate(&batch);
        let chunk = Chunk::from_rows(batch, 6);
        assert!(
            chunk.heap_bytes() * 2 <= old,
            "{} vs {old}",
            chunk.heap_bytes()
        );
    }

    /// Pulls the rows out of a `SyntheticSource` (its futures are always ready).
    fn futures_lite_block(src: &mut SyntheticSource, out: &mut Vec<Row>) {
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            let mut fut = src.next_event();
            let Poll::Ready(ev) = fut.as_mut().poll(&mut cx) else {
                unreachable!("synthetic futures are ready");
            };
            match ev {
                Some(Ok(QueryEvent::Rows(r))) => out.extend(r),
                Some(Ok(QueryEvent::Done { .. })) | None => return,
                _ => {}
            }
        }
    }
}
