// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `PostgreSQL` session.

use std::sync::Arc;

use sqler_driver_api::sqler_core::{ConnectionConfig, Query};
use sqler_driver_api::{BoxFuture, CancelHandle, DriverError, Introspect, QueryStream, Session};
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::{Connection as _, Executor as _, Statement as _};

use crate::cancel::PgCanceller;
use crate::convert::{bind_params, column, query_error, sql_str};
use crate::options::{connect_options, server_name_override};
use crate::stream::PgQueryStream;

/// The local TLS bridge kept alive by the session and its cancel handles.
#[cfg(unix)]
pub(crate) type Bridge = crate::tls_bridge::TlsBridge;
/// No bridge exists on this platform.
#[cfg(not(unix))]
pub(crate) type Bridge = std::convert::Infallible;

/// Redirects the connection through a local bridge when the certificate name differs from the
/// dial host. Returns the (possibly rewritten) options and the bridge that must outlive them.
fn apply_server_name(
    config: &ConnectionConfig,
    opts: PgConnectOptions,
) -> Result<(PgConnectOptions, Option<Bridge>), DriverError> {
    let Some(name) = server_name_override(config, &opts) else {
        return Ok((opts, None));
    };
    #[cfg(unix)]
    {
        let bridge = Bridge::open(
            opts.get_host(),
            opts.get_port(),
            &format!(".s.PGSQL.{}", opts.get_port()),
        )?;
        let opts = opts.host(&name).socket(bridge.dir());
        Ok((opts, Some(bridge)))
    }
    #[cfg(not(unix))]
    {
        let _ = (name, opts);
        Err(DriverError::Unsupported(
            "verify-full with a server name other than the dial host needs Unix domain sockets, \
             which this platform's TLS backend cannot use"
                .to_owned(),
        ))
    }
}

/// One open `PostgreSQL` connection.
pub(crate) struct PgSession {
    pub(crate) conn: PgConnection,
    cancel: Arc<PgCanceller>,
}

impl PgSession {
    pub(crate) async fn connect(config: &ConnectionConfig) -> Result<Self, DriverError> {
        let (opts, bridge) = apply_server_name(config, connect_options(config)?)?;
        let mut conn = PgConnection::connect_with(&opts)
            .await
            .map_err(|e| DriverError::Connection(e.to_string()))?;
        let pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
            .fetch_one(&mut conn)
            .await
            .map_err(|e| DriverError::Connection(e.to_string()))?;
        Ok(Self {
            conn,
            cancel: Arc::new(PgCanceller::new(opts, pid, bridge)),
        })
    }

    async fn run<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> Result<Box<dyn QueryStream + 's>, DriverError> {
        let cancel = self.cancel.clone();
        let (bound, param_types) = bind_params(&query.sql, &query.params)?;
        let statement = (&mut self.conn)
            .prepare_with(sql_str(&query.sql), &param_types)
            .await
            .map_err(|e| query_error(e, cancel.is_requested()))?;
        let columns: Vec<_> = statement.columns().iter().map(column).collect();
        let stream = if columns.is_empty() {
            PgQueryStream::executing(Box::pin(bound.execute(&mut self.conn)), cancel)
        } else {
            PgQueryStream::rows(columns, bound.fetch(&mut self.conn), cancel)
        };
        Ok(Box::new(stream) as Box<dyn QueryStream + 's>)
    }
}

impl Session for PgSession {
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
            self.cancel.begin();
            let cancel = self.cancel.clone();
            let result = self.run(query).await;
            if result.is_err() {
                cancel.finish();
            }
            result
        })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(async move { self.conn.close().await.map_err(|e| query_error(e, false)) })
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use sqler_driver_api::sqler_core::{DriverId, TlsConfig, TlsMode};

    use super::*;

    #[test]
    fn server_name_is_unsupported_without_unix_sockets() {
        let cfg = ConnectionConfig::new(
            "t",
            DriverId::new("postgres").unwrap(),
            "postgres://u:p@127.0.0.1:1/x",
        )
        .with_tls(TlsConfig::new(TlsMode::VerifyFull).with_server_name("db.internal"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt.block_on(PgSession::connect(&cfg)).err().unwrap();
        assert!(matches!(err, DriverError::Unsupported(_)), "{err:?}");
    }
}
