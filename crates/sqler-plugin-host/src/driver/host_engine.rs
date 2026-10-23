// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A host-engine driver (`kind = "host"`): a declarative shell around a native engine the binary links. The host owns
//! connect, TLS and sockets, and the plugin's guest, if it has one, is not involved.
//!
//! Availability is checked at connect and again before each statement and each catalog call, so a revoke or a disable
//! stops the session at its next statement. A statement already running completes.

use std::sync::Arc;

use sqler_driver_api::sqler_core::{
    ConnectionConfig, DbObject, Listing, ObjectKind, ObjectPath, Query, Table,
};
use sqler_driver_api::{
    BoxFuture, CancelHandle, Driver, DriverError, Introspect, QueryStream, Session,
};

use super::source::Gate;

/// The driver the registry gets for a host-engine plugin driver.
pub(super) struct HostEngineDriver {
    gate: Gate,
    engine: Arc<dyn Driver>,
}

impl HostEngineDriver {
    pub(super) fn new(gate: Gate, engine: Arc<dyn Driver>) -> Self {
        Self { gate, engine }
    }
}

impl Driver for HostEngineDriver {
    fn id(&self) -> &str {
        &self.gate.driver
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async move {
            self.gate.check()?;
            let mut inner = self.engine.connect(config).await?;
            let supported = inner.introspection().map(|i| {
                ObjectKind::ALL
                    .into_iter()
                    .filter(|k| i.supports(*k))
                    .collect::<Vec<_>>()
            });
            Ok(Box::new(GatedSession {
                gate: self.gate.clone(),
                inner,
                supported,
            }) as Box<dyn Session>)
        })
    }
}

/// A native session whose every statement first asks the gate. It is its own [`Introspect`], so the catalog calls
/// pass the gate too; the supported kinds are read once at connect because `supports` cannot reach the inner session.
struct GatedSession {
    gate: Gate,
    inner: Box<dyn Session>,
    supported: Option<Vec<ObjectKind>>,
}

impl Session for GatedSession {
    fn cancel_handle(&self) -> CancelHandle {
        self.inner.cancel_handle()
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(async move {
            self.gate.check()?;
            self.inner.execute(query).await
        })
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        if self.supported.is_some() {
            Some(self)
        } else {
            None
        }
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        self.inner.close()
    }
}

impl Introspect for GatedSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        self.supported
            .as_ref()
            .is_some_and(|kinds| kinds.contains(&kind))
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        Box::pin(async move {
            self.gate.check()?;
            match self.inner.introspection() {
                Some(i) => i.list(parent, kind).await,
                None => Ok(Listing::Unsupported),
            }
        })
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        Box::pin(async move {
            self.gate.check()?;
            match self.inner.introspection() {
                Some(i) => i.table(table).await,
                None => Ok(None),
            }
        })
    }

    fn definition<'a>(
        &'a mut self,
        object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(async move {
            self.gate.check()?;
            match self.inner.introspection() {
                Some(i) => i.definition(object).await,
                None => Ok(None),
            }
        })
    }
}
