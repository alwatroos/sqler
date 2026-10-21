// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Execution of a planned call: one deadline, a row cap per statement, cancel on timeout.

use std::future::Future;
use std::time::Duration;

use sqler_driver_api::sqler_core::{Column, Query, Row};
use sqler_driver_api::{QueryEvent, QueryStream, Session};
use tokio::time::{Instant, timeout_at};

use crate::guard::Planned;
use crate::toolset::Truncation;

/// What one statement produced.
#[derive(Debug)]
pub(crate) struct StatementOutput {
    pub index: usize,
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub rows_affected: Option<u64>,
    pub truncated: Option<Truncation>,
    pub error: Option<String>,
}

/// What a whole call produced.
#[derive(Debug, Default)]
pub(crate) struct Execution {
    /// The statements that were sent to the driver, in order.
    pub statements: Vec<StatementOutput>,
    /// The statements that never ran (a timeout or an error stopped the call).
    pub not_run: usize,
}

/// How long the cleanup after the deadline (cancel, release) may take.
pub(crate) const GRACE: Duration = Duration::from_secs(2);

struct TimedOut;

/// Awaits `fut` until `deadline`, if there is one.
async fn bounded<F: Future>(deadline: Option<Instant>, fut: F) -> Result<F::Output, TimedOut> {
    match deadline {
        Some(at) => timeout_at(at, fut).await.map_err(|_| TimedOut),
        None => Ok(fut.await),
    }
}

/// How the stream of one statement ended.
enum End {
    Done,
    TimedOut,
}

/// Reads one statement's stream into `out`. The stream is dropped when this returns.
async fn read_stream(
    mut stream: Box<dyn QueryStream + '_>,
    deadline: Option<Instant>,
    row_cap: Option<usize>,
    out: &mut StatementOutput,
) -> End {
    loop {
        match bounded(deadline, stream.next_event()).await {
            Err(TimedOut) => return End::TimedOut,
            Ok(None) => {
                out.error = Some("the driver ended the result before it was complete".to_owned());
                return End::Done;
            }
            Ok(Some(Err(e))) => {
                out.error = Some(e.to_string());
                return End::Done;
            }
            Ok(Some(Ok(QueryEvent::Columns(columns)))) => out.columns = columns,
            Ok(Some(Ok(QueryEvent::Rows(batch)))) => {
                for row in batch {
                    if row_cap.is_some_and(|cap| out.rows.len() >= cap) {
                        out.truncated = Some(Truncation::RowLimit);
                        return End::Done;
                    }
                    out.rows.push(row);
                }
            }
            Ok(Some(Ok(QueryEvent::Done { rows_affected }))) => {
                out.rows_affected = rows_affected;
                return End::Done;
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Runs the planned statements on `session`, in order.
///
/// The deadline starts now and covers the whole call. On a timeout the driver is cancelled
/// through its [`CancelHandle`](sqler_driver_api::CancelHandle) (an error from the cancel is
/// ignored, and the cancel itself gets at most [`GRACE`]), the statement is marked [`Truncation::TimeLimit`] and the rest are counted in
/// `not_run`. A driver error is kept on its statement and also stops the call. A statement that
/// would return more than `row_cap` rows is cut and marked [`Truncation::RowLimit`]; the call goes on.
pub(crate) async fn execute(
    session: &mut dyn Session,
    plan: &[Planned],
    timeout: Option<Duration>,
    row_cap: Option<u32>,
) -> Execution {
    let deadline = timeout.map(|t| Instant::now() + t);
    let row_cap = row_cap.map(|n| usize::try_from(n).unwrap_or(usize::MAX));
    let mut execution = Execution::default();
    for (index, statement) in plan.iter().enumerate() {
        let mut out = StatementOutput {
            index,
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: None,
            truncated: None,
            error: None,
        };
        let cancel = session.cancel_handle();
        let query = Query::new(statement.sql.as_str());
        let mut stop = false;
        match bounded(deadline, session.execute(&query)).await {
            Err(TimedOut) => {
                let _ = tokio::time::timeout(GRACE, cancel.cancel()).await;
                out.truncated = Some(Truncation::TimeLimit);
                stop = true;
            }
            Ok(Err(e)) => {
                out.error = Some(e.to_string());
                stop = true;
            }
            Ok(Ok(stream)) => {
                if let End::TimedOut = read_stream(stream, deadline, row_cap, &mut out).await {
                    let _ = tokio::time::timeout(GRACE, cancel.cancel()).await;
                    out.truncated = Some(Truncation::TimeLimit);
                    stop = true;
                }
                if out.error.is_some() {
                    stop = true;
                }
            }
        }
        execution.statements.push(out);
        if stop {
            execution.not_run = plan.len() - index - 1;
            break;
        }
    }
    execution
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_driver_api::sqler_core::{DataType, Value};
    use sqler_driver_api::{BoxFuture, CancelHandle, Canceller, DriverError};
    use sqler_sql::StatementClass;

    use super::*;

    /// Counts cancels.
    struct Counter(Arc<AtomicUsize>);

    impl Canceller for Counter {
        fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(Ok(())))
        }
    }

    /// Yields its columns, one row, then hangs forever.
    struct Hanging {
        sent: usize,
    }

    impl QueryStream for Hanging {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            self.sent += 1;
            let step = self.sent;
            Box::pin(async move {
                match step {
                    1 => Some(Ok(QueryEvent::Columns(vec![Column::new(
                        "a",
                        DataType::Integer,
                    )]))),
                    2 => Some(Ok(QueryEvent::Rows(vec![Row(vec![Value::Int(1)])]))),
                    _ => std::future::pending().await,
                }
            })
        }
    }

    struct HangingSession {
        cancels: Arc<AtomicUsize>,
        executes: Arc<AtomicUsize>,
    }

    impl Session for HangingSession {
        fn cancel_handle(&self) -> CancelHandle {
            CancelHandle::new(Arc::new(Counter(Arc::clone(&self.cancels))))
        }

        fn execute<'s>(
            &'s mut self,
            _query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            self.executes.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(Ok(
                Box::new(Hanging { sent: 0 }) as Box<dyn QueryStream + 's>
            )))
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    fn planned(sql: &str) -> Planned {
        Planned {
            sql: sql.to_owned(),
            class: StatementClass::Read,
            reason: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_stream_that_never_yields_is_cancelled_at_the_deadline() {
        let cancels = Arc::new(AtomicUsize::new(0));
        let executes = Arc::new(AtomicUsize::new(0));
        let mut session = HangingSession {
            cancels: Arc::clone(&cancels),
            executes: Arc::clone(&executes),
        };
        let plan = [planned("select 1"), planned("select 2")];
        let run = execute(&mut session, &plan, Some(Duration::from_millis(50)), None).await;
        assert_eq!(
            cancels.load(Ordering::SeqCst),
            1,
            "the driver was cancelled"
        );
        assert_eq!(
            executes.load(Ordering::SeqCst),
            1,
            "the second statement never started"
        );
        assert_eq!(run.statements.len(), 1);
        assert_eq!(run.statements[0].truncated, Some(Truncation::TimeLimit));
        assert_eq!(run.statements[0].rows.len(), 1, "rows read so far are kept");
        assert_eq!(run.not_run, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn row_cap_stops_reading_without_cancelling() {
        let cancels = Arc::new(AtomicUsize::new(0));
        let executes = Arc::new(AtomicUsize::new(0));
        let mut session = HangingSession {
            cancels: Arc::clone(&cancels),
            executes: Arc::clone(&executes),
        };
        // One row arrives, then the stream hangs: a cap of zero rows cuts at the first row.
        let run = execute(&mut session, &[planned("select 1")], None, Some(0)).await;
        assert_eq!(run.statements[0].truncated, Some(Truncation::RowLimit));
        assert_eq!(run.statements[0].rows.len(), 0);
        assert_eq!(cancels.load(Ordering::SeqCst), 0);
        assert_eq!(run.not_run, 0);
    }
}
