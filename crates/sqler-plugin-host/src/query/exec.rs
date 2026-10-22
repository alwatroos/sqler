// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `query.run`: the checks in their pinned order, then one statement under row, byte and time caps.

use std::sync::Arc;
use std::time::Duration;

use sqler_driver_api::sqler_core::{Query, Value};
use sqler_driver_api::{CancelHandle, QueryEvent, QueryStream, Session};
use sqler_plugin_api::{CapabilityKind, CapabilitySet, QueryLimits};
use tokio::time::{Instant, timeout, timeout_at};

use super::convert::{cell_cost, clip, data_type_to_wit, type_name, value_from_wit, value_to_wit};
use super::guard::{self, Planned};
use super::service::{
    DataError, DataService, MAX_PARAMS, MAX_RESULT_BYTES, MAX_SQL_BYTES, PluginConnection,
    PluginData,
};
use crate::discovery::DiscoveredPlugin;
use crate::error::ErrorCode;
use crate::runtime::plugin_error;
use crate::runtime::wit::query::{QueryRequest, QueryResult};
use crate::runtime::wit::types::{self as wit, PluginError};

/// The message of a granted call when the host has no [`PluginData`].
pub(crate) const NOT_AVAILABLE: &str = "not available in this host";

/// How long the cleanup after a deadline or a drop (cancel, release) may take.
const GRACE: Duration = Duration::from_secs(2);

/// What `run_guarded` needs from the plugin it serves.
pub(crate) struct QueryCtx<'a> {
    /// The calling plugin.
    pub plugin: &'a DiscoveredPlugin,
    /// What it may do now.
    pub effective: &'a CapabilitySet,
    /// The application's data, if it provided any.
    pub data: Option<Arc<PluginData>>,
    /// Row and time bounds.
    pub limits: QueryLimits,
}

fn denied(what: &str) -> PluginError {
    plugin_error(
        ErrorCode::CapabilityDenied,
        format!("`{what}` is not granted to this plugin"),
    )
}

fn invalid(message: impl Into<String>) -> PluginError {
    plugin_error(ErrorCode::InvalidArgument, message)
}

/// What the plugin learns when no session could be had. A driver's connect, TLS or auth error can name the host, the
/// user or the database, and a plugin with `net.http` could send that out, so [`DataError::Driver`] never reaches it:
/// only a fixed text does. [`DataError::NotAvailable`] text is written by the host and passes.
pub(crate) fn data_error(e: &DataError) -> PluginError {
    match e {
        DataError::UnknownConnection(id) => plugin_error(
            ErrorCode::NotFound,
            format!("unknown connection `{}`", clip(id)),
        ),
        DataError::NotAvailable(text) => plugin_error(ErrorCode::Failed, clip(text)),
        _ => plugin_error(ErrorCode::Failed, "the connection could not be opened"),
    }
}

/// A lent session. If the host call is dropped before the session went back (the deadline, a cancel, a refresh of the
/// grants, a disable), `Drop` cancels the driver and releases the session from a spawned task.
struct SessionLease {
    source: Arc<dyn DataService>,
    connection: String,
    session: Option<Box<dyn Session>>,
    cancel: CancelHandle,
}

impl SessionLease {
    fn new(source: Arc<dyn DataService>, connection: String, session: Box<dyn Session>) -> Self {
        let cancel = session.cancel_handle();
        Self {
            source,
            connection,
            session: Some(session),
            cancel,
        }
    }

    fn session(&mut self) -> &mut dyn Session {
        // `release` is the only taker and it consumes the lease.
        self.session
            .as_deref_mut()
            .unwrap_or_else(|| unreachable!("the session is present until released"))
    }

    /// A handle that cancels the running statement; see [`cancel_statement`].
    fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    /// Gives the session back.
    async fn release(mut self) {
        if let Some(session) = self.session.take() {
            let _ = timeout(GRACE, self.source.release(&self.connection, session)).await;
        }
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // No runtime to run the cleanup on; the session closes with its own drop.
            return;
        };
        let source = Arc::clone(&self.source);
        let connection = std::mem::take(&mut self.connection);
        let cancel = self.cancel.clone();
        handle.spawn(async move {
            cancel_statement(cancel).await;
            let _ = timeout(GRACE, source.release(&connection, session)).await;
        });
    }
}

/// Cancels the running statement; a cancel that fails or hangs is given up after the grace period.
async fn cancel_statement(cancel: CancelHandle) {
    let _ = timeout(GRACE, cancel.cancel()).await;
}

/// What reading one statement's stream produced.
struct Read {
    columns: Vec<wit::Column>,
    rows: Vec<Vec<wit::Value>>,
    truncated: bool,
    rows_affected: Option<u64>,
}

/// Reads the stream into at most `max_rows` rows and [`MAX_RESULT_BYTES`] of cells. Hitting a cap stops reading and
/// sets `truncated`; it does not cancel. The stream is dropped when this returns.
async fn read_stream(
    mut stream: Box<dyn QueryStream + '_>,
    max_rows: usize,
) -> Result<Read, PluginError> {
    let mut out = Read {
        columns: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        rows_affected: None,
    };
    let mut bytes = 0_usize;
    loop {
        match stream.next_event().await {
            None => {
                return Err(plugin_error(
                    ErrorCode::Failed,
                    "the driver ended the result before it was complete",
                ));
            }
            Some(Err(e)) => return Err(plugin_error(ErrorCode::Failed, clip(&e.to_string()))),
            Some(Ok(QueryEvent::Columns(columns))) => {
                out.columns = columns
                    .iter()
                    .map(|c| wit::Column {
                        name: c.name.clone(),
                        data_type: data_type_to_wit(c.data_type),
                        type_name: type_name(c.native_type.as_deref(), c.data_type),
                    })
                    .collect();
            }
            Some(Ok(QueryEvent::Rows(batch))) => {
                for row in batch {
                    let cells: Vec<wit::Value> = row.0.iter().map(value_to_wit).collect();
                    let cost: usize = cells.iter().map(cell_cost).sum();
                    if out.rows.len() >= max_rows || bytes.saturating_add(cost) > MAX_RESULT_BYTES {
                        out.truncated = true;
                        return Ok(out);
                    }
                    bytes += cost;
                    out.rows.push(cells);
                }
            }
            Some(Ok(QueryEvent::Done { rows_affected })) => {
                out.rows_affected = rows_affected;
                return Ok(out);
            }
            Some(Ok(_)) => {}
        }
    }
}

/// The checks of `query.run` in the pinned order, then the run. See ADR-0058.
///
/// # Errors
/// A WIT error: `capability-denied`, `failed`, `invalid-argument`, `not-found`, `declined`, `timeout`.
pub(crate) async fn run_guarded(
    ctx: &QueryCtx<'_>,
    req: QueryRequest,
) -> Result<QueryResult, PluginError> {
    // 1. Some db capability.
    let can_read = ctx.effective.allows(CapabilityKind::DbRead);
    let can_write = ctx.effective.allows(CapabilityKind::DbWrite);
    if !can_read && !can_write {
        return Err(denied("query"));
    }
    // 2. The host has data to lend.
    let Some(data) = ctx.data.as_deref() else {
        return Err(plugin_error(ErrorCode::Failed, NOT_AVAILABLE));
    };
    // 3. Arguments.
    if req.sql.len() > MAX_SQL_BYTES {
        return Err(invalid(format!(
            "`sql` is longer than {MAX_SQL_BYTES} bytes"
        )));
    }
    if req.params.len() > MAX_PARAMS {
        return Err(invalid(format!("more than {MAX_PARAMS} parameters")));
    }
    if req.max_rows == Some(0) {
        return Err(invalid("`max-rows` must be at least 1"));
    }
    if req.timeout_ms == Some(0) {
        return Err(invalid("`timeout-ms` must be at least 1"));
    }
    let params: Vec<Value> = req
        .params
        .into_iter()
        .enumerate()
        .map(|(i, p)| value_from_wit(p).map_err(|m| invalid(format!("parameter {}: {m}", i + 1))))
        .collect::<Result<_, _>>()?;
    // 4. The connection.
    let conn: PluginConnection = data
        .source()
        .connections()
        .into_iter()
        .find(|c| c.id == req.connection)
        .ok_or_else(|| {
            plugin_error(
                ErrorCode::NotFound,
                format!("unknown connection `{}`", clip(&req.connection)),
            )
        })?;
    // 5. Exactly one statement, classified.
    let planned = guard::plan(&conn, &req.sql)?;
    // 6. The capability this class needs, then the approvals.
    if planned.needs_approval() {
        if !can_write {
            return Err(denied("db.write"));
        }
        guard::approve(data, ctx.plugin, &conn, &planned, &params).await?;
    } else if !can_read {
        return Err(denied("db.read"));
    }
    // 7. Run.
    let limits = ctx.limits;
    let max_rows = req
        .max_rows
        .unwrap_or(limits.max_rows_default)
        .min(limits.max_rows_cap);
    let time_cap = req
        .timeout_ms
        .map_or(limits.timeout_default, |ms| {
            Duration::from_millis(u64::from(ms))
        })
        .min(limits.timeout_cap);
    run_statement(
        data.source(),
        &conn.id,
        &planned,
        params,
        max_rows,
        time_cap,
    )
    .await
}

async fn run_statement(
    source: &Arc<dyn DataService>,
    connection: &str,
    planned: &Planned,
    params: Vec<Value>,
    max_rows: u32,
    time_cap: Duration,
) -> Result<QueryResult, PluginError> {
    let timed_out = || {
        plugin_error(
            ErrorCode::Timeout,
            format!(
                "the query did not finish within {} ms",
                time_cap.as_millis()
            ),
        )
    };
    let deadline = Instant::now() + time_cap;
    let session = match timeout_at(deadline, source.acquire(connection)).await {
        Err(_) => return Err(timed_out()),
        Ok(Err(e)) => return Err(data_error(&e)),
        Ok(Ok(session)) => session,
    };
    let mut lease = SessionLease::new(Arc::clone(source), connection.to_owned(), session);
    let query = Query::new(planned.sql.as_str()).with_params(params);
    let rows_cap = usize::try_from(max_rows).unwrap_or(usize::MAX);
    let outcome = timeout_at(deadline, async {
        let stream = lease
            .session()
            .execute(&query)
            .await
            .map_err(|e| plugin_error(ErrorCode::Failed, clip(&e.to_string())))?;
        read_stream(stream, rows_cap).await
    })
    .await;
    let result = if let Ok(result) = outcome {
        result
    } else {
        // The statement may still run on the server: ask the driver to stop it, return no partial rows.
        cancel_statement(lease.cancel_handle()).await;
        Err(timed_out())
    };
    lease.release().await;
    let read = result?;
    Ok(QueryResult {
        columns: read.columns,
        rows: read.rows,
        truncated: read.truncated,
        rows_affected: read.rows_affected,
    })
}
