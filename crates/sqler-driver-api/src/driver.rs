// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::future::Future;
use std::pin::Pin;

use sqler_core::{ConnectionConfig, Query};

use crate::{CancelHandle, DriverError, Introspect, QueryStream};

/// A boxed, sendable future. Keeps the traits dyn-compatible.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A database backend. Registered once, opens any number of sessions.
pub trait Driver: Send + Sync {
    /// The driver id. Must satisfy [`sqler_core::DriverId::new`].
    fn id(&self) -> &'static str;

    /// Opens a session for `config`.
    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>>;
}

/// One open connection to a database.
pub trait Session: Send {
    /// A handle that cancels the running operation of this session.
    ///
    /// Take it before calling [`Session::execute`]; it can be used from another task.
    fn cancel_handle(&self) -> CancelHandle;

    /// Runs one statement and returns its event stream.
    ///
    /// The stream borrows the session, so one statement runs at a time.
    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>>;

    /// Schema introspection, if this driver supports it.
    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        None
    }

    /// Closes the session.
    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>>;
}
