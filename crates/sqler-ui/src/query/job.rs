// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The async job that runs a plan's statements on a tab's session.

use std::time::Instant;

use sqler_driver_api::{BoxFuture, CancelFlag, DriverError, QueryEvent, QueryStream, Session};
use tokio::sync::mpsc::UnboundedSender;

use super::params::BoundStatement;
use super::run::{RunEvent, RunId};
use crate::grid::{GridSink, ingest};
use crate::tabs::TabId;

/// Runs the statements of one run, in order, on the session lent to it.
///
/// `sinks` has one sink per statement, in the same order.
pub struct QueryJob {
    /// The tab the run belongs to.
    pub tab: TabId,
    /// The run.
    pub run: RunId,
    /// The tab's session, on loan until the job returns it.
    pub session: Box<dyn Session>,
    /// The statements, in execution order.
    pub statements: Vec<BoundStatement>,
    /// The grid sink of each statement.
    pub sinks: Vec<GridSink>,
    /// Set to stop the run before the next statement starts.
    pub stop: CancelFlag,
    /// Where progress is reported; a closed receiver is ignored.
    pub events: UnboundedSender<RunEvent>,
}

impl std::fmt::Debug for QueryJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryJob")
            .field("tab", &self.tab)
            .field("run", &self.run)
            .field("statements", &self.statements.len())
            .finish_non_exhaustive()
    }
}

/// What a finished job hands back.
pub struct QueryJobDone {
    /// The tab the run belonged to.
    pub tab: TabId,
    /// The run.
    pub run: RunId,
    /// The session, returned to its tab.
    pub session: Box<dyn Session>,
}

impl std::fmt::Debug for QueryJobDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryJobDone")
            .field("tab", &self.tab)
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

/// A stream that fails at once; it tells a grid sink why no rows will come.
struct FailedStream(Option<DriverError>);

impl QueryStream for FailedStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(std::future::ready(self.0.take().map(Err)))
    }
}

/// A copy of `err` with the same message (`DriverError` is not `Clone`).
fn copy_of(err: &DriverError) -> DriverError {
    match err {
        DriverError::Cancelled => DriverError::Cancelled,
        other => DriverError::Query(other.to_string()),
    }
}

impl QueryJob {
    /// Runs the job.
    ///
    /// For each statement: if `stop` (or its sink) is cancelled, reports `Cancelled` and stops;
    /// else reports `Started`, executes it and streams the result into its sink, timed. `Finished`
    /// follows success; a cancellation reports `Cancelled` and stops; any other error reports
    /// `Failed` (the sink gets it too) and stops. The sinks of statements that did not run are
    /// dropped. The session is always returned; the job never panics.
    pub fn run(self) -> BoxFuture<'static, QueryJobDone> {
        Box::pin(async move {
            let Self {
                tab,
                run,
                mut session,
                statements,
                sinks,
                stop,
                events,
            } = self;
            for (stmt, sink) in statements.iter().zip(sinks) {
                let index = stmt.index;
                if stop.is_cancelled() || sink.is_cancelled() {
                    let _ = events.send(RunEvent::Cancelled { index });
                    break;
                }
                let _ = events.send(RunEvent::Started { index });
                let started = Instant::now();
                let outcome = execute_one(&mut *session, stmt, sink).await;
                let duration = started.elapsed();
                match outcome {
                    Ok((rows, rows_affected)) => {
                        let _ = events.send(RunEvent::Finished {
                            index,
                            rows,
                            rows_affected,
                            duration,
                        });
                    }
                    Err(DriverError::Cancelled) => {
                        let _ = events.send(RunEvent::Cancelled { index });
                        break;
                    }
                    Err(err) => {
                        let _ = events.send(RunEvent::Failed {
                            index,
                            message: err.to_string(),
                            duration,
                        });
                        break;
                    }
                }
            }
            QueryJobDone { tab, run, session }
        })
    }
}

async fn execute_one(
    session: &mut (dyn Session + '_),
    stmt: &BoundStatement,
    sink: GridSink,
) -> Result<(usize, Option<u64>), DriverError> {
    match session.execute(&stmt.query).await {
        Ok(mut stream) => ingest(&mut *stream, sink)
            .await
            .map(|summary| (summary.rows, summary.rows_affected)),
        Err(err) => {
            // The sink learns why no rows come; the same error goes to the caller.
            let mut failed = FailedStream(Some(copy_of(&err)));
            let _ = ingest(&mut failed, sink).await;
            Err(err)
        }
    }
}
