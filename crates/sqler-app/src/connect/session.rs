// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A driver session that owns the SSH tunnel it runs through, so the tunnel closes with it.

use sqler_driver_api::sqler_core::Query;
use sqler_driver_api::{BoxFuture, CancelHandle, DriverError, Introspect, QueryStream, Session};
use tokio::runtime::Handle;

use super::tunnel::TunnelHandle;

/// Owns the tunnel. A `Tunnel` must not be dropped outside a tokio runtime, so dropping a guard
/// that still holds one hands its close to the runtime.
pub(crate) struct TunnelGuard {
    tunnel: Option<Box<dyn TunnelHandle>>,
    handle: Handle,
}

impl TunnelGuard {
    pub(crate) fn new(tunnel: Box<dyn TunnelHandle>, handle: Handle) -> Self {
        Self {
            tunnel: Some(tunnel),
            handle,
        }
    }

    /// Closes the tunnel now; a failure is reported as a text.
    pub(crate) async fn close(&mut self) -> Result<(), String> {
        match self.tunnel.take() {
            Some(tunnel) => tunnel.close().await.map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }
}

impl Drop for TunnelGuard {
    fn drop(&mut self) {
        if let Some(tunnel) = self.tunnel.take() {
            // Detached on purpose: nobody can wait for a drop, and the result has no reader.
            drop(self.handle.spawn(async move {
                let _ = tunnel.close().await;
            }));
        }
    }
}

/// A session whose lifetime bounds the tunnel's.
pub(crate) struct TunnelledSession {
    inner: Box<dyn Session>,
    guard: TunnelGuard,
}

impl TunnelledSession {
    pub(crate) fn new(inner: Box<dyn Session>, guard: TunnelGuard) -> Self {
        Self { inner, guard }
    }
}

impl Session for TunnelledSession {
    fn cancel_handle(&self) -> CancelHandle {
        self.inner.cancel_handle()
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        self.inner.execute(query)
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        self.inner.introspection()
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        let Self { inner, mut guard } = *self;
        Box::pin(async move {
            let inner_result = inner.close().await;
            let tunnel_result = guard.close().await;
            // The session's own error wins; a tunnel that will not close is still reported.
            inner_result?;
            tunnel_result
                .map_err(|e| DriverError::Connection(format!("closing the SSH tunnel: {e}")))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_driver_api::CancelFlag;

    use super::*;
    use crate::connect::tunnel::fake;

    struct Inner {
        closed: Arc<AtomicUsize>,
        fail: bool,
    }

    impl Session for Inner {
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
            self.closed.fetch_add(1, Ordering::SeqCst);
            let fail = self.fail;
            Box::pin(async move {
                if fail {
                    Err(DriverError::Connection("inner".into()))
                } else {
                    Ok(())
                }
            })
        }
    }

    fn session(fail: bool) -> (Box<dyn Session>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let inner_closed = Arc::new(AtomicUsize::new(0));
        let tunnel_closed = Arc::new(AtomicUsize::new(0));
        let guard = TunnelGuard::new(fake::handle(1, &tunnel_closed), Handle::current());
        let inner = Box::new(Inner {
            closed: Arc::clone(&inner_closed),
            fail,
        });
        (
            Box::new(TunnelledSession::new(inner, guard)),
            inner_closed,
            tunnel_closed,
        )
    }

    #[tokio::test]
    async fn close_closes_session_then_tunnel() {
        let (s, inner, tunnel) = session(false);
        s.close().await.unwrap();
        assert_eq!(
            (inner.load(Ordering::SeqCst), tunnel.load(Ordering::SeqCst)),
            (1, 1)
        );
    }

    #[tokio::test]
    async fn inner_error_wins_and_tunnel_still_closes() {
        let (s, _, tunnel) = session(true);
        let err = s.close().await.unwrap_err();
        assert_eq!(err.to_string(), "connection failed: inner");
        assert_eq!(tunnel.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn drop_closes_the_tunnel_on_the_runtime() {
        let (s, _, tunnel) = session(false);
        drop(s);
        for _ in 0..50 {
            if tunnel.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(tunnel.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn forwards_cancel_execute_and_introspection() {
        let (mut s, _, _) = session(false);
        let _ = s.cancel_handle();
        assert!(s.introspection().is_none());
        let q = Query::new("select 1");
        assert!(s.execute(&q).await.is_err());
    }
}
