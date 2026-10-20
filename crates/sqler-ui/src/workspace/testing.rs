// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Fake driver and session shared by the workspace unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqler_driver_api::sqler_core::Query;
use sqler_driver_api::{BoxFuture, CancelFlag, CancelHandle, DriverError, QueryStream, Session};

/// A session whose `close` sets a flag.
pub(crate) struct FakeSession {
    pub(crate) closed: Arc<AtomicBool>,
}

impl FakeSession {
    pub(crate) fn boxed() -> (Box<dyn Session>, Arc<AtomicBool>) {
        let closed = Arc::new(AtomicBool::new(false));
        (
            Box::new(Self {
                closed: closed.clone(),
            }),
            closed,
        )
    }
}

impl Session for FakeSession {
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
        self.closed.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}
