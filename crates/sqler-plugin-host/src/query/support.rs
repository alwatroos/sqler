// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Test helpers: a SQLite-backed [`DataService`] with spies, a scripted approver, a session that hangs, a session that
//! plays back events, and a discovered plugin on disk. File names carry the process id and a counter and avoid names
//! Windows reserves.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::{
    Column, ConnectionConfig, DataType, DbObject, DriverId, Environment, Listing, ObjectKind,
    ObjectPath, Query, Row, Table, Value,
};
use sqler_driver_api::{
    BoxFuture, CancelHandle, Canceller, Driver, DriverError, Introspect, QueryEvent, QueryStream,
    Session, collect,
};
use sqler_driver_sqlite::SqliteDriver;
use sqler_plugin_api::{Capability, CapabilitySet};
use tokio::sync::mpsc;

use super::{
    DataError, DataService, Decision, PluginConnection, PluginData, QueryApprover, WriteRequest,
};
use crate::dirs::PluginDirs;
use crate::discovery::{DiscoveredPlugin, discover};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn unique(tag: &str) -> String {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    format!("sqler-plugin-host-{tag}-{}-{n}", std::process::id())
}

/// A unique database file, removed on drop together with its journal files.
pub struct TempDb(pub PathBuf);

impl TempDb {
    pub fn new(tag: &str) -> Self {
        Self(std::env::temp_dir().join(format!("{}.db", unique(tag))))
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
            let _ = fs::remove_file(path);
        }
    }
}

/// A plugin directory with a manifest and no component, removed on drop.
pub struct TestPlugin {
    dir: PathBuf,
    pub plugin: Arc<DiscoveredPlugin>,
}

impl TestPlugin {
    pub fn new(id: &str, name: &str) -> Self {
        let dir = std::env::temp_dir().join(unique("plugin"));
        fs::create_dir_all(dir.join(id)).unwrap();
        fs::write(
            dir.join(id).join("sqler.toml"),
            format!("[plugin]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = []\n"),
        )
        .unwrap();
        let found = discover(&PluginDirs {
            dev: vec![dir.join(id)],
            ..PluginDirs::default()
        });
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        let plugin = Arc::new(found.plugins.into_iter().next().unwrap());
        Self { dir, plugin }
    }
}

impl Drop for TestPlugin {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The capabilities named: `db.read` and `db.write`.
pub fn caps(names: &[&str]) -> CapabilitySet {
    let mut set = CapabilitySet::new();
    for n in names {
        set.insert(match *n {
            "db.read" => Capability::DbRead,
            "db.write" => Capability::DbWrite,
            other => panic!("no test capability {other}"),
        });
    }
    set
}

/// Runs a statement on a session and drains it.
pub async fn exec(session: &mut dyn Session, sql: &str) {
    let query = Query::new(sql);
    let mut stream = session.execute(&query).await.unwrap();
    collect(&mut *stream).await.unwrap();
}

/// Wraps a session and records every executed SQL text and its parameters.
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

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        self.inner.close()
    }
}

/// Forwards the catalog's introspection calls to a SQLite session and counts them.
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
        _object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

pub const SEED: &[&str] = &[
    "create table users (id integer primary key, name text not null, email text default 'none')",
    "create table orders (id integer primary key, user_id integer references users(id), total real)",
    "create view user_names as select name from users",
    "create index users_name on users (name)",
    "insert into users (id, name, email) values (1,'ann','a@x'),(2,'bob','b@x'),(3,'cy','c@x'),(4,'di','d@x'),(5,'ed','e@x')",
];

/// One SQLite connection `db` lent to plugins, with spies. Needs a running tokio runtime.
pub struct SqliteData {
    pub db: TempDb,
    pub conn: PluginConnection,
    pub catalog: Catalog,
    pub acquires: AtomicUsize,
    pub releases: AtomicUsize,
    pub introspections: Arc<AtomicUsize>,
    /// Every SQL text that reached a lent session.
    pub executed: Arc<Mutex<Vec<String>>>,
    /// Whether the service has a catalog at all.
    pub with_catalog: bool,
}

impl SqliteData {
    pub async fn new(tag: &str, env: Option<Environment>, read_only: bool) -> Arc<Self> {
        Self::build(tag, env, read_only, true).await
    }

    /// A service whose `catalog` answers `None`.
    pub async fn without_catalog(tag: &str) -> Arc<Self> {
        Self::build(tag, None, false, false).await
    }

    async fn build(
        tag: &str,
        env: Option<Environment>,
        read_only: bool,
        with_catalog: bool,
    ) -> Arc<Self> {
        let db = TempDb::new(tag);
        let mut seeder = db.open().await;
        for sql in SEED {
            exec(&mut *seeder, sql).await;
        }
        let introspections = Arc::new(AtomicUsize::new(0));
        let boxed: Box<dyn Introspect> = Box::new(Counting {
            session: db.open().await,
            calls: Arc::clone(&introspections),
        });
        let (catalog, worker) = Catalog::new(boxed);
        tokio::spawn(worker.run());
        let conn = PluginConnection::new("db", "Test DB", DriverId::new("sqlite").unwrap(), env)
            .with_read_only(read_only);
        Arc::new(Self {
            db,
            conn,
            catalog,
            acquires: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            introspections,
            executed: Arc::new(Mutex::new(Vec::new())),
            with_catalog,
        })
    }

    pub fn executed(&self) -> Vec<String> {
        self.executed.lock().unwrap().clone()
    }

    pub fn introspection_calls(&self) -> usize {
        self.introspections.load(Ordering::SeqCst)
    }
}

impl DataService for SqliteData {
    fn connections(&self) -> Vec<PluginConnection> {
        vec![self.conn.clone()]
    }

    fn catalog(&self, connection: &str) -> Option<Catalog> {
        (self.with_catalog && connection == self.conn.id).then(|| self.catalog.clone())
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DataError>> {
        Box::pin(async move {
            if connection != self.conn.id {
                return Err(DataError::UnknownConnection(connection.to_owned()));
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
pub struct Scripted {
    requests: Mutex<Vec<WriteRequest>>,
    decide: Box<dyn Fn(&WriteRequest) -> Decision + Send + Sync>,
}

impl Scripted {
    pub fn new(decide: impl Fn(&WriteRequest) -> Decision + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            decide: Box::new(decide),
        })
    }

    pub fn always(decision: Decision) -> Arc<Self> {
        Self::new(move |_| decision)
    }

    pub fn requests(&self) -> Vec<WriteRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl QueryApprover for Scripted {
    fn approve<'a>(&'a self, request: &'a WriteRequest) -> BoxFuture<'a, Decision> {
        self.requests.lock().unwrap().push(request.clone());
        let decision = (self.decide)(request);
        Box::pin(std::future::ready(decision))
    }
}

/// `PluginData` over `source`, with an approver if given.
pub fn data(source: Arc<dyn DataService>, approver: Option<Arc<Scripted>>) -> Arc<PluginData> {
    let d = PluginData::new(source);
    Arc::new(match approver {
        Some(a) => d.with_approver(a),
        None => d,
    })
}

/// Counts cancels.
struct CountCancel(Arc<AtomicUsize>);

impl Canceller for CountCancel {
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(())))
    }
}

/// What a played-back stream does after its events.
#[derive(Clone, Copy)]
pub enum After {
    /// Ends (`None`).
    End,
    /// Hangs forever.
    Hang,
}

struct Playback {
    events: std::vec::IntoIter<QueryEvent>,
    error: Option<DriverError>,
    after: After,
    reached: mpsc::UnboundedSender<()>,
}

impl QueryStream for Playback {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        let next = self.events.next();
        let after = self.after;
        let reached = self.reached.clone();
        let error = if next.is_none() {
            self.error.take()
        } else {
            None
        };
        Box::pin(async move {
            if let Some(e) = error {
                return Some(Err(e));
            }
            match (next, after) {
                (Some(e), _) => Some(Ok(e)),
                (None, After::End) => None,
                (None, After::Hang) => {
                    let _ = reached.send(());
                    std::future::pending().await
                }
            }
        })
    }
}

/// Driver errors a [`ScriptData`] session fails with once.
#[derive(Default)]
pub struct FailWith {
    pub execute: Mutex<Option<DriverError>>,
    pub stream: Mutex<Option<DriverError>>,
}

struct ScriptSession {
    events: Vec<QueryEvent>,
    errors: Arc<FailWith>,
    after: After,
    cancels: Arc<AtomicUsize>,
    hang_execute: bool,
    reached: mpsc::UnboundedSender<()>,
}

impl Session for ScriptSession {
    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::new(Arc::new(CountCancel(Arc::clone(&self.cancels))))
    }

    fn execute<'s>(
        &'s mut self,
        _query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        if let Some(e) = self.errors.execute.lock().unwrap().take() {
            return Box::pin(std::future::ready(Err(e)));
        }
        if self.hang_execute {
            let reached = self.reached.clone();
            return Box::pin(async move {
                let _ = reached.send(());
                std::future::pending().await
            });
        }
        let stream = Playback {
            events: self.events.clone().into_iter(),
            error: self.errors.stream.lock().unwrap().take(),
            after: self.after,
            reached: self.reached.clone(),
        };
        Box::pin(std::future::ready(Ok(
            Box::new(stream) as Box<dyn QueryStream + 's>
        )))
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A connection `fake` (sqlite dialect) whose sessions play back `events`, then end or hang. Counts cancels and
/// releases; every release is also announced on a channel so a test can wait for it without sleeping.
pub struct ScriptData {
    pub conn: PluginConnection,
    events: Vec<QueryEvent>,
    after: After,
    hang_execute: bool,
    pub cancels: Arc<AtomicUsize>,
    pub acquires: AtomicUsize,
    pub releases: AtomicUsize,
    /// When set, `acquire` fails with this error.
    pub acquire_error: Mutex<Option<DataError>>,
    /// Driver errors the next session fails with, at `execute` or after the scripted events.
    pub errors: Arc<FailWith>,
    released_tx: mpsc::UnboundedSender<()>,
    released_rx: Mutex<Option<mpsc::UnboundedReceiver<()>>>,
    reached_tx: mpsc::UnboundedSender<()>,
    reached_rx: Mutex<Option<mpsc::UnboundedReceiver<()>>>,
}

impl ScriptData {
    pub fn new(events: Vec<QueryEvent>, after: After) -> Arc<Self> {
        Self::build("sqlite", events, after, false)
    }

    /// Like [`ScriptData::new`] on a connection of another driver.
    pub fn with_driver(driver: &str, events: Vec<QueryEvent>, after: After) -> Arc<Self> {
        Self::build(driver, events, after, false)
    }

    /// Sessions whose `execute` never answers.
    pub fn hanging_execute() -> Arc<Self> {
        Self::build("sqlite", Vec::new(), After::Hang, true)
    }

    fn build(driver: &str, events: Vec<QueryEvent>, after: After, hang_execute: bool) -> Arc<Self> {
        let (released_tx, released_rx) = mpsc::unbounded_channel();
        let (reached_tx, reached_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            conn: PluginConnection::new("fake", "Fake", DriverId::new(driver).unwrap(), None),
            events,
            after,
            hang_execute,
            cancels: Arc::new(AtomicUsize::new(0)),
            acquires: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            acquire_error: Mutex::new(None),
            errors: Arc::new(FailWith::default()),
            released_tx,
            released_rx: Mutex::new(Some(released_rx)),
            reached_tx,
            reached_rx: Mutex::new(Some(reached_rx)),
        })
    }

    /// One row of one integer column, then a hang.
    pub fn hanging() -> Arc<Self> {
        Self::new(
            vec![
                QueryEvent::Columns(vec![Column::new("a", DataType::Integer)]),
                QueryEvent::Rows(vec![Row(vec![Value::Int(1)])]),
            ],
            After::Hang,
        )
    }

    /// The channel that announces that a session is hanging in the driver; take it once.
    pub fn take_reached(&self) -> mpsc::UnboundedReceiver<()> {
        self.reached_rx.lock().unwrap().take().unwrap()
    }

    /// The channel that announces releases; take it once.
    pub fn take_released(&self) -> mpsc::UnboundedReceiver<()> {
        self.released_rx.lock().unwrap().take().unwrap()
    }
}

impl DataService for ScriptData {
    fn connections(&self) -> Vec<PluginConnection> {
        vec![self.conn.clone()]
    }

    fn catalog(&self, _connection: &str) -> Option<Catalog> {
        None
    }

    fn acquire<'a>(
        &'a self,
        _connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DataError>> {
        self.acquires.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = self.acquire_error.lock().unwrap().clone() {
            return Box::pin(std::future::ready(Err(e)));
        }
        let session = ScriptSession {
            events: self.events.clone(),
            errors: Arc::clone(&self.errors),
            after: self.after,
            cancels: Arc::clone(&self.cancels),
            hang_execute: self.hang_execute,
            reached: self.reached_tx.clone(),
        };
        Box::pin(std::future::ready(
            Ok(Box::new(session) as Box<dyn Session>),
        ))
    }

    fn release(&self, _connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        let tx = self.released_tx.clone();
        Box::pin(async move {
            let _ = session.close().await;
            let _ = tx.send(());
        })
    }
}

/// Real components (the `ok.wat` fixture) driven through `HostState`; needs a compiler.
#[cfg(feature = "cl")]
pub mod wasm {
    use std::pin::Pin;
    use std::time::Duration;

    use sqler_plugin_api::{GrantKey, Limits, PluginId};

    use super::{Arc, Mutex, PathBuf, PluginData, SqliteData, fs, unique};
    use crate::egress::NoEgress;
    use crate::grant::GrantSource;
    use crate::runtime::wit::{catalog, query, types};
    use crate::runtime::{CallFn, HostOptions, Live, PluginHost, Services};
    use crate::settings::MemorySettings;
    use crate::status::CallKind;
    use crate::{PluginDirs, discover};

    struct Approve(Mutex<Vec<(GrantKey, sqler_plugin_api::CapabilitySet)>>);

    impl GrantSource for Approve {
        fn approved(&self, key: &GrantKey) -> Option<sqler_plugin_api::CapabilitySet> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, s)| s.clone())
        }
    }

    /// A host with one plugin `probe` (the `ok` component) that declares `capabilities` and is approved all of them.
    pub struct Fixture {
        root: PathBuf,
        pub rt: tokio::runtime::Runtime,
        pub host: PluginHost,
        pub id: PluginId,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.host.shutdown();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    impl Fixture {
        pub fn new(capabilities: &[&str], data: Option<Arc<PluginData>>) -> Self {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            Self::start(capabilities, rt, data)
        }

        /// Like [`Fixture::new`] with `PluginData` over a SQLite service that `make` builds on the fixture's own
        /// runtime (its catalog worker needs it).
        pub fn serving<F: Future<Output = Arc<SqliteData>>>(
            capabilities: &[&str],
            make: impl FnOnce() -> F,
        ) -> (Self, Arc<SqliteData>) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let src = rt.block_on(make());
            let f = Self::start(capabilities, rt, Some(super::data(src.clone(), None)));
            (f, src)
        }

        fn start(
            capabilities: &[&str],
            rt: tokio::runtime::Runtime,
            data: Option<Arc<PluginData>>,
        ) -> Self {
            let root = std::env::temp_dir().join(unique("wasm"));
            let dir = root.join("probe");
            fs::create_dir_all(&dir).unwrap();
            let list = capabilities
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            fs::write(
                dir.join("sqler.toml"),
                format!("[plugin]\nid = \"probe\"\nname = \"Probe\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = [{list}]\n\n[entry]\ncomponent = \"plugin.wasm\"\n"),
            )
            .unwrap();
            fs::write(
                dir.join("plugin.wasm"),
                wat::parse_str(include_str!("../../tests/fixtures/wat/ok.wat")).unwrap(),
            )
            .unwrap();
            let d = discover(&PluginDirs {
                dev: vec![dir],
                ..PluginDirs::default()
            });
            assert!(d.problems.is_empty(), "{:?}", d.problems);
            let grants = Approve(Mutex::new(
                d.plugins
                    .iter()
                    .map(|p| (p.key.clone(), p.manifest.capabilities.clone()))
                    .collect(),
            ));
            let mut services = Services::new();
            if let Some(data) = data {
                services.insert(data);
            }
            let host = PluginHost::new(HostOptions {
                runtime: rt.handle().clone(),
                grants: Arc::new(grants),
                settings: Arc::new(MemorySettings::default()),
                egress: Arc::new(NoEgress),
                limits: Limits::DEFAULT,
                services,
            });
            assert_eq!(host.load(d).len(), 0);
            Self {
                root,
                rt,
                host,
                id: "probe".parse().unwrap(),
            }
        }

        /// Runs `fut` on the fixture's runtime with a hang guard.
        pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
            self.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(30), fut)
                    .await
                    .expect("hang guard")
            })
        }
    }

    type Out<T> = Result<T, types::PluginError>;

    /// A guest-side `query.run`.
    pub fn run_query(req: query::QueryRequest) -> CallFn<Out<query::QueryResult>> {
        Box::new(move |live: &mut Live| {
            Box::pin(async move { Ok(query::Host::run(live.store.data_mut(), req).await) })
                as Pin<Box<dyn Future<Output = _> + Send + '_>>
        })
    }

    /// A guest-side `catalog.connections`.
    pub fn connections() -> CallFn<Out<Vec<types::ConnectionInfo>>> {
        Box::new(|live: &mut Live| {
            Box::pin(async move { Ok(catalog::Host::connections(live.store.data_mut()).await) })
        })
    }

    /// A guest-side `catalog.children`.
    pub fn children(conn: &str, parent: types::ObjectPath) -> CallFn<Out<Vec<types::DbObject>>> {
        let conn = conn.to_owned();
        Box::new(move |live: &mut Live| {
            Box::pin(async move {
                Ok(catalog::Host::children(live.store.data_mut(), conn, parent).await)
            })
        })
    }

    /// A guest-side `catalog.columns`.
    pub fn columns(
        conn: &str,
        table: types::ObjectPath,
        name: &str,
    ) -> CallFn<Out<Vec<types::ColumnInfo>>> {
        let (conn, name) = (conn.to_owned(), name.to_owned());
        Box::new(move |live: &mut Live| {
            Box::pin(async move {
                Ok(catalog::Host::columns(live.store.data_mut(), conn, table, name).await)
            })
        })
    }

    /// A request for `sql` with no parameters and default bounds.
    pub fn request(connection: &str, sql: &str) -> query::QueryRequest {
        query::QueryRequest {
            connection: connection.to_owned(),
            sql: sql.to_owned(),
            params: Vec::new(),
            max_rows: None,
            timeout_ms: None,
        }
    }

    /// Starts a command call.
    pub fn command<R: Send + 'static>(f: &Fixture, call: CallFn<R>) -> crate::PendingCall<R> {
        f.host.run(&f.id, CallKind::Command, call)
    }
}
