// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

// The generated host trait is async; some implementations here have nothing to await in the wrapper itself.
#![allow(clippy::unused_async_trait_impl)]

//! The guarded `query` interface and the host traits behind it (ADR-0058).
//!
//! The application implements [`DataService`] (which connections plugins see, and a session to run one statement on)
//! and [`QueryApprover`] (the user's confirmation), and inserts one [`PluginData`] into
//! [`Services`](crate::Services). `query.run` then checks, in this order: some db capability, the data service, the
//! arguments, the connection, exactly one statement (classified by `sqler_sql::classify_each`, fail-safe), the
//! capability the class needs, the approvals, and finally runs under row, byte and time caps. A plain read needs
//! `db.read` and nothing else. Anything else, including `EXPLAIN ANALYZE`, needs `db.write`, a connection that is not
//! read-only, an approver and an approval of the exact SQL and parameters; `prod` asks twice. The guest only ever
//! receives values, never a session.

mod convert;
mod exec;
mod guard;
mod scrub;
mod service;

#[cfg(test)]
mod props;
#[cfg(test)]
pub(crate) mod support;
#[cfg(test)]
mod tests;

pub(crate) use convert::{
    clip, environment_to_wit, kind_to_wit, path_from_wit, path_to_wit, type_name,
};
pub(crate) use exec::NOT_AVAILABLE;
pub use service::{
    ApprovalStage, DataError, DataService, Decision, MAX_CATALOG_ITEMS, MAX_PARAMS,
    MAX_RESULT_BYTES, MAX_SQL_BYTES, PluginConnection, PluginData, QueryApprover, WriteReason,
    WriteRequest,
};

use sqler_plugin_api::{Limits, QueryLimits};

use crate::runtime::HostState;
use crate::runtime::wit::query as wit;
use crate::runtime::wit::types::PluginError;
use exec::{QueryCtx, run_guarded};

impl wit::Host for HostState {
    async fn run(&mut self, request: wit::QueryRequest) -> Result<wit::QueryResult, PluginError> {
        let ctx = QueryCtx {
            plugin: &self.plugin,
            effective: &self.effective,
            data: self.services.get::<PluginData>(),
            limits: self
                .services
                .get::<QueryLimits>()
                .map_or(Limits::DEFAULT.query, |l| *l),
        };
        run_guarded(&ctx, request).await
    }
}
