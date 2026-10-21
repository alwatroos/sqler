// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Shared test fixtures: a SQLite-backed session source with spies, a scripted approver and a
//! fake introspection with functions.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use sqler_ai_api::{BoxFuture, ToolCall};
use sqler_ai_tools::{
    ApprovalRequest, Approver, CallReport, ConnectionInfo, Decision, Limits, SessionSource,
    ToolError, ToolSet,
};
use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::{
    ConnectionConfig, DbObject, DriverId, Environment, Listing, ObjectKind, ObjectPath, Query,
    Table,
};
use sqler_driver_api::{
    CancelHandle, Driver, DriverError, Introspect, QueryStream, Session, collect,
};
use sqler_driver_sqlite::SqliteDriver;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A unique database file, removed on drop (also when a test fails).
pub struct TempDb(pub PathBuf);

impl TempDb {
    pub fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let name = format!("sqler-ai-tools-{tag}-{}-{n}.db", std::process::id());
        Self(std::env::temp_dir().join(name))
    }

    pub async fn open(&self) -> Box<dyn Session> {
        let id = DriverId::new("sqlite").unwrap();
        let config = ConnectionConfig::new("t", id, self.0.to_str().unwrap());
        SqliteDriver::new().connect(&config).await.unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Runs a statement on a session and drains it.
pub async fn exec(session: &mut dyn Session, sql: &str) {
    let query = Query::new(sql);
    let mut stream = session.execute(&query).await.unwrap();
    collect(&mut *stream).await.unwrap();
}

/// Wraps a session and records every executed SQL text.
pub struct Spy {
    inner: Box<dyn Session>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Session for Spy {
    fn cancel_handle(&self) -> CancelHandle {
        self.inner.cancel_handle()
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        self.log.lock().unwrap().push(query.sql.clone());
        self.inner.execute(query)
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        self.inner.introspection()
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        self.inner.close()
    }
}

/// Counts the catalog's introspection calls and forwards them to a SQLite session.
struct Counting {
    session: Box<dyn Session>,
    calls: Arc<AtomicUsize>,
}

impl Introspect for Counting {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(
            kind,
            ObjectKind::Schema
                | ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::Index
                | ObjectKind::Trigger
        )
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match self.session.introspection() {
                Some(i) => i.list(parent, kind).await,
                None => Ok(Listing::Unsupported),
            }
        })
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match self.session.introspection() {
                Some(i) => i.table(table).await,
                None => Ok(None),
            }
        })
    }

    fn definition<'a>(
        &'a mut self,
        object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match self.session.introspection() {
                Some(i) => i.definition(object).await,
                None => Ok(None),
            }
        })
    }
}

/// One SQLite connection `db` lent through the tool set, with spies.
pub struct SqliteSource {
    pub db: TempDb,
    pub info: ConnectionInfo,
    pub catalog: Catalog,
    /// How many sessions were acquired.
    pub acquires: AtomicUsize,
    /// How many sessions were released.
    pub releases: AtomicUsize,
    /// How many introspection calls the catalog made.
    pub introspections: Arc<AtomicUsize>,
    /// Every SQL text that reached a lent session.
    pub executed: Arc<Mutex<Vec<String>>>,
}

pub const SEED: &[&str] = &[
    "create table users (id integer primary key, name text not null, email text default 'none')",
    "create table orders (id integer primary key, user_id integer references users(id), total real)",
    "create view user_names as select name from users",
    "insert into users (id, name, email) values (1,'ann','a@x'),(2,'bob','b@x'),(3,'cy','c@x'),(4,'di','d@x'),(5,'ed','e@x')",
];

impl SqliteSource {
    pub async fn new(tag: &str, env: Option<Environment>) -> Arc<Self> {
        let db = TempDb::new(tag);
        let mut seeder = db.open().await;
        for sql in SEED {
            exec(&mut *seeder, sql).await;
        }
        let introspections = Arc::new(AtomicUsize::new(0));
        let counting = Counting {
            session: db.open().await,
            calls: Arc::clone(&introspections),
        };
        let boxed: Box<dyn Introspect> = Box::new(counting);
        let (catalog, worker) = Catalog::new(boxed);
        tokio::spawn(worker.run());
        let info = ConnectionInfo::new("db", "Test DB", DriverId::new("sqlite").unwrap(), env);
        Arc::new(Self {
            db,
            info,
            catalog,
            acquires: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            introspections,
            executed: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn executed(&self) -> Vec<String> {
        self.executed.lock().unwrap().clone()
    }

    pub fn introspection_calls(&self) -> usize {
        self.introspections.load(Ordering::SeqCst)
    }

    pub fn acquire_count(&self) -> usize {
        self.acquires.load(Ordering::SeqCst)
    }
}

impl SessionSource for SqliteSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![self.info.clone()]
    }

    fn catalog(&self, connection: &str) -> Option<Catalog> {
        (connection == self.info.id).then(|| self.catalog.clone())
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        Box::pin(async move {
            if connection != self.info.id {
                return Err(ToolError::UnknownConnection(connection.to_owned()));
            }
            self.acquires.fetch_add(1, Ordering::SeqCst);
            let inner = self.db.open().await;
            Ok(Box::new(Spy {
                inner,
                log: Arc::clone(&self.executed),
            }) as Box<dyn Session>)
        })
    }

    fn release(&self, _connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let _ = session.close().await;
        })
    }
}

/// An approver that records every request and answers with a closure.
pub struct ScriptedApprover {
    pub requests: Mutex<Vec<ApprovalRequest>>,
    decide: Box<dyn Fn(&ApprovalRequest) -> Decision + Send + Sync>,
}

impl ScriptedApprover {
    pub fn new(decide: impl Fn(&ApprovalRequest) -> Decision + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            decide: Box::new(decide),
        })
    }

    pub fn always(decision: Decision) -> Arc<Self> {
        Self::new(move |_| decision)
    }

    pub fn requests(&self) -> Vec<ApprovalRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Approver for ScriptedApprover {
    fn approve<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        self.requests.lock().unwrap().push(request.clone());
        let decision = (self.decide)(request);
        Box::pin(std::future::ready(decision))
    }
}

/// Calls a tool and parses its content.
pub async fn call(ts: &ToolSet, name: &str, args: Value) -> (CallReport, Value) {
    let report = ts
        .call_detailed(&ToolCall::new("id1", name, args.to_string()))
        .await;
    let content = serde_json::from_str(&report.result.content).unwrap();
    (report, content)
}

pub fn limits() -> Limits {
    Limits::default()
}

/// An introspection with two functions and a procedure, counting calls.
#[derive(Clone)]
pub struct FakeIntro {
    pub definitions: Arc<AtomicUsize>,
    pub lists: Arc<AtomicUsize>,
}

impl FakeIntro {
    pub fn new() -> Self {
        Self {
            definitions: Arc::new(AtomicUsize::new(0)),
            lists: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Introspect for FakeIntro {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(kind, ObjectKind::Function | ObjectKind::Procedure)
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        let listing = match kind {
            ObjectKind::Function => Listing::Objects(vec![
                DbObject::new(ObjectKind::Function, parent.clone(), "area"),
                DbObject::new(ObjectKind::Function, parent.clone(), "perimeter"),
            ]),
            ObjectKind::Procedure => Listing::Objects(vec![DbObject::new(
                ObjectKind::Procedure,
                parent.clone(),
                "area",
            )]),
            _ => Listing::Unsupported,
        };
        Box::pin(std::future::ready(Ok(listing)))
    }

    fn table<'a>(
        &'a mut self,
        _table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        Box::pin(std::future::ready(Ok(None)))
    }

    fn definition<'a>(
        &'a mut self,
        object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        self.definitions.fetch_add(1, Ordering::SeqCst);
        let text = format!("create {} {} ...", object.kind.as_str(), object.name);
        Box::pin(std::future::ready(Ok(Some(text))))
    }
}

/// A session that only introspects.
pub struct FakeSession(pub FakeIntro);

impl Session for FakeSession {
    fn cancel_handle(&self) -> CancelHandle {
        sqler_driver_api::CancelFlag::new().handle()
    }

    fn execute<'s>(
        &'s mut self,
        _query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(std::future::ready(Err(DriverError::Unsupported(
            "fake".into(),
        ))))
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        Some(&mut self.0)
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A source over `FakeIntro`, with a postgres-like connection.
pub struct FunctionSource {
    pub intro: FakeIntro,
    pub info: ConnectionInfo,
    pub catalog: Catalog,
    pub acquires: AtomicUsize,
}

impl FunctionSource {
    pub fn new() -> Arc<Self> {
        let intro = FakeIntro::new();
        let boxed: Box<dyn Introspect> = Box::new(intro.clone());
        let (catalog, worker) = Catalog::new(boxed);
        tokio::spawn(worker.run());
        Arc::new(Self {
            intro,
            info: ConnectionInfo::new("pg", "PG", DriverId::new("postgres").unwrap(), None),
            catalog,
            acquires: AtomicUsize::new(0),
        })
    }
}

impl SessionSource for FunctionSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![self.info.clone()]
    }

    fn catalog(&self, _connection: &str) -> Option<Catalog> {
        Some(self.catalog.clone())
    }

    fn acquire<'a>(
        &'a self,
        _connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        self.acquires.fetch_add(1, Ordering::SeqCst);
        let session = FakeSession(self.intro.clone());
        Box::pin(std::future::ready(
            Ok(Box::new(session) as Box<dyn Session>),
        ))
    }
}
