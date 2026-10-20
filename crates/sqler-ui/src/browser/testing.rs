// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Fakes for the browser tests: scripted introspection, a scripted session and a connector.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use sqler_config::ConnectionSpec;
use sqler_driver_api::sqler_core::{
    Column, DataType, DbObject, DriverId, Listing, ObjectKind, ObjectPath, Query, Row, Table, Value,
};
use sqler_driver_api::{
    BoxFuture, CancelFlag, CancelHandle, DriverError, Introspect, QueryEvent, QueryStream, Session,
};
use tokio::sync::Semaphore;

use super::connect::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt, SecretSource};

#[derive(Default)]
struct Script {
    listings: HashMap<(ObjectPath, ObjectKind), Result<Listing, String>>,
    tables: HashMap<DbObject, Table>,
    definitions: HashMap<DbObject, String>,
    definition_calls: Vec<DbObject>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Scripted introspection with a call counter and an optional gate.
///
/// Cloning shares the script and the counter. A listing that was not scripted is
/// `Listing::Unsupported`.
#[derive(Clone, Default)]
pub(crate) struct FakeIntrospect {
    script: Arc<Mutex<Script>>,
    calls: Arc<AtomicUsize>,
    gate: Option<Arc<Semaphore>>,
}

impl FakeIntrospect {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Every `list` call waits for a permit of `gate` first.
    pub(crate) fn with_gate(mut self, gate: Arc<Semaphore>) -> Self {
        self.gate = Some(gate);
        self
    }

    pub(crate) fn with_objects(
        self,
        parent: &ObjectPath,
        kind: ObjectKind,
        objects: Vec<DbObject>,
    ) -> Self {
        lock(&self.script)
            .listings
            .insert((parent.clone(), kind), Ok(Listing::Objects(objects)));
        self
    }

    pub(crate) fn with_list_error(
        self,
        parent: &ObjectPath,
        kind: ObjectKind,
        message: &str,
    ) -> Self {
        lock(&self.script)
            .listings
            .insert((parent.clone(), kind), Err(message.to_owned()));
        self
    }

    pub(crate) fn with_table(self, table: Table) -> Self {
        lock(&self.script)
            .tables
            .insert(table.object.clone(), table);
        self
    }

    pub(crate) fn with_definition(self, object: &DbObject, text: &str) -> Self {
        lock(&self.script)
            .definitions
            .insert(object.clone(), text.to_owned());
        self
    }

    /// How many `list`, `table` and `definition` calls were made.
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The objects `definition` was called for, in order.
    pub(crate) fn definition_calls(&self) -> Vec<DbObject> {
        lock(&self.script).definition_calls.clone()
    }

    pub(crate) fn boxed(&self) -> Box<dyn Introspect> {
        Box::new(self.clone())
    }
}

impl Introspect for FakeIntrospect {
    fn supports(&self, kind: ObjectKind) -> bool {
        lock(&self.script).listings.keys().any(|(_, k)| *k == kind)
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = self.gate.clone()
                && let Ok(permit) = gate.acquire().await
            {
                permit.forget();
            }
            match lock(&self.script).listings.get(&(parent.clone(), kind)) {
                Some(Ok(listing)) => Ok(listing.clone()),
                Some(Err(message)) => Err(DriverError::Query(message.clone())),
                None => Ok(Listing::Unsupported),
            }
        })
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let found = lock(&self.script).tables.get(table).cloned();
        Box::pin(std::future::ready(Ok(found)))
    }

    fn definition<'a>(
        &'a mut self,
        object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut script = lock(&self.script);
        script.definition_calls.push(object.clone());
        let found = script.definitions.get(object).cloned();
        Box::pin(std::future::ready(Ok(found)))
    }
}

struct VecStream(VecDeque<Result<QueryEvent, DriverError>>);

impl QueryStream for VecStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(std::future::ready(self.0.pop_front()))
    }
}

/// A session that records the SQL it runs, answers with a fixed result and counts its close.
pub(crate) struct ScriptedSession {
    intro: FakeIntrospect,
    sql: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicUsize>,
    rows: Vec<Row>,
    fail_execute: Option<String>,
}

impl Session for ScriptedSession {
    fn cancel_handle(&self) -> CancelHandle {
        CancelFlag::new().handle()
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        lock(&self.sql).push(query.sql.clone());
        let result = if let Some(message) = &self.fail_execute {
            Err(DriverError::Query(message.clone()))
        } else {
            let mut events = VecDeque::new();
            events.push_back(Ok(QueryEvent::Columns(vec![Column::new(
                "a",
                DataType::Integer,
            )])));
            if !self.rows.is_empty() {
                events.push_back(Ok(QueryEvent::Rows(self.rows.clone())));
            }
            events.push_back(Ok(QueryEvent::Done {
                rows_affected: None,
            }));
            Ok(Box::new(VecStream(events)) as Box<dyn QueryStream + 's>)
        };
        Box::pin(std::future::ready(result))
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        Some(&mut self.intro)
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A [`Connector`] that hands out [`ScriptedSession`]s sharing one script and one SQL log.
#[derive(Clone)]
pub(crate) struct SessionConnector {
    pub(crate) intro: FakeIntrospect,
    sql: Arc<Mutex<Vec<String>>>,
    connects: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    rows: Vec<Row>,
    fail_connect: Option<String>,
    fail_execute: Option<String>,
    cancel_on_connect: Option<CancelFlag>,
}

impl SessionConnector {
    pub(crate) fn new(intro: FakeIntrospect) -> Self {
        Self {
            intro,
            sql: Arc::default(),
            connects: Arc::default(),
            closed: Arc::default(),
            rows: Vec::new(),
            fail_connect: None,
            fail_execute: None,
            cancel_on_connect: None,
        }
    }

    pub(crate) fn with_rows(mut self, count: i64) -> Self {
        self.rows = (0..count).map(|i| Row(vec![Value::Int(i)])).collect();
        self
    }

    pub(crate) fn failing_connect(mut self, message: &str) -> Self {
        self.fail_connect = Some(message.to_owned());
        self
    }

    pub(crate) fn failing_execute(mut self, message: &str) -> Self {
        self.fail_execute = Some(message.to_owned());
        self
    }

    /// Cancels `flag` while connecting, to test a cancel that arrives mid-job.
    pub(crate) fn cancelling_on_connect(mut self, flag: CancelFlag) -> Self {
        self.cancel_on_connect = Some(flag);
        self
    }

    pub(crate) fn connects(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }

    pub(crate) fn closed(&self) -> usize {
        self.closed.load(Ordering::SeqCst)
    }

    /// The SQL every session ran, in order.
    pub(crate) fn sql(&self) -> Vec<String> {
        lock(&self.sql).clone()
    }
}

impl Connector for SessionConnector {
    fn driver_ids(&self) -> Vec<DriverId> {
        Vec::new()
    }

    fn connect(
        &self,
        _request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        if let Some(flag) = &self.cancel_on_connect {
            flag.cancel();
        }
        let result = match &self.fail_connect {
            Some(message) => Err(ConnectFailure::Tunnel(message.clone())),
            None => Ok(Box::new(ScriptedSession {
                intro: self.intro.clone(),
                sql: Arc::clone(&self.sql),
                closed: Arc::clone(&self.closed),
                rows: self.rows.clone(),
                fail_execute: self.fail_execute.clone(),
            }) as Box<dyn Session>),
        };
        Box::pin(std::future::ready(result))
    }

    fn trust_host_key(
        &self,
        _key: HostKeyPrompt,
    ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A request for the fake connector (it ignores the contents).
pub(crate) fn request() -> ConnectRequest {
    ConnectRequest {
        spec: ConnectionSpec::new(
            "fake",
            DriverId::new("sqlite").unwrap_or_else(|_| unreachable!("valid id")),
            ":memory:",
        ),
        db_password: SecretSource::None,
        ssh_secret: SecretSource::None,
    }
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll, Waker};

    use super::*;

    /// Lists once; the fake never suspends unless gated.
    fn list_now(
        intro: &mut FakeIntrospect,
        path: &ObjectPath,
        kind: ObjectKind,
    ) -> Result<Listing, DriverError> {
        let mut fut = intro.list(path, kind);
        match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("unexpectedly pending"),
        }
    }

    #[test]
    fn listings_errors_and_calls() {
        let path = ObjectPath::root();
        let obj = DbObject::new(ObjectKind::Schema, path.clone(), "main");
        let mut intro = FakeIntrospect::new()
            .with_objects(&path, ObjectKind::Schema, vec![obj.clone()])
            .with_list_error(&path, ObjectKind::Database, "boom");
        assert!(intro.supports(ObjectKind::Schema));
        assert!(!intro.supports(ObjectKind::Table));
        assert_eq!(
            list_now(&mut intro, &path, ObjectKind::Schema).unwrap(),
            Listing::Objects(vec![obj])
        );
        assert!(matches!(
            list_now(&mut intro, &path, ObjectKind::Database),
            Err(DriverError::Query(m)) if m == "boom"
        ));
        assert_eq!(
            list_now(&mut intro, &path, ObjectKind::Role).unwrap(),
            Listing::Unsupported
        );
        assert_eq!(intro.calls(), 3);
    }

    #[test]
    fn the_gate_holds_a_list_until_a_permit_arrives() {
        let gate = Arc::new(Semaphore::new(0));
        let mut intro = FakeIntrospect::new().with_gate(Arc::clone(&gate));
        let path = ObjectPath::root();
        let mut fut = intro.list(&path, ObjectKind::Schema);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        gate.add_permits(1);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(_))));
        assert_eq!(gate.available_permits(), 0);
    }
}
