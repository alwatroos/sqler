// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Shared test fixtures: a SQLite-backed session source and a ready server.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqler_ai_api::BoxFuture;
use sqler_ai_tools::{
    ConnectionInfo, Limits, MemoryAudit, SessionSource, ToolError, ToolSet, UiHost,
};
use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::{
    ConnectionConfig, DbObject, DriverId, Listing, ObjectKind, ObjectPath, Query, Table,
};
use sqler_driver_api::{Driver, DriverError, Introspect, Session, collect};
use sqler_driver_sqlite::SqliteDriver;
use sqler_mcp::{McpServer, ServerOptions};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A unique database file, removed on drop.
pub struct TempDb(pub PathBuf);

impl TempDb {
    pub fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let name = format!("sqler-mcp-{tag}-{}-{n}.db", std::process::id());
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

const SEED: &[&str] = &[
    "create table users (id integer primary key, name text not null)",
    "insert into users (id, name) values (1,'ann'),(2,'bob'),(3,'cy')",
];

/// One SQLite connection `db`, counting its acquires and releases.
pub struct SqliteSource {
    pub db: TempDb,
    pub info: ConnectionInfo,
    pub catalog: Catalog,
    pub acquires: AtomicUsize,
    pub releases: AtomicUsize,
}

impl SqliteSource {
    pub async fn new(tag: &str) -> Arc<Self> {
        let db = TempDb::new(tag);
        let mut seeder = db.open().await;
        for sql in SEED {
            let query = Query::new(*sql);
            let mut stream = seeder.execute(&query).await.unwrap();
            collect(&mut *stream).await.unwrap();
        }
        let boxed: Box<dyn Introspect> = Box::new(Intro(db.open().await));
        let (catalog, worker) = Catalog::new(boxed);
        tokio::spawn(worker.run());
        let info = ConnectionInfo::new("db", "Test DB", DriverId::new("sqlite").unwrap(), None);
        Arc::new(Self {
            db,
            info,
            catalog,
            acquires: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
        })
    }
}

/// Forwards introspection to a SQLite session.
struct Intro(Box<dyn Session>);

impl Introspect for Intro {
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
        Box::pin(async move {
            match self.0.introspection() {
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
            match self.0.introspection() {
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
            match self.0.introspection() {
                Some(i) => i.definition(object).await,
                None => Ok(None),
            }
        })
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
            Ok(self.db.open().await)
        })
    }

    fn release(&self, _connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let _ = session.close().await;
        })
    }
}

/// A UI host that does nothing; its presence makes the tool set list the UI-only tools.
pub struct StubUi;

impl UiHost for StubUi {
    fn open_in_editor<'a>(
        &'a self,
        _connection: Option<&'a str>,
        _sql: &'a str,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn open_results_tab<'a>(
        &'a self,
        _connection: &'a str,
        _sql: &'a str,
        _title: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A server over `source` with a memory audit and a stub UI host.
pub fn server(source: &Arc<SqliteSource>, options: ServerOptions) -> (McpServer, Arc<MemoryAudit>) {
    let tools = ToolSet::new(source.clone(), Limits::default()).with_ui_host(Arc::new(StubUi));
    let audit = Arc::new(MemoryAudit::new());
    (McpServer::new(tools, audit.clone(), options), audit)
}

/// A plain read that runs far longer than any test waits: a 3^20 cross join.
pub fn slow_sql() -> String {
    let from = (0..20)
        .map(|i| format!("users t{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("select count(*) from {from}")
}
