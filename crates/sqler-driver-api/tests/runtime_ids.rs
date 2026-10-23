// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Drivers whose id is owned at run time (plugins) register, connect and show up as sources.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query};
use sqler_driver_api::{
    BoxFuture, CancelFlag, CancelHandle, Driver, DriverError, DriverRegistry, DriverSource,
    QueryStream, Session, SourceLookup,
};

struct NullSession;
impl Session for NullSession {
    fn cancel_handle(&self) -> CancelHandle {
        CancelFlag::new().handle()
    }
    fn execute<'s>(
        &'s mut self,
        _query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(async { Err(DriverError::Unsupported("test".into())) })
    }
    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(async { Ok(()) })
    }
}

/// A driver with an id that is not `'static`.
struct Dyn(String);
impl Driver for Dyn {
    fn id(&self) -> &str {
        &self.0
    }
    fn connect<'a>(
        &'a self,
        _config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async { Ok(Box::new(NullSession) as Box<dyn Session>) })
    }
}

struct One(Arc<Dyn>);
impl DriverSource for One {
    fn ids(&self) -> Vec<DriverId> {
        vec![DriverId::new(self.0.id()).unwrap()]
    }
    fn lookup(&self, id: &DriverId) -> SourceLookup {
        if id.as_str() == self.0.id() {
            SourceLookup::Available(self.0.clone())
        } else {
            SourceLookup::Unknown
        }
    }
}

fn cfg(driver: &str) -> ConnectionConfig {
    ConnectionConfig::new("t", DriverId::new(driver).unwrap(), "x")
}

/// Polls a future that never really suspends.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("future suspended"),
    }
}

#[test]
fn runtime_id_driver_registers_and_connects() {
    let id = format!("dyn-{}", 7);
    let mut r = DriverRegistry::new();
    r.register(Box::new(Dyn(id.clone()))).unwrap();
    assert_eq!(r.get(&DriverId::new(&id).unwrap()).unwrap().id(), id);
    assert!(block_on(r.connect(&cfg(&id))).is_ok());
}

#[test]
fn runtime_id_driver_through_a_source() {
    let mut r = DriverRegistry::new();
    r.add_source(Arc::new(One(Arc::new(Dyn(format!("plug-{}", 1))))));
    assert_eq!(r.available_ids(), [DriverId::new("plug-1").unwrap()]);
    assert_eq!(r.ids().count(), 0);
    assert!(block_on(r.connect(&cfg("plug-1"))).is_ok());
    let err = block_on(r.connect(&cfg("other"))).err().unwrap();
    assert!(matches!(err, DriverError::UnknownDriver(_)));
}
