// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The row stream of one statement.

use std::future::poll_fn;
use std::sync::Arc;

use futures_core::stream::BoxStream;
use sqler_driver_api::sqler_core::Column;
use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};
use sqlx::mysql::{MySqlQueryResult, MySqlRow};

use crate::convert::decode_row;
use crate::session::MySqlCanceller;

/// Rows per `QueryEvent::Rows`.
const BATCH: usize = 256;

type ExecFuture<'s> = BoxFuture<'s, Result<MySqlQueryResult, sqlx::Error>>;

enum State {
    Columns,
    Streaming,
    Executing,
    Ending,
    Finished,
}

/// Event stream: `Columns`, then `Rows` batches, then `Done`.
pub(crate) struct MySqlQueryStream<'s> {
    state: State,
    columns: Vec<Column>,
    rows: Option<BoxStream<'s, Result<MySqlRow, sqlx::Error>>>,
    exec: Option<ExecFuture<'s>>,
    cancel: Arc<MySqlCanceller>,
}

impl<'s> MySqlQueryStream<'s> {
    /// A stream over a statement with result columns.
    pub(crate) fn rows(
        columns: Vec<Column>,
        rows: BoxStream<'s, Result<MySqlRow, sqlx::Error>>,
        cancel: Arc<MySqlCanceller>,
    ) -> Self {
        Self {
            state: State::Columns,
            columns,
            rows: Some(rows),
            exec: None,
            cancel,
        }
    }

    /// A stream over a statement without result columns; `exec` has not run yet, so a cancel
    /// always surfaces in the stream.
    pub(crate) fn executing(exec: ExecFuture<'s>, cancel: Arc<MySqlCanceller>) -> Self {
        Self {
            state: State::Columns,
            columns: Vec::new(),
            rows: None,
            exec: Some(exec),
            cancel,
        }
    }

    async fn step(&mut self) -> Option<Result<QueryEvent, DriverError>> {
        match self.state {
            State::Finished => None,
            State::Columns => {
                self.state = if self.rows.is_some() {
                    State::Streaming
                } else {
                    State::Executing
                };
                Some(Ok(QueryEvent::Columns(std::mem::take(&mut self.columns))))
            }
            State::Executing => Some(self.execute().await),
            State::Ending => {
                self.finish();
                Some(Ok(QueryEvent::Done {
                    rows_affected: None,
                }))
            }
            State::Streaming => Some(self.stream_batch().await),
        }
    }

    async fn execute(&mut self) -> Result<QueryEvent, DriverError> {
        if self.cancel.is_cancelled() {
            return Err(self.fail(DriverError::Cancelled));
        }
        let Some(exec) = self.exec.take() else {
            self.finish();
            return Ok(QueryEvent::Done {
                rows_affected: None,
            });
        };
        let result = exec.await;
        // A killed `DO SLEEP()` returns OK instead of an error, so the flag decides.
        if self.cancel.is_cancelled() {
            return Err(self.fail(DriverError::Cancelled));
        }
        match result {
            Ok(done) => {
                self.finish();
                Ok(QueryEvent::Done {
                    rows_affected: Some(done.rows_affected()),
                })
            }
            Err(e) => {
                let err = self.cancel.map_error(e);
                Err(self.fail(err))
            }
        }
    }

    async fn stream_batch(&mut self) -> Result<QueryEvent, DriverError> {
        if self.cancel.is_cancelled() {
            return Err(self.fail(DriverError::Cancelled));
        }
        let Some(rows) = self.rows.as_mut() else {
            self.state = State::Finished;
            return Ok(QueryEvent::Done {
                rows_affected: None,
            });
        };
        let mut batch = Vec::new();
        let mut ended = false;
        while batch.len() < BATCH {
            let polled = poll_fn(|cx| rows.as_mut().poll_next(cx)).await;
            // A killed `SLEEP()` returns 1 instead of an error, so every poll result counts,
            // the end of the rows included.
            if self.cancel.is_cancelled() {
                return Err(self.fail(DriverError::Cancelled));
            }
            match polled {
                Some(Ok(row)) => match decode_row(&row) {
                    Ok(row) => batch.push(row),
                    Err(e) => return Err(self.fail(e)),
                },
                Some(Err(e)) => {
                    let err = self.cancel.map_error(e);
                    return Err(self.fail(err));
                }
                None => {
                    ended = true;
                    break;
                }
            }
        }
        if !ended {
            return Ok(QueryEvent::Rows(batch));
        }
        self.rows = None;
        if batch.is_empty() {
            self.finish();
            Ok(QueryEvent::Done {
                rows_affected: None,
            })
        } else {
            self.state = State::Ending;
            Ok(QueryEvent::Rows(batch))
        }
    }

    fn finish(&mut self) {
        self.state = State::Finished;
        self.rows = None;
        self.exec = None;
        self.cancel.finish();
    }

    fn fail(&mut self, err: DriverError) -> DriverError {
        self.finish();
        err
    }
}

impl Drop for MySqlQueryStream<'_> {
    fn drop(&mut self) {
        self.cancel.finish();
    }
}

impl QueryStream for MySqlQueryStream<'_> {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(self.step())
    }
}
