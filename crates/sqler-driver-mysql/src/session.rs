// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `MySQL` session and its canceller.

use std::str::FromStr as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sqler_driver_api::sqler_core::{ConnectionConfig, Query, TlsMode};
use sqler_driver_api::{
    BoxFuture, CancelHandle, Canceller, DriverError, Introspect, QueryStream, Session,
};
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlSslMode};
use sqlx::{AssertSqlSafe, Connection as _, Executor as _, Row as _, Statement as _};

use crate::convert::{ER_QUERY_INTERRUPTED, bind_params, column, mysql_code, query_error, sql_str};
use crate::stream::MySqlQueryStream;
#[cfg(unix)]
use crate::tls_bridge::TlsBridge;

/// Keeps the TLS bridge alive; it only exists on Unix.
#[cfg(unix)]
type Bridge = TlsBridge;
#[cfg(not(unix))]
type Bridge = ();

/// `ER_UNSUPPORTED_PS`: the statement cannot be prepared.
const ER_UNSUPPORTED_PS: u16 = 1295;
/// `ER_NO_SUCH_THREAD`: the connection to kill is already gone.
const ER_NO_SUCH_THREAD: u16 = 1094;

/// How long a cancel request may take, connect included.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(10);

/// Maps a [`TlsMode`] onto the `sqlx` ssl mode.
///
/// A mode this version does not know is treated as `Required`, never as a weaker one.
fn ssl_mode(mode: TlsMode) -> MySqlSslMode {
    match mode {
        TlsMode::Disable => MySqlSslMode::Disabled,
        TlsMode::Prefer => MySqlSslMode::Preferred,
        TlsMode::VerifyCa => MySqlSslMode::VerifyCa,
        TlsMode::VerifyFull => MySqlSslMode::VerifyIdentity,
        _ => MySqlSslMode::Required,
    }
}

/// Builds connect options from `ConnectionConfig::target` (`mysql://` or `mariadb://`).
///
/// URL parameters (`ssl-mode`, `ssl-ca`, ...) apply when `config.tls` is `None`; a `Some` TLS
/// config overrides the mode and the certificate paths. The error text never contains the
/// target, because it may hold a password.
pub(crate) fn options(config: &ConnectionConfig) -> Result<MySqlConnectOptions, DriverError> {
    let mut opts = MySqlConnectOptions::from_str(&config.target)
        .map_err(|e| DriverError::Connection(e.to_string()))?;
    if let Some(tls) = &config.tls {
        opts = opts.ssl_mode(ssl_mode(tls.mode));
        if let Some(ca) = &tls.ca_cert {
            opts = opts.ssl_ca(ca);
        }
        if let Some(cert) = &tls.client_cert {
            opts = opts.ssl_client_cert(cert);
        }
        if let Some(key) = &tls.client_key {
            opts = opts.ssl_client_key(key);
        }
    }
    Ok(opts)
}

/// The name to verify the certificate against when it differs from the dial host.
///
/// `Some` only when the effective mode is `VerifyIdentity`, the target is TCP and the configured
/// server name differs from `opts.get_host()` (ASCII case-insensitive, `[]` trimmed). In every
/// other case the name is ignored.
pub(crate) fn server_name_override(
    config: &ConnectionConfig,
    opts: &MySqlConnectOptions,
) -> Option<String> {
    let name = config.tls.as_ref()?.server_name.as_deref()?;
    if !matches!(opts.get_ssl_mode(), MySqlSslMode::VerifyIdentity) || opts.get_socket().is_some() {
        return None;
    }
    let trim = |s: &str| s.trim_matches(['[', ']']).to_owned();
    let (name, host) = (trim(name), trim(opts.get_host()));
    if name.is_empty() || name.eq_ignore_ascii_case(&host) {
        return None;
    }
    Some(name)
}

/// Points the options at a local bridge so the certificate is verified against `name`.
#[cfg(unix)]
fn bridged(
    opts: MySqlConnectOptions,
    name: &str,
) -> Result<(MySqlConnectOptions, Option<Bridge>), DriverError> {
    let bridge = TlsBridge::open(opts.get_host(), opts.get_port(), "mysql.sock")?;
    let opts = opts.host(name).socket(bridge.socket_path());
    Ok((opts, Some(bridge)))
}

#[cfg(not(unix))]
fn bridged(
    _opts: MySqlConnectOptions,
    _name: &str,
) -> Result<(MySqlConnectOptions, Option<Bridge>), DriverError> {
    Err(DriverError::Unsupported(
        "verify-full with a server name other than the dial host needs Unix domain sockets, \
         which this platform's TLS backend cannot use"
            .to_owned(),
    ))
}

/// Cancels the running statement: sets a flag, then runs `KILL QUERY` from a side connection.
///
/// It holds the connect options (and so the password), which is why it has no `Debug`.
pub(crate) struct MySqlCanceller {
    flag: AtomicBool,
    active: AtomicBool,
    opts: MySqlConnectOptions,
    id: u64,
    /// The TLS bridge the options point at; it lives as long as the session or a cancel handle.
    _bridge: Option<Bridge>,
}

impl MySqlCanceller {
    fn new(opts: MySqlConnectOptions, id: u64, bridge: Option<Bridge>) -> Self {
        Self {
            flag: AtomicBool::new(false),
            active: AtomicBool::new(false),
            opts,
            id,
            _bridge: bridge,
        }
    }

    /// Whether a cancel was requested since the last `begin`.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Starts a statement: forgets an earlier request and allows the server round trip.
    fn begin(&self) {
        self.flag.store(false, Ordering::SeqCst);
        self.active.store(true, Ordering::SeqCst);
    }

    /// Ends the statement; a later cancel only sets the flag.
    pub(crate) fn finish(&self) {
        self.active.store(false, Ordering::SeqCst);
    }

    /// Maps an error of the running statement: `1317`, or any error after a cancel request,
    /// is `Cancelled`.
    pub(crate) fn map_error(&self, err: sqlx::Error) -> DriverError {
        if self.is_cancelled() || mysql_code(&err) == Some(ER_QUERY_INTERRUPTED) {
            DriverError::Cancelled
        } else {
            query_error(err)
        }
    }

    async fn kill(&self) -> Result<(), DriverError> {
        let mut side = MySqlConnection::connect_with(&self.opts)
            .await
            .map_err(|e| DriverError::Connection(e.to_string()))?;
        let killed = sqlx::raw_sql(AssertSqlSafe(format!("KILL QUERY {}", self.id)))
            .execute(&mut side)
            .await;
        let _ = side.close().await;
        match killed {
            Ok(_) => Ok(()),
            Err(e) if mysql_code(&e) == Some(ER_NO_SUCH_THREAD) => Ok(()),
            Err(e) => Err(query_error(e)),
        }
    }
}

impl Canceller for MySqlCanceller {
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
        self.flag.store(true, Ordering::SeqCst);
        Box::pin(async move {
            if !self.active.load(Ordering::SeqCst) {
                return Ok(());
            }
            tokio::time::timeout(CANCEL_TIMEOUT, self.kill())
                .await
                .unwrap_or_else(|_| {
                    Err(DriverError::Connection(
                        "cancel request timed out".to_owned(),
                    ))
                })
        })
    }
}

/// One open `MySQL` connection.
pub(crate) struct MySqlSession {
    pub(crate) conn: MySqlConnection,
    cancel: Arc<MySqlCanceller>,
}

impl MySqlSession {
    pub(crate) async fn connect(config: &ConnectionConfig) -> Result<Self, DriverError> {
        let mut opts = options(config)?;
        let mut bridge = None;
        if let Some(name) = server_name_override(config, &opts) {
            (opts, bridge) = bridged(opts, &name)?;
        }
        let mut conn = MySqlConnection::connect_with(&opts)
            .await
            .map_err(|e| DriverError::Connection(e.to_string()))?;
        let row = sqlx::query("select connection_id()")
            .fetch_one(&mut conn)
            .await
            .map_err(query_error)?;
        let id: u64 = row.try_get(0).map_err(query_error)?;
        Ok(Self {
            conn,
            cancel: Arc::new(MySqlCanceller::new(opts, id, bridge)),
        })
    }
}

impl MySqlSession {
    async fn run<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> Result<Box<dyn QueryStream + 's>, DriverError> {
        let cancel = self.cancel.clone();
        let statement = match (&mut self.conn).prepare(sql_str(&query.sql)).await {
            Ok(statement) => statement,
            Err(e) if query.params.is_empty() && mysql_code(&e) == Some(ER_UNSUPPORTED_PS) => {
                // `CREATE PROCEDURE` and the like cannot be prepared: use the text protocol.
                let exec = sqlx::raw_sql(AssertSqlSafe(query.sql.clone())).execute(&mut self.conn);
                return Ok(Box::new(MySqlQueryStream::executing(
                    Box::pin(exec),
                    cancel,
                )));
            }
            Err(e) => return Err(cancel.map_error(e)),
        };
        let columns: Vec<_> = statement.columns().iter().map(column).collect();
        let bound = bind_params(&query.sql, &query.params)?;
        let stream = if columns.is_empty() {
            MySqlQueryStream::executing(Box::pin(bound.execute(&mut self.conn)), cancel)
        } else {
            MySqlQueryStream::rows(columns, bound.fetch(&mut self.conn), cancel)
        };
        Ok(Box::new(stream))
    }
}

impl Session for MySqlSession {
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
        Box::pin(async move { self.conn.close().await.map_err(query_error) })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::{DriverId, TlsConfig};

    fn config(target: &str) -> ConnectionConfig {
        ConnectionConfig::new("t", DriverId::new("mysql").unwrap(), target)
    }

    fn canceller() -> MySqlCanceller {
        let opts = MySqlConnectOptions::from_str("mysql://u:s3cret@127.0.0.1:1/x").unwrap();
        MySqlCanceller::new(opts, 42, None)
    }

    #[test]
    fn cancel_while_idle_only_sets_the_flag() {
        let c = canceller();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Not active: no connection is attempted (port 1 would fail), so this is Ok.
        rt.block_on(c.cancel()).unwrap();
        assert!(c.is_cancelled());
        c.begin();
        assert!(!c.is_cancelled());
        c.finish();
        assert!(!c.active.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_while_active_fails_without_secret() {
        let c = canceller();
        c.begin();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(c.cancel()).unwrap_err();
        assert!(matches!(err, DriverError::Connection(_)));
        assert!(!err.to_string().contains("s3cret"));
    }

    #[test]
    fn tls_modes_map() {
        assert!(matches!(ssl_mode(TlsMode::Disable), MySqlSslMode::Disabled));
        assert!(matches!(ssl_mode(TlsMode::Prefer), MySqlSslMode::Preferred));
        assert!(matches!(ssl_mode(TlsMode::Require), MySqlSslMode::Required));
        assert!(matches!(
            ssl_mode(TlsMode::VerifyCa),
            MySqlSslMode::VerifyCa
        ));
        assert!(matches!(
            ssl_mode(TlsMode::VerifyFull),
            MySqlSslMode::VerifyIdentity
        ));
    }

    #[test]
    fn both_url_schemes_parse() {
        for scheme in ["mysql", "mariadb"] {
            let url = format!("{scheme}://u:p@db.example:3307/app");
            let opts = options(&config(&url)).unwrap();
            assert_eq!(opts.get_host(), "db.example");
            assert_eq!(opts.get_port(), 3307);
            assert_eq!(opts.get_username(), "u");
            assert_eq!(opts.get_database(), Some("app"));
            assert!(matches!(opts.get_ssl_mode(), MySqlSslMode::Preferred));
        }
    }

    #[test]
    fn url_ssl_mode_applies_without_tls_config() {
        let opts = options(&config("mysql://u@h/d?ssl-mode=required")).unwrap();
        assert!(matches!(opts.get_ssl_mode(), MySqlSslMode::Required));
    }

    #[test]
    fn tls_config_overrides_url() {
        let cfg = config("mysql://u@h/d?ssl-mode=disabled")
            .with_tls(TlsConfig::new(TlsMode::VerifyFull).with_ca_cert("ca.pem"));
        let opts = options(&cfg).unwrap();
        assert!(matches!(opts.get_ssl_mode(), MySqlSslMode::VerifyIdentity));
    }

    fn named(url: &str, mode: TlsMode, name: &str) -> (ConnectionConfig, MySqlConnectOptions) {
        let cfg = config(url).with_tls(TlsConfig::new(mode).with_server_name(name));
        let opts = options(&cfg).unwrap();
        (cfg, opts)
    }

    #[test]
    fn server_name_applies_to_verify_full_with_a_different_name() {
        let (cfg, opts) = named("mysql://u@127.0.0.1/d", TlsMode::VerifyFull, "db.internal");
        assert_eq!(
            server_name_override(&cfg, &opts).as_deref(),
            Some("db.internal")
        );
    }

    #[test]
    fn server_name_is_ignored_when_it_matches_the_host() {
        for name in ["127.0.0.1", "[127.0.0.1]"] {
            let (cfg, opts) = named("mysql://u@127.0.0.1/d", TlsMode::VerifyFull, name);
            assert_eq!(server_name_override(&cfg, &opts), None, "{name}");
        }
        let (cfg, opts) = named(
            "mysql://u@DB.Internal/d",
            TlsMode::VerifyFull,
            "db.internal",
        );
        assert_eq!(server_name_override(&cfg, &opts), None);
    }

    #[test]
    fn server_name_is_ignored_in_other_modes_or_without_a_name() {
        for mode in [
            TlsMode::Disable,
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyCa,
        ] {
            let (cfg, opts) = named("mysql://u@127.0.0.1/d", mode, "db.internal");
            assert_eq!(server_name_override(&cfg, &opts), None, "{mode:?}");
        }
        let cfg = config("mysql://u@127.0.0.1/d").with_tls(TlsConfig::new(TlsMode::VerifyFull));
        let opts = options(&cfg).unwrap();
        assert_eq!(server_name_override(&cfg, &opts), None);
        let cfg = config("mysql://u@127.0.0.1/d");
        let opts = options(&cfg).unwrap();
        assert_eq!(server_name_override(&cfg, &opts), None);
    }

    #[test]
    fn server_name_is_ignored_for_a_socket_target() {
        let (cfg, opts) = named(
            "mysql://u@127.0.0.1/d?socket=/tmp/x.sock",
            TlsMode::VerifyFull,
            "db.internal",
        );
        assert!(opts.get_socket().is_some());
        assert_eq!(server_name_override(&cfg, &opts), None);
    }

    #[cfg(not(unix))]
    #[test]
    fn server_name_without_unix_sockets_is_unsupported() {
        let (cfg, _) = named(
            "mysql://u@127.0.0.1:1/d",
            TlsMode::VerifyFull,
            "db.internal",
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(MySqlSession::connect(&cfg)).err().unwrap();
        assert!(matches!(err, DriverError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn bad_target_is_connection_error_without_the_target() {
        let target = "mysql://user:s3cret-pw@host:notaport/db";
        let err = options(&config(target)).unwrap_err();
        assert!(matches!(err, DriverError::Connection(_)));
        let text = err.to_string();
        assert!(!text.contains("s3cret-pw"), "{text}");
        assert!(!text.contains(target), "{text}");
    }
}
