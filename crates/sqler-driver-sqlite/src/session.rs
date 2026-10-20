// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `SQLite` session.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqler_driver_api::sqler_core::{ConnectionConfig, Query};
use sqler_driver_api::{
    BoxFuture, CancelHandle, Canceller, DriverError, Introspect, QueryStream, Session,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection as _, Executor as _, Statement as _};
use std::str::FromStr as _;

use crate::convert::{bind_params, column, query_error, sql_str};
use crate::stream::SqliteQueryStream;

/// Cooperative cancel flag, reset at the start of each `execute`.
#[derive(Debug, Default)]
pub(crate) struct SqliteCanceller(AtomicBool);

impl SqliteCanceller {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Canceller for SqliteCanceller {
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
        self.0.store(true, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(())))
    }
}

/// Builds connect options from `ConnectionConfig::target`.
fn options(target: &str) -> Result<SqliteConnectOptions, sqlx::Error> {
    let opts = if target == ":memory:" {
        SqliteConnectOptions::new()
            .filename(":memory:")
            .in_memory(true)
    } else if target.starts_with("sqlite:") {
        SqliteConnectOptions::from_str(target)?
    } else {
        SqliteConnectOptions::new().filename(target)
    };
    Ok(opts.create_if_missing(true))
}

/// One open `SQLite` connection.
pub(crate) struct SqliteSession {
    pub(crate) conn: SqliteConnection,
    cancel: Arc<SqliteCanceller>,
}

impl SqliteSession {
    pub(crate) async fn connect(config: &ConnectionConfig) -> Result<Self, DriverError> {
        let opts = options(&config.target).map_err(|e| DriverError::Connection(e.to_string()))?;
        let conn = SqliteConnection::connect_with(&opts)
            .await
            .map_err(|e| DriverError::Connection(e.to_string()))?;
        Ok(Self {
            conn,
            cancel: Arc::new(SqliteCanceller::default()),
        })
    }
}

impl Session for SqliteSession {
    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::new(self.cancel.clone())
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        Some(self)
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(async move {
            self.cancel.reset();
            let statement = (&mut self.conn)
                .prepare(sql_str(&query.sql))
                .await
                .map_err(query_error)?;
            let columns: Vec<_> = statement.columns().iter().map(column).collect();
            let bound = bind_params(&query.sql, &query.params)?;
            if columns.is_empty() {
                let done = bound.execute(&mut self.conn).await.map_err(query_error)?;
                Ok(Box::new(SqliteQueryStream::affected(
                    done.rows_affected(),
                    self.cancel.clone(),
                )) as Box<dyn QueryStream + 's>)
            } else {
                let rows = bound.fetch(&mut self.conn);
                Ok(
                    Box::new(SqliteQueryStream::rows(columns, rows, self.cancel.clone()))
                        as Box<dyn QueryStream + 's>,
                )
            }
        })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(async move { self.conn.close().await.map_err(query_error) })
    }
}
