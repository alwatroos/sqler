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
    BoxFuture, CancelHandle, Driver, DriverError, Introspect, QueryEvent, QueryStream, Session,
};

use super::source::Gate;
use crate::query::query_text;

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
            let secrets: Arc<[String]> =
                super::error::secrets(&super::params::ConnectParams::parse(&config.target)).into();
            let mut inner = self
                .engine
                .connect(config)
                .await
                .map_err(|e| scrub(e, &secrets))?;
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
                secrets,
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
    /// The forms of the connect password; an engine message carrying one is denied.
    secrets: Arc<[String]>,
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
            let secrets = Arc::clone(&self.secrets);
            let stream = self
                .inner
                .execute(query)
                .await
                .map_err(|e| scrub(e, &secrets))?;
            Ok(Box::new(ScrubStream(stream, secrets)) as Box<dyn QueryStream + 's>)
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
        let secrets = Arc::clone(&self.secrets);
        let close = self.inner.close();
        Box::pin(async move { close.await.map_err(|e| scrub(e, &secrets)) })
    }
}

/// The native engine's error text as the UI may see it: connection and query text pass the same scrub as the text
/// of a guest driver (a login failure or a denial becomes the fixed text, accounts, hosts and paths are replaced).
/// A text that carries the connect password (any form in `secrets`) becomes the denial text.
fn scrub(e: DriverError, secrets: &[String]) -> DriverError {
    let text = |t: &str| {
        if secrets.iter().any(|s| t.contains(s.as_str())) {
            crate::query::DENIED.to_owned()
        } else {
            query_text(t)
        }
    };
    match e {
        DriverError::Query(t) => DriverError::Query(text(&t)),
        DriverError::Connection(t) => DriverError::Connection(text(&t)),
        other => other,
    }
}

/// A result stream whose errors pass [`scrub`].
struct ScrubStream<'s>(Box<dyn QueryStream + 's>, Arc<[String]>);

impl QueryStream for ScrubStream<'_> {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(async move {
            let event = self.0.next_event().await;
            event.map(|r| r.map_err(|e| scrub(e, &self.1)))
        })
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
                Some(i) => i
                    .list(parent, kind)
                    .await
                    .map_err(|e| scrub(e, &self.secrets)),
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
                Some(i) => i.table(table).await.map_err(|e| scrub(e, &self.secrets)),
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
                Some(i) => i
                    .definition(object)
                    .await
                    .map_err(|e| scrub(e, &self.secrets)),
                None => Ok(None),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_errors_are_scrubbed() {
        assert!(matches!(
            scrub(
                DriverError::Connection("Login failed for user 'sa'. (error 18456)".to_owned()),
                &[]
            ),
            DriverError::Connection(t) if t == crate::query::DENIED
        ));
        assert!(matches!(
            scrub(
                DriverError::Query(
                    "The SELECT permission was denied on the object 'orders', database 'sales', schema 'dbo'. (error 229)"
                        .to_owned()
                ),
                &[]
            ),
            DriverError::Query(t) if t == crate::query::DENIED
        ));
        assert!(matches!(
            scrub(
                DriverError::Query(
                    "The server principal \"app\" is not able to access the database \"sales\" under the current security context."
                        .to_owned()
                ),
                &[]
            ),
            DriverError::Query(t) if !t.contains("\"app\"")
        ));
        assert!(matches!(
            scrub(DriverError::Query("Invalid object name 't'. (error 208)".to_owned()), &[]),
            DriverError::Query(t) if t.contains("Invalid object name")
        ));
        assert!(matches!(
            scrub(DriverError::Cancelled, &[]),
            DriverError::Cancelled
        ));
    }

    #[test]
    fn the_connect_password_is_denied_in_any_form() {
        let secrets = super::super::error::secrets(&super::super::params::ConnectParams::parse(
            "sqlserver://sa:p%40ss%2Fw%3Ard@h/db",
        ));
        for text in ["login p@ss/w:rd", "x p%40ss%2Fw%3Ard", "x p%40ss%2fw%3ard"] {
            assert!(matches!(
                scrub(DriverError::Connection(text.to_owned()), &secrets),
                DriverError::Connection(t) if t == crate::query::DENIED
            ));
            assert!(matches!(
                scrub(DriverError::Query(text.to_owned()), &secrets),
                DriverError::Query(t) if t == crate::query::DENIED
            ));
        }
        assert!(matches!(
            scrub(DriverError::Query("Invalid object name 't'".to_owned()), &secrets),
            DriverError::Query(t) if t.contains("Invalid object name")
        ));
    }
}
