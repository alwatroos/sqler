// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The writer API: [`RowWriter`], [`Format`] and the [`Exporter`] that drives a writer.

use std::io::{self, BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use sqler_core::{Column, Row};

use crate::{ExportError, ExportOptions};

/// Where the bytes go: any `io::Write` that can move between threads.
pub type Sink<'a> = Box<dyn Write + Send + 'a>;

/// The rows go to the writer in chunks of this size; cancel is checked before every chunk.
const CHUNK_ROWS: usize = 1024;

/// The sink is buffered with this many bytes.
const BUFFER_BYTES: usize = 64 * 1024;

/// A push-style renderer of one format. It keeps no rows.
pub trait RowWriter: Send {
    /// Starts the output. Called once, before any row.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if the columns cannot be written or the sink fails.
    fn begin(&mut self, columns: &[Column]) -> Result<(), ExportError>;

    /// Writes rows. Every row has exactly as many values as there were columns.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if the sink fails.
    fn write_batch(&mut self, rows: &[Row]) -> Result<(), ExportError>;

    /// Writes the trailer and flushes the sink.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if the sink fails.
    fn finish(self: Box<Self>) -> Result<(), ExportError>;
}

/// A file format: it makes a [`RowWriter`] on a sink.
pub trait Format: Send + Sync {
    /// A stable id such as `csv`.
    fn id(&self) -> &'static str;
    /// A name for a menu.
    fn label(&self) -> &'static str;
    /// The file extension without the dot.
    fn extension(&self) -> &'static str;
    /// Makes a writer on `sink`.
    ///
    /// # Errors
    ///
    /// [`ExportError::InvalidOptions`] if the options do not suit the format.
    fn create<'a>(
        &self,
        options: &ExportOptions,
        sink: Sink<'a>,
    ) -> Result<Box<dyn RowWriter + 'a>, ExportError>;
}

/// A shared flag that stops an export. Clones share the flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every clone of this token.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether [`cancel`](Self::cancel) was called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// How far an export has come.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Rows handed to the writer so far.
    pub rows: u64,
    /// Bytes that reached the sink so far (the buffer of up to 64 KiB may hold a few more).
    pub bytes: u64,
}

/// The result of a finished export.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Rows written.
    pub rows: u64,
    /// Bytes written to the sink.
    pub bytes: u64,
}

/// Cancel and progress hooks of one export.
#[derive(Default)]
pub struct ExportControl {
    /// Set it to stop the export with [`ExportError::Cancelled`].
    pub cancel: CancelToken,
    /// Called after every batch and once in `finish`.
    pub progress: Option<Box<dyn FnMut(Progress) + Send>>,
}

/// Counts the bytes that pass to the inner sink.
struct CountingWrite<W> {
    inner: W,
    count: Arc<AtomicU64>,
}

impl<W: Write> Write for CountingWrite<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Drives a [`RowWriter`]: call order, row width, buffering, byte counting, cancel and progress.
pub struct Exporter<'a> {
    writer: Box<dyn RowWriter + 'a>,
    bytes: Arc<AtomicU64>,
    control: ExportControl,
    width: Option<usize>,
    rows: u64,
}

impl<'a> Exporter<'a> {
    /// Wraps `sink` in a 64 KiB buffer and makes the writer of `format`.
    ///
    /// # Errors
    ///
    /// [`ExportError::InvalidOptions`] if the options do not suit the format.
    pub fn new(
        format: &dyn Format,
        options: &ExportOptions,
        sink: Sink<'a>,
        control: ExportControl,
    ) -> Result<Self, ExportError> {
        let bytes = Arc::new(AtomicU64::new(0));
        let counting = CountingWrite {
            inner: sink,
            count: Arc::clone(&bytes),
        };
        let buffered: Sink<'a> = Box::new(BufWriter::with_capacity(BUFFER_BYTES, counting));
        let writer = format.create(options, buffered)?;
        Ok(Self {
            writer,
            bytes,
            control,
            width: None,
            rows: 0,
        })
    }

    /// Starts the output. Must be called once, first.
    ///
    /// # Errors
    ///
    /// [`ExportError::State`] if called twice, or the writer's error.
    pub fn begin(&mut self, columns: &[Column]) -> Result<(), ExportError> {
        if self.width.is_some() {
            return Err(ExportError::State("begin was already called"));
        }
        self.writer.begin(columns)?;
        self.width = Some(columns.len());
        Ok(())
    }

    /// Writes a batch of rows.
    ///
    /// # Errors
    ///
    /// [`ExportError::State`] before `begin`, [`ExportError::RowWidth`] if any row is too
    /// short or too long (nothing of the batch is written then), [`ExportError::Cancelled`],
    /// or the writer's error.
    pub fn write_batch(&mut self, rows: &[Row]) -> Result<(), ExportError> {
        let expected = self
            .width
            .ok_or(ExportError::State("write_batch was called before begin"))?;
        if let Some(bad) = rows.iter().find(|r| r.len() != expected) {
            return Err(ExportError::RowWidth {
                expected,
                got: bad.len(),
            });
        }
        for chunk in rows.chunks(CHUNK_ROWS) {
            self.check_cancel()?;
            self.writer.write_batch(chunk)?;
            self.rows += chunk.len() as u64;
        }
        self.report();
        Ok(())
    }

    /// Writes the trailer, flushes the sink and returns the totals.
    ///
    /// # Errors
    ///
    /// [`ExportError::State`] before `begin`, [`ExportError::Cancelled`] (no trailer is
    /// written), or the writer's error.
    pub fn finish(mut self) -> Result<Summary, ExportError> {
        if self.width.is_none() {
            return Err(ExportError::State("finish was called before begin"));
        }
        self.check_cancel()?;
        self.writer.finish()?;
        let summary = Summary {
            rows: self.rows,
            bytes: self.bytes.load(Ordering::Relaxed),
        };
        if let Some(progress) = self.control.progress.as_mut() {
            progress(Progress {
                rows: summary.rows,
                bytes: summary.bytes,
            });
        }
        Ok(summary)
    }

    fn check_cancel(&self) -> Result<(), ExportError> {
        if self.control.cancel.is_cancelled() {
            Err(ExportError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn report(&mut self) {
        let progress = Progress {
            rows: self.rows,
            bytes: self.bytes.load(Ordering::Relaxed),
        };
        if let Some(f) = self.control.progress.as_mut() {
            f(progress);
        }
    }
}

/// Renders a whole result to memory: for copy-as and for tests.
///
/// # Errors
///
/// Any [`ExportError`] of the format or the rows.
pub fn export_to_vec(
    format: &dyn Format,
    options: &ExportOptions,
    columns: &[Column],
    rows: &[Row],
) -> Result<Vec<u8>, ExportError> {
    let mut out = Vec::new();
    let mut exporter = Exporter::new(
        format,
        options,
        Box::new(&mut out),
        ExportControl::default(),
    )?;
    exporter.begin(columns)?;
    exporter.write_batch(rows)?;
    exporter.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use sqler_core::{DataType, Value};

    use super::*;
    use crate::{CsvFormat, JsonFormat};

    /// A sink that shares its bytes.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn cols(n: usize) -> Vec<Column> {
        (0..n)
            .map(|i| Column::new(format!("c{i}"), DataType::Integer))
            .collect()
    }

    fn rows(n: usize, width: usize) -> Vec<Row> {
        (0..n)
            .map(|i| Row(vec![Value::Int(i64::try_from(i).unwrap()); width]))
            .collect()
    }

    fn exporter(control: ExportControl, sink: &Shared) -> Exporter<'static> {
        Exporter::new(
            &CsvFormat,
            &ExportOptions::default(),
            Box::new(sink.clone()),
            control,
        )
        .unwrap()
    }

    #[test]
    fn state_errors() {
        let sink = Shared::default();
        let mut e = exporter(ExportControl::default(), &sink);
        assert!(matches!(
            e.write_batch(&rows(1, 1)),
            Err(ExportError::State(_))
        ));
        e.begin(&cols(1)).unwrap();
        assert!(matches!(e.begin(&cols(1)), Err(ExportError::State(_))));
        let e2 = exporter(ExportControl::default(), &sink);
        assert!(matches!(e2.finish(), Err(ExportError::State(_))));
    }

    #[test]
    fn row_width_is_checked_for_every_row() {
        let sink = Shared::default();
        let mut e = exporter(ExportControl::default(), &sink);
        e.begin(&cols(2)).unwrap();
        let mut batch = rows(3000, 2);
        batch[2999] = Row(vec![Value::Null]);
        let err = e.write_batch(&batch).unwrap_err();
        assert!(matches!(
            err,
            ExportError::RowWidth {
                expected: 2,
                got: 1
            }
        ));
        // Nothing of the bad batch was written.
        assert_eq!(e.finish().unwrap().rows, 0);
    }

    #[test]
    fn cancel_before_a_batch_and_before_finish() {
        let sink = Shared::default();
        let control = ExportControl::default();
        let token = control.cancel.clone();
        let mut e = exporter(control, &sink);
        e.begin(&cols(1)).unwrap();
        e.write_batch(&rows(2, 1)).unwrap();
        token.cancel();
        assert!(matches!(
            e.write_batch(&rows(2, 1)),
            Err(ExportError::Cancelled)
        ));
        assert!(matches!(e.finish(), Err(ExportError::Cancelled)));

        // JSON writes a trailer in finish: a cancelled export has none.
        let control = ExportControl::default();
        let token = control.cancel.clone();
        let out = Shared::default();
        let mut e = Exporter::new(
            &JsonFormat,
            &ExportOptions::default(),
            Box::new(out.clone()),
            control,
        )
        .unwrap();
        e.begin(&cols(1)).unwrap();
        e.write_batch(&rows(1, 1)).unwrap();
        token.cancel();
        assert!(matches!(e.finish(), Err(ExportError::Cancelled)));
        assert!(
            !String::from_utf8(out.0.lock().unwrap().clone())
                .unwrap()
                .contains(']')
        );
    }

    #[test]
    fn cancel_is_checked_between_chunks() {
        let sink = Shared::default();
        let token = CancelToken::new();
        let control = ExportControl {
            cancel: token.clone(),
            progress: None,
        };
        let mut e = exporter(control, &sink);
        e.begin(&cols(1)).unwrap();
        token.cancel();
        assert!(matches!(
            e.write_batch(&rows(5000, 1)),
            Err(ExportError::Cancelled)
        ));
    }

    #[test]
    fn progress_calls_and_summary_bytes() {
        let sink = Shared::default();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&calls);
        let control = ExportControl {
            cancel: CancelToken::new(),
            progress: Some(Box::new(move |p| log.lock().unwrap().push(p))),
        };
        let mut e = exporter(control, &sink);
        e.begin(&cols(1)).unwrap();
        e.write_batch(&rows(1500, 1)).unwrap();
        e.write_batch(&rows(10, 1)).unwrap();
        let summary = e.finish().unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].rows, 1500);
        assert_eq!(calls[1].rows, 1510);
        assert_eq!(calls[2].rows, 1510);
        assert_eq!(calls[2].bytes, summary.bytes);
        assert_eq!(summary.rows, 1510);
        assert_eq!(summary.bytes, sink.0.lock().unwrap().len() as u64);
        assert!(summary.bytes > 0);
    }

    #[test]
    fn export_to_vec_renders_everything() {
        let out =
            export_to_vec(&CsvFormat, &ExportOptions::default(), &cols(1), &rows(2, 1)).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "c0\r\n0\r\n1\r\n");
    }
}
