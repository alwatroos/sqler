// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end check of the driver API through `dyn` objects only.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::task::{Context, Poll, Waker};

use sqler_driver_api::sqler_core::{
    Column, ConnectionConfig, DataType, DriverId, Query, Row, Value,
};
use sqler_driver_api::{
    BoxFuture, CancelFlag, CancelHandle, Driver, DriverError, DriverRegistry, QueryEvent,
    QueryStream, Session, collect,
};

fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

struct MockDriver;

impl Driver for MockDriver {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn connect<'a>(
        &'a self,
        _config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async {
            Ok(Box::new(MockSession {
                flag: CancelFlag::new(),
            }) as Box<dyn Session>)
        })
    }
}

struct MockSession {
    flag: CancelFlag,
}

impl Session for MockSession {
    fn cancel_handle(&self) -> CancelHandle {
        self.flag.handle()
    }

    fn execute<'s>(
        &'s mut self,
        _query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        let stream = MockStream {
            flag: self.flag.clone(),
            step: 0,
            finished: false,
        };
        Box::pin(async move { Ok(Box::new(stream) as Box<dyn QueryStream + 's>) })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Columns, then three one-row batches, then Done. Checks the flag before each batch.
struct MockStream {
    flag: CancelFlag,
    step: usize,
    finished: bool,
}

impl QueryStream for MockStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        let event = if self.finished {
            None
        } else if self.step > 0 && self.flag.is_cancelled() {
            self.finished = true;
            Some(Err(DriverError::Cancelled))
        } else {
            let step = self.step;
            self.step += 1;
            Some(Ok(match step {
                0 => QueryEvent::Columns(vec![Column::new("n", DataType::Integer)]),
                1..=3 => {
                    let n = i64::try_from(step).unwrap_or_default();
                    QueryEvent::Rows(vec![Row(vec![Value::Int(n)])])
                }
                _ => {
                    self.finished = true;
                    QueryEvent::Done {
                        rows_affected: None,
                    }
                }
            }))
        };
        Box::pin(std::future::ready(event))
    }
}

fn registry() -> DriverRegistry {
    let mut r = DriverRegistry::new();
    r.register(Box::new(MockDriver)).unwrap();
    r
}

fn config() -> ConnectionConfig {
    ConnectionConfig::new("t", DriverId::new("mock").unwrap(), "mem")
}

#[test]
fn execute_and_collect() {
    let r = registry();
    let mut session = block_on(r.connect(&config())).unwrap();
    let q = Query::new("select n");
    let rs = block_on(async {
        let mut stream = session.execute(&q).await?;
        collect(&mut *stream).await
    })
    .unwrap();
    assert_eq!(rs.columns()[0].name, "n");
    assert_eq!(rs.len(), 3);
    assert_eq!(rs.rows()[2].get(0), Some(&Value::Int(3)));
    block_on(session.close()).unwrap();
}

#[test]
fn cancel_mid_stream() {
    let r = registry();
    let mut session = block_on(r.connect(&config())).unwrap();
    let handle = session.cancel_handle();
    let q = Query::new("select n");
    block_on(async {
        let mut stream = session.execute(&q).await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Columns(_)))
        ));
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Rows(_)))
        ));
        handle.cancel().await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Err(DriverError::Cancelled))
        ));
        assert!(stream.next_event().await.is_none());
        assert!(stream.next_event().await.is_none());
    });
}

#[test]
fn introspection_defaults_to_none() {
    let r = registry();
    let mut session = block_on(r.connect(&config())).unwrap();
    assert!(session.introspection().is_none());
}

#[test]
fn unknown_driver() {
    let r = registry();
    let cfg = ConnectionConfig::new("t", DriverId::new("other").unwrap(), "x");
    let err = block_on(r.connect(&cfg)).err().unwrap();
    assert!(matches!(err, DriverError::UnknownDriver(_)));
}
