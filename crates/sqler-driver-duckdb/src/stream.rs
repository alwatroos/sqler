// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The row stream of one statement.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqler_driver_api::sqler_core::Column;
use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};
use tokio::sync::mpsc;

use crate::session::{RowMessage, Started};

enum State {
    Columns,
    Streaming,
    Finished,
}

/// Event stream: `Columns`, then `Rows` batches, then `Done`.
pub(crate) struct DuckQueryStream {
    state: State,
    columns: Vec<Column>,
    rows_affected: Option<u64>,
    rows: mpsc::Receiver<RowMessage>,
    cancel: Arc<AtomicBool>,
}

impl DuckQueryStream {
    pub(crate) fn new(started: Started, cancel: Arc<AtomicBool>) -> Self {
        Self {
            state: State::Columns,
            columns: started.columns,
            rows_affected: started.rows_affected,
            rows: started.rows,
            cancel,
        }
    }

    async fn step(&mut self) -> Option<Result<QueryEvent, DriverError>> {
        match self.state {
            State::Finished => None,
            State::Columns => {
                self.state = State::Streaming;
                Some(Ok(QueryEvent::Columns(std::mem::take(&mut self.columns))))
            }
            State::Streaming => Some(self.next_batch().await),
        }
    }

    async fn next_batch(&mut self) -> Result<QueryEvent, DriverError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(self.fail(DriverError::Cancelled));
        }
        match self.rows.recv().await {
            Some(Ok(rows)) => Ok(QueryEvent::Rows(rows)),
            Some(Err(e)) => {
                let cancelled = self.cancel.load(Ordering::SeqCst);
                Err(self.fail(if cancelled { DriverError::Cancelled } else { e }))
            }
            None => {
                self.state = State::Finished;
                Ok(QueryEvent::Done {
                    rows_affected: self.rows_affected,
                })
            }
        }
    }

    fn fail(&mut self, err: DriverError) -> DriverError {
        self.state = State::Finished;
        // Closing the receiver makes the worker's next send fail, which ends its statement.
        self.rows.close();
        err
    }
}

impl QueryStream for DuckQueryStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(self.step())
    }
}
