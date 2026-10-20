// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The row stream of one statement.

use std::future::poll_fn;
use std::sync::Arc;

use futures_core::stream::BoxStream;
use sqler_driver_api::sqler_core::{Column, DataType};
use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};
use sqlx::sqlite::SqliteRow;

use crate::convert::{decode_row, query_error};
use crate::session::SqliteCanceller;

/// Rows per `QueryEvent::Rows`.
const BATCH: usize = 256;

enum State {
    Columns,
    Streaming,
    Ending,
    Finished,
}

/// Event stream: `Columns`, then `Rows` batches, then `Done`.
pub(crate) struct SqliteQueryStream<'s> {
    state: State,
    columns: Vec<Column>,
    types: Vec<DataType>,
    rows: Option<BoxStream<'s, Result<SqliteRow, sqlx::Error>>>,
    rows_affected: Option<u64>,
    cancel: Arc<SqliteCanceller>,
}

impl<'s> SqliteQueryStream<'s> {
    /// A stream over a statement with result columns.
    pub(crate) fn rows(
        columns: Vec<Column>,
        rows: BoxStream<'s, Result<SqliteRow, sqlx::Error>>,
        cancel: Arc<SqliteCanceller>,
    ) -> Self {
        Self {
            state: State::Columns,
            types: columns.iter().map(|c| c.data_type).collect(),
            columns,
            rows: Some(rows),
            rows_affected: None,
            cancel,
        }
    }

    /// A stream for a statement without columns that already ran.
    pub(crate) fn affected(rows_affected: u64, cancel: Arc<SqliteCanceller>) -> Self {
        Self {
            state: State::Columns,
            columns: Vec::new(),
            types: Vec::new(),
            rows: None,
            rows_affected: Some(rows_affected),
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
                    State::Ending
                };
                Some(Ok(QueryEvent::Columns(std::mem::take(&mut self.columns))))
            }
            State::Ending => {
                self.state = State::Finished;
                Some(Ok(QueryEvent::Done {
                    rows_affected: self.rows_affected,
                }))
            }
            State::Streaming => Some(self.stream_batch().await),
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
        let types = &self.types;
        let mut ended = false;
        while batch.len() < BATCH {
            match poll_fn(|cx| rows.as_mut().poll_next(cx)).await {
                Some(Ok(row)) => match decode_row(&row, types) {
                    Ok(row) => batch.push(row),
                    Err(e) => return Err(self.fail(e)),
                },
                Some(Err(e)) => return Err(self.fail(query_error(e))),
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
            self.state = State::Finished;
            Ok(QueryEvent::Done {
                rows_affected: None,
            })
        } else {
            self.state = State::Ending;
            Ok(QueryEvent::Rows(batch))
        }
    }

    fn fail(&mut self, err: DriverError) -> DriverError {
        self.state = State::Finished;
        self.rows = None;
        err
    }
}

impl QueryStream for SqliteQueryStream<'_> {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(self.step())
    }
}
