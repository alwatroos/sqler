// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use sqler_driver_api::sqler_core::CoreError;
use sqler_driver_api::{DriverError, QueryEvent, QueryStream};

use super::model::{GridSink, Msg};

/// What a finished ingestion read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct IngestSummary {
    /// Rows sent to the model.
    pub rows: usize,
    /// Rows affected by the statement, if the backend reports it.
    pub rows_affected: Option<u64>,
}

/// Reads `stream` to its end into `sink`, enforcing the stream protocol like
/// `sqler_driver_api::collect`. Sends never block, and the function does not depend on a runtime.
///
/// The cancel flag is checked before each event. On any error the model is told (`Cancelled` for
/// cancellation, `Failed` otherwise) and the error is returned. Returning drops the borrow of the
/// stream; dropping the stream stops fetching.
///
/// # Errors
/// [`DriverError::Cancelled`] when cancelled or when the model was dropped;
/// [`DriverError::Protocol`] for `Rows`/`Done` before `Columns`, a second `Columns`, or a stream
/// that ends before `Done`; [`DriverError::Core`] for a row of the wrong width; any error from the
/// stream unchanged.
pub async fn ingest(
    stream: &mut (dyn QueryStream + '_),
    sink: GridSink,
) -> Result<IngestSummary, DriverError> {
    match run(stream, &sink).await {
        Ok(summary) => Ok(summary),
        Err(DriverError::Cancelled) => {
            // A failed send means the model is gone, which is fine here.
            sink.send(Msg::Cancelled);
            Err(DriverError::Cancelled)
        }
        Err(err) => {
            sink.send(Msg::Failed(err.to_string()));
            Err(err)
        }
    }
}

async fn run(
    stream: &mut (dyn QueryStream + '_),
    sink: &GridSink,
) -> Result<IngestSummary, DriverError> {
    let mut width: Option<usize> = None;
    let mut rows = 0;
    loop {
        if sink.is_cancelled() {
            return Err(DriverError::Cancelled);
        }
        match stream.next_event().await {
            None => return Err(DriverError::Protocol("stream ended before Done".to_owned())),
            Some(Err(e)) => return Err(e),
            Some(Ok(QueryEvent::Columns(columns))) => {
                if width.is_some() {
                    return Err(DriverError::Protocol("second Columns event".to_owned()));
                }
                width = Some(columns.len());
                deliver(sink, Msg::Columns(columns))?;
            }
            Some(Ok(QueryEvent::Rows(batch))) => {
                let Some(expected) = width else {
                    return Err(DriverError::Protocol("Rows before Columns".to_owned()));
                };
                if let Some(bad) = batch.iter().find(|r| r.len() != expected) {
                    return Err(CoreError::RowWidth {
                        expected,
                        actual: bad.len(),
                    }
                    .into());
                }
                rows += batch.len();
                deliver(sink, Msg::Rows(batch))?;
            }
            Some(Ok(QueryEvent::Done { rows_affected })) => {
                if width.is_none() {
                    return Err(DriverError::Protocol("Done before Columns".to_owned()));
                }
                deliver(sink, Msg::Done(rows_affected))?;
                return Ok(IngestSummary {
                    rows,
                    rows_affected,
                });
            }
            Some(Ok(_)) => {
                return Err(DriverError::Protocol("unknown event".to_owned()));
            }
        }
    }
}

/// Sends a message; a dropped model means nobody wants the rest, which counts as a cancel.
fn deliver(sink: &GridSink, msg: Msg) -> Result<(), DriverError> {
    if sink.send(msg) {
        Ok(())
    } else {
        Err(DriverError::Cancelled)
    }
}

/// Runs [`ingest()`] on a new thread named `sqler-grid-ingest`; the stream moves to that thread.
///
/// The thread has no async runtime. Streams that need a Tokio context (the `sqlx` drivers) must
/// instead run [`ingest()`] on the driver's own runtime.
///
/// # Errors
/// Returns the OS error if the thread cannot be spawned.
pub fn spawn_ingest<S: QueryStream + 'static>(
    mut stream: S,
    sink: GridSink,
) -> std::io::Result<thread::JoinHandle<Result<IngestSummary, DriverError>>> {
    thread::Builder::new()
        .name("sqler-grid-ingest".to_owned())
        .spawn(move || block_on(ingest(&mut stream, sink)))
}

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls a future on the current thread, parking while it is pending.
fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};
    use sqler_driver_api::{BoxFuture, CancelFlag};

    use super::super::model::{IngestState, grid_channel};
    use super::super::synthetic::SyntheticSource;
    use super::*;

    struct VecStream {
        events: std::vec::IntoIter<Result<QueryEvent, DriverError>>,
        /// Cancels this flag after yielding the first `Rows` event.
        cancel_after_rows: Option<CancelFlag>,
        yielded_rows: bool,
    }

    impl VecStream {
        fn new(events: Vec<Result<QueryEvent, DriverError>>) -> Self {
            Self {
                events: events.into_iter(),
                cancel_after_rows: None,
                yielded_rows: false,
            }
        }
    }

    impl QueryStream for VecStream {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            let next = self.events.next();
            if matches!(next, Some(Ok(QueryEvent::Rows(_)))) && !self.yielded_rows {
                self.yielded_rows = true;
                if let Some(flag) = &self.cancel_after_rows {
                    flag.cancel();
                }
            }
            Box::pin(std::future::ready(next))
        }
    }

    fn cols() -> QueryEvent {
        QueryEvent::Columns(vec![Column::new("a", DataType::Integer)])
    }

    fn rows(from: i64, n: i64) -> QueryEvent {
        QueryEvent::Rows((from..from + n).map(|i| Row(vec![Value::Int(i)])).collect())
    }

    fn done() -> QueryEvent {
        QueryEvent::Done {
            rows_affected: None,
        }
    }

    /// Ingests the events and returns the result plus the polled model.
    fn run(
        events: Vec<Result<QueryEvent, DriverError>>,
    ) -> (Result<IngestSummary, DriverError>, super::super::GridModel) {
        let (mut model, sink) = grid_channel();
        let res = block_on(ingest(&mut VecStream::new(events), sink));
        model.poll(usize::MAX);
        (res, model)
    }

    #[test]
    fn happy_path() {
        let (res, model) = run(vec![Ok(cols()), Ok(rows(0, 3)), Ok(rows(3, 2)), Ok(done())]);
        let summary = res.unwrap();
        assert_eq!(summary.rows, 5);
        assert_eq!(summary.rows_affected, None);
        assert_eq!(model.row_count(), 5);
        assert!(matches!(model.state(), IngestState::Done { .. }));
    }

    #[test]
    fn rows_affected_passes_through() {
        let (res, model) = run(vec![
            Ok(QueryEvent::Columns(vec![])),
            Ok(QueryEvent::Done {
                rows_affected: Some(9),
            }),
        ]);
        assert_eq!(res.unwrap().rows_affected, Some(9));
        assert_eq!(
            model.state(),
            &IngestState::Done {
                rows_affected: Some(9)
            }
        );
    }

    #[test]
    fn cancelled_before_start() {
        let (mut model, sink) = grid_channel();
        sink.cancel_flag().cancel();
        let res = block_on(ingest(
            &mut VecStream::new(vec![Ok(cols()), Ok(done())]),
            sink,
        ));
        assert!(matches!(res, Err(DriverError::Cancelled)));
        model.poll(usize::MAX);
        assert_eq!(model.state(), &IngestState::Cancelled);
        assert_eq!(model.row_count(), 0);
    }

    #[test]
    fn cancelled_mid_stream_keeps_first_batch() {
        let (mut model, sink) = grid_channel();
        let mut stream =
            VecStream::new(vec![Ok(cols()), Ok(rows(0, 4)), Ok(rows(4, 4)), Ok(done())]);
        stream.cancel_after_rows = Some(sink.cancel_flag());
        let res = block_on(ingest(&mut stream, sink));
        assert!(matches!(res, Err(DriverError::Cancelled)));
        model.poll(usize::MAX);
        assert_eq!(model.state(), &IngestState::Cancelled);
        assert_eq!(model.row_count(), 4);
    }

    #[test]
    fn model_dropped_stops_ingestion() {
        let (model, sink) = grid_channel();
        drop(model);
        let res = block_on(ingest(
            &mut VecStream::new(vec![Ok(cols()), Ok(done())]),
            sink,
        ));
        assert!(matches!(res, Err(DriverError::Cancelled)));
    }

    #[test]
    fn protocol_errors() {
        for events in [
            vec![Ok(rows(0, 1))],
            vec![Ok(done())],
            vec![Ok(cols()), Ok(cols())],
            vec![Ok(cols())],
            vec![],
        ] {
            let (res, model) = run(events);
            assert!(matches!(res, Err(DriverError::Protocol(_))));
            assert!(matches!(model.state(), IngestState::Failed(_)));
        }
    }

    #[test]
    fn width_mismatch_fails() {
        let (res, model) = run(vec![
            Ok(cols()),
            Ok(QueryEvent::Rows(vec![Row(vec![])])),
            Ok(done()),
        ]);
        assert!(matches!(
            res,
            Err(DriverError::Core(CoreError::RowWidth {
                expected: 1,
                actual: 0
            }))
        ));
        assert!(matches!(model.state(), IngestState::Failed(_)));
        assert_eq!(model.row_count(), 0);
    }

    #[test]
    fn stream_cancelled_error_is_cancelled_state() {
        let (res, model) = run(vec![Ok(cols()), Err(DriverError::Cancelled)]);
        assert!(matches!(res, Err(DriverError::Cancelled)));
        assert_eq!(model.state(), &IngestState::Cancelled);
    }

    #[test]
    fn stream_error_is_failed_state() {
        let (res, model) = run(vec![Ok(cols()), Err(DriverError::Query("bad".into()))]);
        assert!(matches!(res, Err(DriverError::Query(_))));
        assert_eq!(
            model.state(),
            &IngestState::Failed("query failed: bad".to_owned())
        );
    }

    #[test]
    fn spawn_ingest_synthetic() {
        let (mut model, sink) = grid_channel();
        let handle = spawn_ingest(SyntheticSource::new(10_000, 6, 256), sink).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !matches!(model.state(), IngestState::Done { .. }) {
            assert!(Instant::now() < deadline, "ingestion timed out");
            if model.poll(usize::MAX) == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(model.row_count(), 10_000);
        assert_eq!(model.columns().len(), 6);
        let summary = handle.join().unwrap().unwrap();
        assert_eq!(summary.rows, 10_000);
    }

    #[test]
    fn block_on_wakes_from_another_thread() {
        struct Flag(Arc<std::sync::atomic::AtomicBool>, Option<Waker>);
        impl Future for Flag {
            type Output = ();
            fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                    return Poll::Ready(());
                }
                self.1 = Some(cx.waker().clone());
                let flag = self.0.clone();
                let waker = cx.waker().clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(20));
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    waker.wake();
                });
                Poll::Pending
            }
        }
        block_on(Flag(Arc::default(), None));
    }
}
