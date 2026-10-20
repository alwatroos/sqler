// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end check of the driver API through `dyn` objects only.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::task::{Context, Poll, Waker};

use sqler_driver_api::sqler_core::{
    Column, ColumnInfo, ConnectionConfig, DataType, Date, DbObject, Decimal, DriverId, Listing,
    ObjectKind, ObjectPath, Query, Row, Table, Time, Timestamp, TimestampTz, Value,
};
use sqler_driver_api::{
    BoxFuture, CancelFlag, CancelHandle, Driver, DriverError, DriverRegistry, Introspect,
    QueryEvent, QueryStream, Session, collect,
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

/// Supports only tables and returns one fixed table.
impl Introspect for MockSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        kind == ObjectKind::Table
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        let listing = if self.supports(kind) {
            Listing::Objects(vec![DbObject::new(kind, parent.clone(), "t")])
        } else {
            Listing::Unsupported
        };
        Box::pin(async move { Ok(listing) })
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        let found = (table.kind == ObjectKind::Table && table.name == "t").then(|| {
            Table::new(table.clone()).with_columns(vec![
                ColumnInfo::new("n", DataType::Integer).with_nullable(false),
            ])
        });
        Box::pin(async move { Ok(found) })
    }

    fn definition<'a>(
        &'a mut self,
        _object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(async { Ok(None) })
    }
}

/// A session that does not override `introspection`.
struct PlainSession(MockSession);

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

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        Some(self)
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(async { Ok(()) })
    }
}

impl Session for PlainSession {
    fn cancel_handle(&self) -> CancelHandle {
        self.0.cancel_handle()
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        self.0.execute(query)
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
    let mut session = PlainSession(MockSession {
        flag: CancelFlag::new(),
    });
    assert!(session.introspection().is_none());
}

#[test]
fn introspection_through_dyn() {
    let r = registry();
    let mut session = block_on(r.connect(&config())).unwrap();
    let intro = session.introspection().unwrap();
    assert!(intro.supports(ObjectKind::Table));
    assert!(!intro.supports(ObjectKind::Role));
    let root = ObjectPath::root();
    assert_eq!(
        block_on(intro.list(&root, ObjectKind::Role)).unwrap(),
        Listing::Unsupported
    );
    let Listing::Objects(tables) = block_on(intro.list(&root, ObjectKind::Table)).unwrap() else {
        panic!("tables are supported");
    };
    assert_eq!(tables.len(), 1);
    let t = block_on(intro.table(&tables[0])).unwrap().unwrap();
    assert!(!t.column("n").unwrap().nullable);
    let missing = DbObject::new(ObjectKind::Table, root.clone(), "nope");
    assert!(block_on(intro.table(&missing)).unwrap().is_none());
    let function = DbObject::new(ObjectKind::Function, root, "f");
    assert!(block_on(intro.definition(&function)).unwrap().is_none());
}

/// Replays a fixed list of events.
struct Fixed(Vec<QueryEvent>);
impl QueryStream for Fixed {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        let next = if self.0.is_empty() {
            None
        } else {
            Some(Ok(self.0.remove(0)))
        };
        Box::pin(std::future::ready(next))
    }
}

#[test]
fn every_value_variant_survives_collect() {
    let date = Date::new(2024, 2, 29).unwrap();
    let time = Time::new(1, 2, 3, 4).unwrap();
    let ts = Timestamp::new(date, time);
    let row = vec![
        Value::Null,
        Value::Bool(true),
        Value::Int(1),
        Value::Float(1.5),
        Value::Text("t".into()),
        Value::Bytes(vec![1]),
        Value::Decimal("1.50".parse::<Decimal>().unwrap()),
        Value::Date(date),
        Value::Time(time),
        Value::Timestamp(ts),
        Value::TimestampTz(TimestampTz::new(ts, -3600).unwrap()),
        Value::Json("{\"a\": 1}".into()),
    ];
    let columns: Vec<Column> = (0..row.len())
        .map(|i| Column::new(format!("c{i}"), DataType::Other))
        .collect();

    let mut stream = Fixed(vec![
        QueryEvent::Columns(columns),
        QueryEvent::Rows(vec![Row(row.clone())]),
        QueryEvent::Done {
            rows_affected: None,
        },
    ]);
    let rs = block_on(collect(&mut stream)).unwrap();
    assert_eq!(rs.rows()[0].0, row);
}

#[test]
fn unknown_driver() {
    let r = registry();
    let cfg = ConnectionConfig::new("t", DriverId::new("other").unwrap(), "x");
    let err = block_on(r.connect(&cfg)).err().unwrap();
    assert!(matches!(err, DriverError::UnknownDriver(_)));
}
