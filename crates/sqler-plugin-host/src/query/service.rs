// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The host-side traits through which an application lends databases to plugins, and the caps that bound a query.
//!
//! Modelled on `sqler-ai-tools` (`SessionSource`, `Approver`; ADR-0036). The plugin host depends on neither the app nor
//! the UI: the app implements [`DataService`] and [`QueryApprover`] and puts one [`PluginData`] into
//! [`Services`](crate::Services).

use std::fmt;
use std::sync::Arc;

use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::{DriverId, Environment, Value};
use sqler_driver_api::{BoxFuture, Session};
use sqler_plugin_api::PluginId;
use sqler_sql::StatementClass;
use thiserror::Error;

/// The most bytes of result cells one `query.run` returns: the payload of every cell plus 16 bytes per cell. It fits
/// under the 64 MiB guest memory cap.
pub const MAX_RESULT_BYTES: usize = 8 << 20;
/// The longest SQL text of one `query.run`, in bytes.
pub const MAX_SQL_BYTES: usize = 1 << 20;
/// The most parameters of one `query.run`.
pub const MAX_PARAMS: usize = 1000;
/// The most objects (or columns) one `catalog` call returns.
pub const MAX_CATALOG_ITEMS: usize = 10_000;

/// What a plugin may know about a connection. Never carries a target or a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PluginConnection {
    /// The id plugins use to name the connection.
    pub id: String,
    /// The display name.
    pub name: String,
    /// The driver id.
    pub driver: DriverId,
    /// The environment tag, if the user set one. `prod` asks for a second confirmation of every write.
    pub environment: Option<Environment>,
    /// Whether the connection refuses everything but plain reads.
    pub read_only: bool,
}

impl PluginConnection {
    /// Creates a connection description; `read_only` is `false`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        driver: DriverId,
        environment: Option<Environment>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            driver,
            environment,
            read_only: false,
        }
    }

    /// Sets the read-only flag.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }
}

/// Why a [`DataService`] could not lend a session.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DataError {
    /// The service has no connection of this id.
    #[error("unknown connection `{0}`")]
    UnknownConnection(String),
    /// The connection exists but cannot be used now (for example, the user holds a transaction on it).
    ///
    /// The text **reaches the plugin unchanged** (clipped to 512 characters). The application must write it itself and
    /// never put a host, user, database name, path or secret in it.
    #[error("{0}")]
    NotAvailable(String),
    /// The driver failed to connect. The text is for the application's own log: the plugin sees only a fixed message,
    /// because connect, TLS and auth errors can name a target or a user.
    #[error("{0}")]
    Driver(String),
}

/// How the application lends database access to plugins (ADR-0058).
///
/// The application decides which connections plugins may see (the filter is its policy) and never lends a session on
/// which the user holds an open transaction. The host asks for a session only to run one guarded statement and always
/// hands it back through [`DataService::release`], also when the plugin call is cancelled.
pub trait DataService: Send + Sync {
    /// The connections plugins may use.
    fn connections(&self) -> Vec<PluginConnection>;

    /// The catalog of a connection, if the application has one.
    fn catalog(&self, connection: &str) -> Option<Catalog>;

    /// Lends a session for one statement.
    ///
    /// # Errors
    /// [`DataError::UnknownConnection`], [`DataError::NotAvailable`] or [`DataError::Driver`] if no session can be had.
    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DataError>>;

    /// Takes a lent session back. The default closes it and ignores the error.
    ///
    /// An implementation must **close the session or reset it completely** and must never return it to a pool that
    /// the user's tabs draw from. An approved `BEGIN`, `SET ROLE`, `SET search_path`, `USE`, a `SQLite` `ATTACH` or a
    /// temporary table changes the state of the session, and a plugin statement must not leak that state into the
    /// user's own work or into a later plugin call. A pooled session is acceptable only if it is rolled back and every
    /// setting is discarded first. On `PostgreSQL` and `MySQL` closing a session whose result was cut short (row or byte
    /// cap) may drain the remaining rows, with a 2 s grace period (`release` runs under it).
    fn release(&self, connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        let _ = connection;
        Box::pin(async move {
            let _ = session.close().await;
        })
    }
}

/// The answer of a [`QueryApprover`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// Run the statement.
    Approve,
    /// Do not run it.
    Deny,
}

/// Which confirmation is being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApprovalStage {
    /// The first (and, outside `prod`, only) confirmation.
    Confirm,
    /// The second confirmation that every statement on a `prod` connection needs.
    ProdSecond,
}

/// Why a statement needs an approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteReason {
    /// It changes data.
    Write,
    /// It changes the schema or permissions.
    Ddl,
    /// Transaction control or settings.
    Other,
    /// The classifier does not recognise it.
    Unknown,
    /// An `EXPLAIN` that also runs the query.
    ExplainAnalyze,
}

impl WriteReason {
    /// A stable lowercase name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Ddl => "ddl",
            Self::Other => "other",
            Self::Unknown => "unknown",
            Self::ExplainAnalyze => "explain-analyze",
        }
    }
}

/// A statement that wants to run and needs the user's approval.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct WriteRequest {
    /// The plugin that asks.
    pub plugin: PluginId,
    /// Its display name.
    pub plugin_name: String,
    /// The connection the statement targets.
    pub connection: PluginConnection,
    /// The exact text that will be sent to the driver.
    pub sql: String,
    /// The exact parameters that will be bound.
    pub params: Vec<Value>,
    /// The classifier's verdict.
    pub class: StatementClass,
    /// Why approval is needed.
    pub reason: WriteReason,
    /// Which confirmation this is.
    pub stage: ApprovalStage,
}

impl WriteRequest {
    /// A request. The host builds them itself; applications need this to test their approver.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // one field each; the struct is `non_exhaustive`
    pub fn new(
        plugin: PluginId,
        plugin_name: impl Into<String>,
        connection: PluginConnection,
        sql: impl Into<String>,
        params: Vec<Value>,
        class: StatementClass,
        reason: WriteReason,
        stage: ApprovalStage,
    ) -> Self {
        Self {
            plugin,
            plugin_name: plugin_name.into(),
            connection,
            sql: sql.into(),
            params,
            class,
            reason,
            stage,
        }
    }
}

/// Asks the user whether a plugin's statement may run (the dialog is the application's, ADR-0036).
pub trait QueryApprover: Send + Sync {
    /// Asks for one stage. Dropping the future means not approved.
    fn approve<'a>(&'a self, request: &'a WriteRequest) -> BoxFuture<'a, Decision>;
}

/// The one value an application puts into [`Services`](crate::Services) to give plugins `query` and `catalog`:
/// `services.insert(Arc::new(PluginData::new(source).with_approver(approver)))`.
#[derive(Clone)]
pub struct PluginData {
    source: Arc<dyn DataService>,
    approver: Option<Arc<dyn QueryApprover>>,
}

impl PluginData {
    /// Plugins read through `source`. With no approver every statement that is not a plain read is refused.
    #[must_use]
    pub fn new(source: Arc<dyn DataService>) -> Self {
        Self {
            source,
            approver: None,
        }
    }

    /// Sets the approver that statements other than plain reads go through.
    #[must_use]
    pub fn with_approver(mut self, approver: Arc<dyn QueryApprover>) -> Self {
        self.approver = Some(approver);
        self
    }

    pub(crate) fn source(&self) -> &Arc<dyn DataService> {
        &self.source
    }

    pub(crate) fn approver(&self) -> Option<&Arc<dyn QueryApprover>> {
        self.approver.as_ref()
    }
}

impl fmt::Debug for PluginData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginData")
            .field("approver", &self.approver.is_some())
            .finish_non_exhaustive()
    }
}
