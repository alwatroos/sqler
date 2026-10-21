// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Fake connector and session shared by the workspace unit tests.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sqler_driver_api::sqler_core::{DriverId, Query};
use sqler_driver_api::{BoxFuture, CancelFlag, CancelHandle, DriverError, QueryStream, Session};

use crate::connections::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt};

/// A session whose `close` sets a flag.
pub(crate) struct FakeSession {
    pub(crate) closed: Arc<AtomicBool>,
    /// `close` never finishes.
    pub(crate) hang_on_close: bool,
}

impl FakeSession {
    pub(crate) fn boxed() -> (Box<dyn Session>, Arc<AtomicBool>) {
        let closed = Arc::new(AtomicBool::new(false));
        (
            Box::new(Self {
                closed: closed.clone(),
                hang_on_close: false,
            }),
            closed,
        )
    }

    /// A session whose `close` records the call and then hangs.
    pub(crate) fn hanging_close() -> (Box<dyn Session>, Arc<AtomicBool>) {
        let closed = Arc::new(AtomicBool::new(false));
        (
            Box::new(Self {
                closed: closed.clone(),
                hang_on_close: true,
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
        if self.hang_on_close {
            return Box::pin(async {
                std::future::pending::<()>().await;
                Ok(())
            });
        }
        Box::pin(async { Ok(()) })
    }
}

/// Sets a flag when dropped.
pub(crate) struct DropFlag(pub(crate) Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

type Scripted = Result<Box<dyn Session>, ConnectFailure>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A [`Connector`] that plays back scripted results and records what it was asked.
///
/// With an empty script every attempt yields a [`FakeSession`]. In hang mode the attempt never
/// finishes; `started` and `dropped` show that it ran and that its future was dropped.
pub(crate) struct FakeConnector {
    script: Mutex<VecDeque<Scripted>>,
    hang: bool,
    trust_result: Mutex<Option<ConnectFailure>>,
    /// Every request passed to `connect`.
    pub(crate) requests: Mutex<Vec<ConnectRequest>>,
    /// Every key passed to `trust_host_key`.
    pub(crate) trusted: Mutex<Vec<HostKeyPrompt>>,
    /// Flags of the sessions handed out by the default (empty-script) path.
    pub(crate) closed_flags: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    pub(crate) started: Arc<AtomicBool>,
    pub(crate) dropped: Arc<AtomicBool>,
}

impl FakeConnector {
    pub(crate) fn new() -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            hang: false,
            trust_result: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            trusted: Mutex::new(Vec::new()),
            closed_flags: Arc::new(Mutex::new(Vec::new())),
            started: Arc::new(AtomicBool::new(false)),
            dropped: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn hanging() -> Self {
        Self {
            hang: true,
            ..Self::new()
        }
    }

    pub(crate) fn push(&self, result: Scripted) {
        lock(&self.script).push_back(result);
    }

    pub(crate) fn fail_trust(&self, failure: ConnectFailure) {
        *lock(&self.trust_result) = Some(failure);
    }
}

impl Connector for FakeConnector {
    fn driver_ids(&self) -> Vec<DriverId> {
        vec![DriverId::new("fake").unwrap()]
    }

    fn connect(&self, request: ConnectRequest) -> BoxFuture<'static, Scripted> {
        lock(&self.requests).push(request);
        if self.hang {
            let guard = DropFlag(self.dropped.clone());
            self.started.store(true, Ordering::SeqCst);
            return Box::pin(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
                Err(ConnectFailure::Aborted)
            });
        }
        let scripted = lock(&self.script).pop_front();
        let flags = Arc::clone(&self.closed_flags);
        let started = Arc::clone(&self.started);
        // The session is made when the future runs, like a real driver does.
        Box::pin(async move {
            started.store(true, Ordering::SeqCst);
            scripted.unwrap_or_else(|| {
                let (session, closed) = FakeSession::boxed();
                lock(&flags).push(closed);
                Ok(session)
            })
        })
    }

    fn trust_host_key(&self, key: HostKeyPrompt) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        lock(&self.trusted).push(key);
        let failure = lock(&self.trust_result).take();
        Box::pin(async move { failure.map_or(Ok(()), Err) })
    }
}

/// Whole-flow tests of inline editing on real `SQLite` files (ADR-0033).
#[cfg(test)]
mod flow {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

    use sqler_catalog::CacheKey;
    use sqler_config::MemoryStore;
    use sqler_driver_api::sqler_core::{ConnectionConfig, Environment, Value};
    use sqler_driver_api::{DriverRegistry, collect};
    use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
    use tokio::runtime::{Builder, Runtime};

    use super::*;
    use crate::browser::BrowserAction;
    use crate::grid::edit::{
        ApplyJob, COMMIT_UNKNOWN, CellMark, EditState, EditorInput, GridKey, LAST_WRITER_WINS,
        ReadOnlyReason,
    };
    use crate::grid::{CellPos, GridTab, IngestState};
    use crate::query::{BufferSnapshot, RunMode};
    use crate::sidebar::SidebarModel;
    use crate::tabs::TabId;
    use crate::workspace::{CatalogResult, CloseTarget, Effect, SessionBridge, Workspace};
    use crate::workspace::{WorkspaceAction, lost_apply_outcome};

    static FILES: AtomicUsize = AtomicUsize::new(0);

    /// A database file that is removed when dropped.
    struct TempDb(PathBuf);

    impl TempDb {
        fn new() -> Self {
            let n = FILES.fetch_add(1, Ordering::SeqCst);
            Self(std::env::temp_dir().join(format!("sqler-flow-{}-{n}.db", std::process::id())))
        }

        fn path(&self) -> String {
            self.0.display().to_string()
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    async fn open_sqlite(path: &str) -> Box<dyn Session> {
        let mut registry = DriverRegistry::new();
        registry.register(Box::new(SqliteDriver::new())).unwrap();
        let config = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), path);
        registry.connect(&config).await.unwrap()
    }

    async fn rows_of(session: &mut dyn Session, sql: &str) -> Vec<Vec<Value>> {
        let q = Query::new(sql);
        let mut stream = session.execute(&q).await.unwrap();
        let result = collect(&mut *stream).await.unwrap();
        result.rows().iter().map(|r| r.0.clone()).collect()
    }

    /// Which statements a wrapped session refuses.
    #[derive(Clone, Default)]
    struct Failing {
        begin: bool,
        commit: bool,
        rollback: bool,
    }

    /// A real session that fails `BEGIN`, `COMMIT` or `ROLLBACK` on request.
    struct Flaky {
        inner: Box<dyn Session>,
        failing: Failing,
    }

    impl Session for Flaky {
        fn cancel_handle(&self) -> CancelHandle {
            self.inner.cancel_handle()
        }

        fn execute<'s>(
            &'s mut self,
            query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            let refuse = match query.sql.trim() {
                "BEGIN" => self.failing.begin,
                "COMMIT" => self.failing.commit,
                "ROLLBACK" => self.failing.rollback,
                _ => false,
            };
            if refuse {
                return Box::pin(async { Err(DriverError::Query("refused for the test".into())) });
            }
            self.inner.execute(query)
        }

        fn introspection(&mut self) -> Option<&mut dyn sqler_driver_api::Introspect> {
            self.inner.introspection()
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            self.inner.close()
        }
    }

    /// Opens real `SQLite` sessions on the target of the request; counts the connects.
    struct SqliteConnector {
        connects: AtomicUsize,
    }

    impl Connector for SqliteConnector {
        fn driver_ids(&self) -> Vec<DriverId> {
            vec![DriverId::new(DRIVER_ID).unwrap()]
        }

        fn connect(
            &self,
            request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(open_sqlite(&request.spec.target).await) })
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    /// The workspace with one real connection, standing in for the view.
    struct Flow {
        rt: Runtime,
        bridge: SessionBridge,
        connector: Arc<SqliteConnector>,
        w: Workspace,
        db: TempDb,
        /// Catalog workers keep running while the flow lives.
        workers: Vec<tokio::sync::oneshot::Receiver<()>>,
    }

    impl Flow {
        /// `t(id integer primary key, name text)` with rows 1 a, 2 b, 3 c, and `k(a, b)` with no
        /// key and two identical rows.
        fn new(environment: Option<Environment>) -> Self {
            let rt = Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let db = TempDb::new();
            rt.block_on(async {
                let mut s = open_sqlite(&db.path()).await;
                for sql in [
                    "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
                    "INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')",
                    "CREATE TABLE k (a INTEGER, b TEXT)",
                    "INSERT INTO k VALUES (1, 'x'), (1, 'x')",
                ] {
                    let q = Query::new(sql);
                    let mut stream = s.execute(&q).await.unwrap();
                    collect(&mut *stream).await.unwrap();
                }
                s.close().await.unwrap();
            });
            let connector = Arc::new(SqliteConnector {
                connects: AtomicUsize::new(0),
            });
            let bridge = SessionBridge::new(
                connector.clone(),
                Arc::new(MemoryStore::new()),
                rt.handle().clone(),
            );
            let mut config =
                ConnectionConfig::new("db", DriverId::new(DRIVER_ID).unwrap(), db.path());
            if let Some(environment) = environment {
                config = config.with_environment(environment);
            }
            let w = Workspace::new(SidebarModel::from_connections(vec![config]));
            Self {
                rt,
                bridge,
                connector,
                w,
                db,
                workers: Vec::new(),
            }
        }

        fn conn(&self) -> sqler_config::ConnectionId {
            self.w.sidebar().entries()[0].id().clone()
        }

        /// Carries out the effects the flow needs, synchronously.
        fn pump(&mut self, effect: Effect) {
            match effect {
                Effect::None => {}
                Effect::Batch(effects) => effects.into_iter().for_each(|e| self.pump(e)),
                Effect::Connect { request, connect } => {
                    let result = self.rt.block_on(self.bridge.connect(connect));
                    let next = self.w.connect_finished(request, result);
                    self.pump(next);
                }
                Effect::StartCatalog { worker, .. } => {
                    let done = self.bridge.run_catalog(worker);
                    self.workers.push(done);
                }
                Effect::LoadCatalog {
                    conn,
                    generation,
                    keys,
                } => {
                    let catalog = self.w.catalog(&conn, generation).expect("catalog");
                    for key in keys {
                        let result = match &key {
                            CacheKey::Children(k) => CatalogResult::Children(
                                self.rt.block_on(catalog.children(k.clone())),
                            ),
                            CacheKey::Table(o) => {
                                CatalogResult::Table(self.rt.block_on(catalog.table(o.clone())))
                            }
                            _ => continue,
                        };
                        let next = self.w.catalog_loaded(&conn, generation, key, result);
                        self.pump(next);
                    }
                }
                Effect::CloseSession(session) => {
                    let _ = self.rt.block_on(session.close());
                }
                other => panic!("unexpected effect {other:?}"),
            }
        }

        /// Opens the connection (a query tab) and lists the tables of the database.
        fn open(&mut self) -> TabId {
            let effect = self.w.dispatch(WorkspaceAction::OpenConnection(0));
            self.pump(effect);
            let tab = self.w.tabs().active().map(crate::tabs::Tab::id).unwrap();
            for prefix in ["Schemas", "main", "Tables"] {
                self.expand(prefix);
            }
            tab
        }

        fn expand(&mut self, prefix: &str) {
            let id = self.conn();
            let node = self
                .w
                .browser(&id)
                .unwrap()
                .rows()
                .iter()
                .find(|r| r.label.starts_with(prefix))
                .unwrap_or_else(|| panic!("no row {prefix}"))
                .id;
            let effect = self.w.dispatch_object(&id, node, BrowserAction::Expand);
            self.pump(effect);
        }

        /// Runs a script on the tab like the view does, and returns the finished grids.
        fn run(&mut self, tab: TabId, sql: &str) -> Vec<GridTab> {
            self.run_with(tab, sql, |_| {})
        }

        /// [`Self::run`] after `tweak` changed the job.
        fn run_with(
            &mut self,
            tab: TabId,
            sql: &str,
            tweak: impl FnOnce(&mut crate::query::QueryJob),
        ) -> Vec<GridTab> {
            let effect = self
                .w
                .run_query(tab, RunMode::All, BufferSnapshot::at_cursor(sql, 0));
            let Effect::RunQuery {
                mut job,
                mut grids,
                mut events,
            } = effect
            else {
                panic!("not started: {:?} {:?}", effect, self.w.take_notice());
            };
            tweak(&mut job);
            let run = job.run;
            let done = self.rt.block_on(job.run());
            while let Ok(event) = events.try_recv() {
                let next = self.w.query_event(tab, run, event);
                if let Effect::RecordHistory(_) = next {
                    continue;
                }
                self.pump(next);
            }
            let next = self.w.query_finished(done);
            self.pump(next);
            for grid in &mut grids {
                // A statement that failed or never ran leaves its grid unfinished.
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
                while !matches!(grid.state(), IngestState::Done { .. })
                    && std::time::Instant::now() < deadline
                {
                    grid.poll(usize::MAX);
                }
            }
            grids
        }

        /// Pushes the edit context of the grid until the catalog has answered.
        fn context(&mut self, key: GridKey, grid: &mut GridTab) {
            for _ in 0..3 {
                let (ctx, effect) = self.w.edit_context(key).expect("a context");
                grid.set_edit_context(ctx);
                self.pump(effect);
            }
        }

        /// Asks to apply and, when the job starts, runs it and reports it to the grid.
        fn apply(&mut self, key: GridKey, grid: &mut GridTab) -> Option<String> {
            let plan = grid.edit().plan();
            let effect = self.w.request_apply(key, plan);
            self.finish(key, grid, effect)
        }

        fn finish(&mut self, key: GridKey, grid: &mut GridTab, effect: Effect) -> Option<String> {
            let Effect::ApplyEdits(job) = effect else {
                return None;
            };
            Some(self.run_job(key, grid, job))
        }

        fn run_job(&mut self, key: GridKey, grid: &mut GridTab, job: ApplyJob) -> String {
            grid.edit_parts().1.begin_apply(job.statement_rows());
            let done = self.rt.block_on(job.run(self.bridge.connector()));
            let finished = self.w.apply_finished(done);
            self.pump(finished.effect);
            assert_eq!(finished.key, key);
            let message = grid.edit_parts().1.finish_apply(&finished.outcome);
            self.w.set_pending_edits(key, grid.edit().pending());
            message
        }

        fn sync(&mut self, key: GridKey, grid: &GridTab) {
            self.w.set_pending_edits(key, grid.edit().pending());
        }

        /// Runs SQL on the database file before the workspace opens it.
        fn exec(&self, sql: &str) {
            let path = self.db.path();
            self.rt.block_on(async {
                let mut s = open_sqlite(&path).await;
                let _ = rows_of(&mut *s, sql).await;
                s.close().await.unwrap();
            });
        }

        /// Reads through a session of its own, as another client would.
        fn select(&self, sql: &str) -> Vec<Vec<Value>> {
            let path = self.db.path();
            self.rt.block_on(async {
                let mut s = open_sqlite(&path).await;
                let rows = rows_of(&mut *s, sql).await;
                s.close().await.unwrap();
                rows
            })
        }

        /// Replaces the session of the tab by one that refuses some statements.
        fn make_flaky(&mut self, tab: TabId, failing: Failing) {
            let inner = self.w.take_session(tab).unwrap();
            self.w
                .restore_session(tab, Box::new(Flaky { inner, failing }));
        }
    }

    fn text(s: &str) -> EditorInput {
        EditorInput::Text(s.to_owned())
    }

    fn at(row: usize, col: usize) -> CellPos {
        CellPos { row, col }
    }

    fn key_of(tab: TabId) -> GridKey {
        GridKey {
            tab,
            result: Some(0),
        }
    }

    fn set_cell(grid: &mut GridTab, row: usize, col: usize, value: &str) {
        let (model, editor) = grid.edit_parts();
        editor.start_edit(model, at(row, col)).unwrap();
        editor.commit_edit(model, text(value)).unwrap();
    }

    /// A query tab on `SELECT * FROM t`, editable, with its grid.
    fn editable(f: &mut Flow) -> (TabId, GridKey, GridTab) {
        let tab = f.open();
        let key = key_of(tab);
        let mut grid = f.run(tab, "SELECT * FROM t").remove(0);
        f.context(key, &mut grid);
        assert_eq!(
            grid.edit_state(),
            EditState::Editable,
            "{:?}",
            grid.edit_state()
        );
        (tab, key, grid)
    }

    fn stage_three(grid: &mut GridTab) {
        set_cell(grid, 0, 1, "A");
        let (model, editor) = grid.edit_parts();
        editor.delete_rows(model, &[1]).unwrap();
        let row = editor.add_row(model).unwrap();
        set_cell(grid, row, 0, "10");
        set_cell(grid, row, 1, "new");
    }

    fn texts(rows: &[Vec<Value>]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|v| match v {
                        Value::Int(i) => i.to_string(),
                        Value::Text(t) => t.clone(),
                        _ => "?".to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect()
    }

    #[test]
    fn query_result_whole_flow() {
        let mut f = Flow::new(Some(Environment::Dev));
        let (_, key, mut grid) = editable(&mut f);
        stage_three(&mut grid);
        assert_eq!(grid.edit().pending(), 3);
        f.sync(key, &grid);
        let message = f.apply(key, &mut grid).expect("the job ran");
        assert_eq!(message, "Applied 3 change(s).");
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,A", "3,c", "10,new"]
        );
        // The overlay shows what was written; nothing is pending any more.
        assert_eq!(grid.edit().pending(), 0);
        assert_eq!(grid.display_cell(0, 1).text, "A");
        assert_eq!(grid.display_cell(1, 0).mark, CellMark::DeletedApplied);
        assert_eq!(grid.display_row_count(), 4);
        assert_eq!(grid.display_cell(3, 1).text, "new");
        f.sync(key, &grid);
        assert!(!f.w.is_applying(key.tab));
        assert!(f.w.has_session(key.tab), "the lent session came back");
    }

    /// Opens the table-data tab of `name` and loads its rows.
    fn open_data(f: &mut Flow, name: &str) -> (TabId, GridKey, GridTab) {
        let id = f.conn();
        let node =
            f.w.browser(&id)
                .unwrap()
                .rows()
                .iter()
                .find(|r| r.label == name)
                .unwrap_or_else(|| panic!("no row {name}"))
                .id;
        let effect = f.w.dispatch_object(
            &id,
            node,
            BrowserAction::Entry(crate::browser::MenuEntry::OpenData),
        );
        let Effect::OpenData { tab, mut grid, job } = effect else {
            panic!("expected OpenData, got {effect:?}");
        };
        f.rt.block_on(job.run(f.bridge.connector())).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !matches!(grid.state(), IngestState::Done { .. }) {
            grid.poll(usize::MAX);
            assert!(std::time::Instant::now() < deadline, "grid never finished");
        }
        let key = GridKey { tab, result: None };
        f.context(key, &mut grid);
        (tab, key, grid)
    }

    #[test]
    fn table_data_whole_flow_uses_a_short_lived_session() {
        let mut f = Flow::new(None);
        let query_tab = f.open();
        let (tab, key, mut grid) = open_data(&mut f, "t");
        assert_ne!(tab, query_tab);
        assert_eq!(grid.edit_state(), EditState::Editable);
        stage_three(&mut grid);
        f.sync(key, &grid);
        let before = f.connector.connects.load(Ordering::SeqCst);
        let message = f.apply(key, &mut grid).expect("the job ran");
        assert_eq!(message, "Applied 3 change(s).");
        // The job opened one session of its own and closed it; the data tab never had one.
        assert_eq!(f.connector.connects.load(Ordering::SeqCst), before + 1);
        assert!(!f.w.has_session(tab));
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,A", "3,c", "10,new"]
        );
        assert_eq!(grid.edit().pending(), 0);
    }

    #[test]
    fn a_failed_connect_keeps_the_edits_of_a_data_tab() {
        let mut f = Flow::new(None);
        f.open();
        let (_, key, mut grid) = open_data(&mut f, "t");
        set_cell(&mut grid, 0, 1, "A");
        let plan = grid.edit().plan();
        let Effect::ApplyEdits(mut job) = f.w.request_apply(key, plan) else {
            panic!("expected a job");
        };
        // The user pressed Cancel before the connection was made.
        let _ = job.stop.cancel();
        job.session = crate::grid::edit::ApplySession::Connect(f.w.request_for(&f.conn()).unwrap());
        let message = f.run_job(key, &mut grid, job);
        assert!(message.contains("Your changes are kept."), "{message}");
        assert_eq!(grid.edit().pending(), 1);
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,a", "2,b", "3,c"]
        );
    }

    #[test]
    fn production_asks_once_and_lists_every_statement() {
        let mut f = Flow::new(Some(Environment::Prod));
        let (tab, key, mut grid) = editable(&mut f);
        stage_three(&mut grid);
        f.sync(key, &grid);
        let plan = grid.edit().plan();
        let effect = f.w.request_apply(key, plan);
        assert!(matches!(effect, Effect::None), "{effect:?}");
        // Nothing ran, the session is still with the tab, and the dialog has the statements.
        assert!(f.w.has_session(tab));
        assert!(!f.w.is_applying(tab));
        let confirm = f.w.edit_confirm().expect("a confirmation");
        // BEGIN, update, delete, insert, COMMIT, ROLLBACK.
        assert_eq!(confirm.statements.len(), 6);
        assert_eq!(confirm.data_statements(), 3);
        assert_eq!(confirm.connection, "db");
        assert!(confirm.text.contains(LAST_WRITER_WINS));
        assert_eq!(confirm.notice(), LAST_WRITER_WINS);
        assert!(
            confirm
                .statements
                .iter()
                .any(|s| s.sql.starts_with("INSERT"))
        );
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,a", "2,b", "3,c"]
        );

        // Declining writes nothing, and a confirm that follows finds nothing.
        f.w.dismiss_apply();
        assert!(f.w.edit_confirm().is_none());
        assert!(matches!(
            f.w.confirm_apply(grid.edit().plan()),
            Effect::None
        ));
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,a", "2,b", "3,c"]
        );

        // A new attempt, confirmed: a job whose plan carries both flags.
        let plan = grid.edit().plan();
        assert!(matches!(f.w.request_apply(key, plan), Effect::None));
        let Effect::ApplyEdits(job) = f.w.confirm_apply(grid.edit().plan()) else {
            panic!("expected a job");
        };
        assert!(job.plan.production());
        assert!(job.plan.user_confirmed());
        // The confirmation was used up.
        assert!(f.w.edit_confirm().is_none());
        assert!(matches!(
            f.w.confirm_apply(grid.edit().plan()),
            Effect::None
        ));
        let message = f.run_job(key, &mut grid, job);
        assert_eq!(message, "Applied 3 change(s).");
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,A", "3,c", "10,new"]
        );
    }

    #[test]
    fn production_gate_holds_on_a_table_data_tab_too() {
        let mut f = Flow::new(Some(Environment::Prod));
        f.open();
        let (tab, key, mut grid) = open_data(&mut f, "t");
        set_cell(&mut grid, 0, 1, "A");
        let plan = grid.edit().plan();
        assert!(matches!(f.w.request_apply(key, plan), Effect::None));
        assert!(f.w.edit_confirm().is_some());
        assert!(!f.w.is_applying(tab));
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,a", "2,b", "3,c"]
        );
    }

    #[test]
    fn a_saved_connection_relabelled_prod_is_honoured_on_the_next_apply() {
        // The tab opened as dev; the saved connection is relabelled prod afterwards. The gate
        // takes the stricter of the two on every request.
        let mut f = Flow::new(Some(Environment::Dev));
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        let id = f.conn();
        let spec =
            f.w.sidebar
                .profile(&id)
                .unwrap()
                .spec()
                .clone()
                .with_environment(Environment::Prod);
        f.w.sidebar.update_connection(&id, spec).unwrap();
        assert_eq!(
            f.w.tabs().get(tab).unwrap().connection().environment,
            Some(Environment::Dev)
        );
        let plan = grid.edit().plan();
        assert!(matches!(f.w.request_apply(key, plan), Effect::None));
        assert!(f.w.edit_confirm().is_some());
        assert!(!f.w.is_applying(tab));
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,a", "2,b", "3,c"]
        );
    }

    #[test]
    fn dev_staging_and_unlabelled_connections_go_straight_to_the_job() {
        for environment in [Some(Environment::Dev), Some(Environment::Staging), None] {
            let mut f = Flow::new(environment);
            let (_, key, mut grid) = editable(&mut f);
            set_cell(&mut grid, 0, 1, "A");
            let plan = grid.edit().plan();
            let Effect::ApplyEdits(job) = f.w.request_apply(key, plan) else {
                panic!("{environment:?}: expected a job");
            };
            assert!(!job.plan.production());
            assert!(!job.plan.user_confirmed());
            assert!(f.w.edit_confirm().is_none());
            f.run_job(key, &mut grid, job);
            assert_eq!(texts(&f.select("SELECT * FROM t WHERE id = 1")), ["1,A"]);
        }
    }

    #[test]
    fn a_tab_that_cannot_be_found_counts_as_production() {
        let mut f = Flow::new(Some(Environment::Dev));
        let tab = f.open();
        assert!(!f.w.production(tab));
        f.w.dispatch(WorkspaceAction::CloseTab(tab));
        assert!(f.w.production(tab), "a missing tab fails closed");
        // And a request for it is refused outright.
        let effect = f.w.request_apply(
            key_of(tab),
            Err(crate::grid::edit::EditError::NothingStaged),
        );
        assert!(matches!(effect, Effect::None));
        assert!(f.w.take_notice().is_some());
    }

    #[test]
    fn every_path_asks_the_same_gate_with_a_fresh_plan_and_flag() {
        // A confirmation for one attempt is replaced, not extended, by the next.
        let mut f = Flow::new(Some(Environment::Prod));
        let (_, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        f.w.request_apply(key, grid.edit().plan());
        assert_eq!(f.w.edit_confirm().unwrap().data_statements(), 1);
        set_cell(&mut grid, 1, 1, "B");
        f.w.request_apply(key, grid.edit().plan());
        assert_eq!(f.w.edit_confirm().unwrap().data_statements(), 2);
        // A refused attempt clears a stale confirmation, so it cannot be confirmed later.
        f.w.request_apply(key, Err(crate::grid::edit::EditError::NothingStaged));
        assert!(f.w.edit_confirm().is_none());
        assert!(matches!(
            f.w.confirm_apply(grid.edit().plan()),
            Effect::None
        ));
    }

    #[test]
    fn preview_and_confirmation_carry_the_last_writer_wins_notice() {
        let mut f = Flow::new(Some(Environment::Dev));
        let (_, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        f.w.open_preview(key, grid.edit().plan());
        let preview = f.w.edit_preview().expect("a preview");
        assert!(preview.text.contains(LAST_WRITER_WINS));
        assert_eq!(preview.statements.len(), 4);
        // Previewing applies nothing.
        assert!(!f.w.is_applying(key.tab));
        assert_eq!(texts(&f.select("SELECT * FROM t WHERE id = 1")), ["1,a"]);
        f.w.close_preview();
        assert!(f.w.edit_preview().is_none());
    }

    #[test]
    fn an_edit_between_request_and_confirm_writes_nothing() {
        let mut f = Flow::new(Some(Environment::Prod));
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        f.sync(key, &grid);
        assert!(matches!(
            f.w.request_apply(key, grid.edit().plan()),
            Effect::None
        ));
        assert!(f.w.edit_confirm().is_some());
        // The user edits another cell while the dialog is open.
        set_cell(&mut grid, 1, 1, "B");
        let effect = f.w.confirm_apply(grid.edit().plan());
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(f.w.edit_confirm().is_none());
        assert!(!f.w.is_applying(tab));
        assert!(f.w.take_notice().unwrap().contains("edited after"));
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,a", "2,b", "3,c"]
        );
        // Nothing is lost: the changes are still pending, and a fresh attempt works.
        assert_eq!(grid.edit().pending(), 2);
    }

    #[test]
    fn a_discard_between_request_and_confirm_writes_nothing() {
        let mut f = Flow::new(Some(Environment::Prod));
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        f.sync(key, &grid);
        assert!(matches!(
            f.w.request_apply(key, grid.edit().plan()),
            Effect::None
        ));
        assert!(f.w.edit_confirm().is_some());
        grid.edit_parts().1.discard();
        f.sync(key, &grid);
        // The dialog for changes that no longer exist is gone.
        assert!(f.w.edit_confirm().is_none());
        let effect = f.w.confirm_apply(grid.edit().plan());
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(!f.w.is_applying(tab));
        assert_eq!(
            texts(&f.select("SELECT * FROM t ORDER BY id")),
            ["1,a", "2,b", "3,c"]
        );
    }

    #[test]
    fn autocommit_off_keeps_the_flag_after_commit() {
        use super::super::tx::TxEffect::{AutocommitOff, AutocommitOn, Begin, End};
        let mut f = Flow::new(None);
        let tab = f.open();
        assert!(f.w.edit.follow(tab, Begin, true, true, true));
        assert!(f.w.edit.follow(tab, End, true, true, true));
        assert!(!f.w.in_transaction(tab));
        // `SET autocommit = 0`, then `COMMIT`: the session is in a transaction still.
        assert!(f.w.edit.follow(tab, AutocommitOff, true, true, true));
        assert!(f.w.in_transaction(tab));
        assert!(!f.w.edit.follow(tab, End, true, true, true));
        assert!(f.w.in_transaction(tab));
        // `SET autocommit = 1` ends it.
        assert!(f.w.edit.follow(tab, AutocommitOn, true, true, true));
        assert!(!f.w.in_transaction(tab));
        assert!(f.w.edit.follow(tab, Begin, true, true, true));
        assert!(f.w.edit.follow(tab, End, true, true, true));
        assert!(!f.w.in_transaction(tab));
    }

    #[test]
    fn debug_of_the_dialogs_and_the_job_shows_no_values_or_sql() {
        let mut f = Flow::new(Some(Environment::Prod));
        let (_, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "sekrit-value");
        f.w.open_preview(key, grid.edit().plan());
        let shown = format!("{:?}", f.w.edit_preview().unwrap());
        f.w.request_apply(key, grid.edit().plan());
        let confirm = format!("{:?}", f.w.edit_confirm().unwrap());
        let effect = f.w.confirm_apply(grid.edit().plan());
        let job = format!("{effect:?}");
        for text in [shown, confirm, job] {
            for secret in ["sekrit", "UPDATE", "WHERE"] {
                assert!(!text.contains(secret), "{text}");
            }
        }
    }

    #[test]
    fn a_plan_that_could_not_be_built_is_a_notice() {
        let mut f = Flow::new(None);
        let (_, key, grid) = editable(&mut f);
        let effect = f.w.request_apply(key, grid.edit().plan());
        assert!(matches!(effect, Effect::None));
        assert_eq!(f.w.take_notice().as_deref(), Some("nothing is staged"));
    }

    #[test]
    fn a_context_pushed_mid_run_is_stale_once_the_run_ended() {
        let mut f = Flow::new(None);
        let tab = f.open();
        let key = key_of(tab);
        let effect = f.w.run_query(
            tab,
            RunMode::All,
            BufferSnapshot::at_cursor("SELECT * FROM t", 0),
        );
        let Effect::RunQuery {
            job,
            mut grids,
            mut events,
        } = effect
        else {
            panic!("not started");
        };
        // What the view pushed while the run was going.
        let seen = f.w.edit_push_state(key);
        let (ctx, _) = f.w.edit_context(key).unwrap();
        assert!(ctx.apply_block.is_some_and(|m| m.contains("running")));
        assert!(!f.w.edit_push_stale(key, &seen));
        let run = job.run;
        let done = f.rt.block_on(job.run());
        while let Ok(event) = events.try_recv() {
            let next = f.w.query_event(tab, run, event);
            if !matches!(next, Effect::RecordHistory(_)) {
                f.pump(next);
            }
        }
        let next = f.w.query_finished(done);
        f.pump(next);
        assert!(grids.pop().is_some());
        // The view pushes again whatever happened to the generation.
        assert!(f.w.edit_push_stale(key, &seen));
        let (ctx, _) = f.w.edit_context(key).unwrap();
        assert_eq!(ctx.apply_block, None);
        assert!(!f.w.edit_push_stale(key, &f.w.edit_push_state(key)));
    }

    #[test]
    fn begin_blocks_the_apply_and_commit_releases_it() {
        let mut f = Flow::new(None);
        let tab = f.open();
        let key = key_of(tab);
        f.run(tab, "BEGIN");
        assert!(f.w.in_transaction(tab));
        let mut grid = f.run(tab, "SELECT * FROM t").remove(0);
        // `BEGIN` is statement 0 of its own run; the select is statement 0 of the next.
        f.context(key, &mut grid);
        set_cell(&mut grid, 0, 1, "A");
        let (ctx, _) = f.w.edit_context(key).unwrap();
        assert!(
            ctx.apply_block
                .as_deref()
                .is_some_and(|m| m.contains("transaction"))
        );
        let effect = f.w.request_apply(key, grid.edit().plan());
        assert!(matches!(effect, Effect::None));
        assert!(f.w.take_notice().unwrap().contains("transaction"));
        assert_eq!(texts(&f.select("SELECT * FROM t WHERE id = 1")), ["1,a"]);

        // The pending change blocks a run, so the user discards it, ends the transaction and
        // edits again.
        grid.edit_parts().1.discard();
        f.sync(key, &grid);
        f.run(tab, "COMMIT");
        assert!(!f.w.in_transaction(tab));
        let mut grid = f.run(tab, "SELECT * FROM t").remove(0);
        f.context(key, &mut grid);
        set_cell(&mut grid, 0, 1, "A");
        assert!(f.apply(key, &mut grid).is_some());
        assert_eq!(texts(&f.select("SELECT * FROM t WHERE id = 1")), ["1,A"]);
    }

    #[test]
    fn a_begin_in_a_script_and_a_failed_begin_both_set_the_flag() {
        let mut f = Flow::new(None);
        let tab = f.open();
        f.run(tab, "SELECT 1; BEGIN; SELECT 2");
        assert!(f.w.in_transaction(tab));
        f.run(tab, "ROLLBACK");
        assert!(!f.w.in_transaction(tab));
        // A begin that fails still counts: the safe direction.
        f.run(tab, "BEGIN; BEGIN");
        assert!(f.w.in_transaction(tab));
        // A failed COMMIT (none open on a fresh session would be fine, so end it for real).
        f.run(tab, "COMMIT");
        assert!(!f.w.in_transaction(tab));
    }

    #[test]
    fn savepoint_and_rollback_to_keep_the_flag() {
        let mut f = Flow::new(None);
        let tab = f.open();
        f.run(tab, "SAVEPOINT a");
        assert!(f.w.in_transaction(tab));
        f.run(tab, "ROLLBACK TO a");
        assert!(f.w.in_transaction(tab));
        f.run(tab, "RELEASE a");
        // `RELEASE` of the outermost savepoint commits on SQLite, but the app cannot know that
        // without reading more than the text: it stays "in a transaction".
        assert!(f.w.in_transaction(tab));
        f.run(tab, "COMMIT");
        assert!(!f.w.in_transaction(tab));
    }

    #[test]
    fn a_temp_table_that_shadows_the_loaded_one_is_a_mismatch() {
        let mut f = Flow::new(None);
        let tab = f.open();
        f.run(
            tab,
            "CREATE TEMP TABLE t (id INTEGER PRIMARY KEY, name TEXT); \
             INSERT INTO temp.t VALUES (1, 'temp')",
        );
        let key = key_of(tab);
        let mut grid = f.run(tab, "SELECT * FROM t").remove(0);
        assert_eq!(grid.cell_text(0, 1), "temp");
        f.context(key, &mut grid);
        let (ctx, _) = f.w.edit_context(key).unwrap();
        assert_eq!(ctx.resolved_schema.as_deref(), Some("temp"));
        assert_eq!(
            grid.edit_state(),
            EditState::ReadOnly(ReadOnlyReason::TableMismatch)
        );
        // The qualified name reaches the loaded table and is editable.
        let mut qualified = f.run(tab, "SELECT * FROM main.t").remove(0);
        f.context(key, &mut qualified);
        assert_eq!(qualified.edit_state(), EditState::Editable);
    }

    #[test]
    fn a_probe_that_did_not_answer_leaves_the_result_read_only() {
        let mut f = Flow::new(None);
        let tab = f.open();
        let key = key_of(tab);
        let mut grid = f.run(tab, "SELECT * FROM t").remove(0);
        f.context(key, &mut grid);
        assert_eq!(grid.edit_state(), EditState::Editable);
        // The same text on a run that has no probe, as when the lookup could not run.
        let mut grid = f
            .run_with(tab, "SELECT * FROM t", |job| job.probes.clear())
            .remove(0);
        f.context(key, &mut grid);
        let (ctx, _) = f.w.edit_context(key).unwrap();
        assert_eq!(ctx.resolved_schema, None);
        assert_eq!(
            grid.edit_state(),
            EditState::ReadOnly(ReadOnlyReason::TableMismatch)
        );
    }

    #[test]
    fn keyless_duplicate_rows_roll_back_and_keep_the_edits() {
        let mut f = Flow::new(None);
        let tab = f.open();
        let key = key_of(tab);
        let mut grid = f.run(tab, "SELECT * FROM k").remove(0);
        f.context(key, &mut grid);
        assert!(matches!(grid.edit_state(), EditState::ReadOnly(_)));
        let columns = grid.edit_parts().0.columns().to_vec();
        assert!(grid.edit_parts().1.allow_keyless(&columns));
        set_cell(&mut grid, 0, 1, "y");
        f.sync(key, &grid);
        let message = f.apply(key, &mut grid).expect("the job ran");
        assert!(message.starts_with("Nothing was written"), "{message}");
        assert!(message.ends_with("Your changes are kept."), "{message}");
        assert_eq!(grid.edit().pending(), 1);
        assert_eq!(grid.display_cell(0, 1).mark, CellMark::Failed);
        assert_eq!(texts(&f.select("SELECT * FROM k")), ["1,x", "1,x"]);
        // The rollback worked, so the session is not in a transaction.
        assert!(!f.w.in_transaction(tab));
    }

    #[test]
    fn commit_unknown_blocks_apply_until_discard() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        f.make_flaky(
            tab,
            Failing {
                commit: true,
                ..Failing::default()
            },
        );
        set_cell(&mut grid, 0, 1, "A");
        f.sync(key, &grid);
        let message = f.apply(key, &mut grid).expect("the job ran");
        assert!(message.starts_with(COMMIT_UNKNOWN), "{message}");
        assert_eq!(
            COMMIT_UNKNOWN,
            "The outcome is uncertain: re-read before retrying."
        );
        assert_eq!(grid.edit().apply_blocked(), Some(COMMIT_UNKNOWN));
        assert_eq!(grid.edit().pending(), 1);
        f.sync(key, &grid);
        // The gate itself refuses, whatever the grid says.
        f.context(key, &mut grid);
        let effect = f.w.request_apply(key, grid.edit().plan());
        assert!(matches!(effect, Effect::None));
        assert_eq!(f.w.take_notice().as_deref(), Some(COMMIT_UNKNOWN));
        // Discarding is what unblocks it.
        grid.edit_parts().1.discard();
        f.sync(key, &grid);
        f.context(key, &mut grid);
        assert_eq!(grid.edit().apply_blocked(), None);
        // The rollback worked: the session is not in a transaction.
        assert!(!f.w.in_transaction(tab));
    }

    #[test]
    fn a_rollback_that_fails_too_sets_the_transaction_flag() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        f.make_flaky(
            tab,
            Failing {
                commit: true,
                rollback: true,
                ..Failing::default()
            },
        );
        set_cell(&mut grid, 0, 1, "A");
        f.apply(key, &mut grid).expect("the job ran");
        assert!(f.w.in_transaction(tab));
    }

    #[test]
    fn an_apply_that_did_not_start_sets_the_transaction_flag() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        f.make_flaky(
            tab,
            Failing {
                begin: true,
                ..Failing::default()
            },
        );
        set_cell(&mut grid, 0, 1, "A");
        let message = f.apply(key, &mut grid).expect("the job ran");
        assert!(message.starts_with("The apply did not start"), "{message}");
        assert!(f.w.in_transaction(tab));
        assert_eq!(grid.edit().pending(), 1);
    }

    #[test]
    fn a_lost_apply_blocks_the_grid_and_the_tab_loses_its_session() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        let Effect::ApplyEdits(job) = f.w.request_apply(key, grid.edit().plan()) else {
            panic!("expected a job");
        };
        let run = job.run;
        grid.edit_parts().1.begin_apply(job.statement_rows());
        drop(job); // the task died with the session
        f.w.apply_lost(key, run);
        assert!(!f.w.is_applying(tab));
        assert!(!f.w.has_session(tab));
        let message = grid.edit_parts().1.finish_apply(&lost_apply_outcome());
        assert!(message.starts_with(COMMIT_UNKNOWN), "{message}");
        assert!(f.w.take_notice().unwrap().contains(COMMIT_UNKNOWN));
        let effect = f.w.request_apply(key, grid.edit().plan());
        assert!(matches!(effect, Effect::None));
    }

    #[test]
    fn a_second_apply_is_refused_while_one_is_in_flight() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        let Effect::ApplyEdits(job) = f.w.request_apply(key, grid.edit().plan()) else {
            panic!("expected a job");
        };
        assert!(f.w.is_applying(tab));
        assert!(!f.w.has_session(tab), "the session is on loan");
        let effect = f.w.request_apply(key, grid.edit().plan());
        assert!(matches!(effect, Effect::None));
        assert_eq!(
            f.w.take_notice().as_deref(),
            Some("Changes are being applied on this tab.")
        );
        // Neither can a query run, nor the tab close.
        let run =
            f.w.run_query(tab, RunMode::All, BufferSnapshot::at_cursor("select 1", 0));
        assert!(matches!(run, Effect::None));
        assert_eq!(
            f.w.take_notice().as_deref(),
            Some("Changes are being applied on this tab.")
        );
        assert!(matches!(
            f.w.dispatch(WorkspaceAction::CloseTab(tab)),
            Effect::None
        ));
        assert_eq!(f.w.tabs().len(), 1);
        assert!(f.w.take_notice().is_some());
        f.run_job(key, &mut grid, job);
        assert!(!f.w.is_applying(tab));
        assert!(f.w.has_session(tab));
    }

    #[test]
    fn cancel_before_the_job_starts_writes_nothing() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        let Effect::ApplyEdits(job) = f.w.request_apply(key, grid.edit().plan()) else {
            panic!("expected a job");
        };
        // The cancel command of the tab goes to the apply; the handle is not stored yet.
        let effect = f.w.cancel_query(tab);
        assert!(matches!(effect, Effect::None));
        assert!(job.stop.is_cancelled());
        let message = f.run_job(key, &mut grid, job);
        assert!(message.starts_with("The apply did not start"), "{message}");
        assert_eq!(texts(&f.select("SELECT * FROM t WHERE id = 1")), ["1,a"]);
        assert_eq!(grid.edit().pending(), 1);
        // The apply never reached the server, but the app cannot tell: it errs toward "yes".
        assert!(f.w.in_transaction(tab));
        // Without an apply, cancel only says so.
        let effect = f.w.cancel_apply(tab);
        assert!(matches!(effect, Effect::None));
    }

    #[test]
    fn a_running_query_refuses_the_apply() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        let Effect::RunQuery { job, .. } =
            f.w.run_query(tab, RunMode::All, BufferSnapshot::at_cursor("SELECT 2", 0))
        else {
            panic!("pending edits should have refused the run");
        };
        // (They did not: the harness never told the workspace about the pending change.)
        let effect = f.w.request_apply(key, grid.edit().plan());
        assert!(matches!(effect, Effect::None));
        assert!(f.w.take_notice().unwrap().contains("query is running"));
        drop(job);
    }

    #[test]
    fn rerun_is_refused_while_edits_are_pending() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        let (model, editor) = grid.edit_parts();
        editor.delete_rows(model, &[1]).unwrap();
        f.sync(key, &grid);
        let effect =
            f.w.run_query(tab, RunMode::All, BufferSnapshot::at_cursor("SELECT 1", 0));
        assert!(matches!(effect, Effect::None));
        assert_eq!(
            f.w.take_notice().as_deref(),
            Some("Apply or discard the 2 pending change(s) first.")
        );
        // After the discard the run goes.
        grid.edit_parts().1.discard();
        f.sync(key, &grid);
        let effect =
            f.w.run_query(tab, RunMode::All, BufferSnapshot::at_cursor("SELECT 1", 0));
        assert!(matches!(effect, Effect::RunQuery { .. }), "{effect:?}");
    }

    #[test]
    fn closing_a_tab_with_pending_edits_asks_first() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        f.sync(key, &grid);
        let effect = f.w.dispatch(WorkspaceAction::CloseTab(tab));
        assert!(matches!(effect, Effect::None));
        assert_eq!(f.w.tabs().len(), 1);
        let prompt = *f.w.close_prompt().expect("a prompt");
        assert_eq!(prompt.target, CloseTarget::Tab(tab));
        assert_eq!(prompt.pending, 1);
        // Keeping the tab leaves everything as it was.
        f.w.dismiss_close();
        assert!(f.w.close_prompt().is_none());
        assert_eq!(f.w.tabs().len(), 1);
        // Closing for real closes the session as well.
        f.w.dispatch(WorkspaceAction::CloseActiveTab);
        assert!(f.w.close_prompt().is_some());
        let effect = f.w.confirm_close();
        assert!(matches!(effect, Effect::CloseSession(_)), "{effect:?}");
        assert_eq!(f.w.tabs().len(), 0);
        assert!(f.w.close_prompt().is_none());
        // A second confirm does nothing.
        assert!(matches!(f.w.confirm_close(), Effect::None));
    }

    #[test]
    fn quitting_with_pending_edits_asks_first() {
        let mut f = Flow::new(None);
        let (_, key, mut grid) = editable(&mut f);
        // Without pending edits the quit goes ahead (the open sessions are closed).
        set_cell(&mut grid, 0, 1, "A");
        f.sync(key, &grid);
        let effect = f.w.dispatch(WorkspaceAction::Quit);
        assert!(matches!(effect, Effect::None), "{effect:?}");
        let prompt = *f.w.close_prompt().expect("a prompt");
        assert_eq!(prompt.target, CloseTarget::Quit);
        assert_eq!(prompt.pending, 1);
        f.w.dismiss_close();
        assert!(f.w.tabs().len() == 1 && f.w.has_session(key.tab));
        f.w.dispatch(WorkspaceAction::Quit);
        let effect = f.w.confirm_close();
        assert!(
            matches!(effect, Effect::CloseSessions(_) | Effect::Batch(_)),
            "{effect:?}"
        );
    }

    #[test]
    fn quit_raises_the_stop_of_an_apply_in_flight() {
        let mut f = Flow::new(None);
        let (_, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        let Effect::ApplyEdits(job) = f.w.request_apply(key, grid.edit().plan()) else {
            panic!("expected a job");
        };
        f.sync(key, &grid);
        f.w.dispatch(WorkspaceAction::Quit);
        f.w.confirm_close();
        assert!(job.stop.is_cancelled());
    }

    #[test]
    fn the_resolved_schema_is_forgotten_with_a_new_run_and_a_closed_tab() {
        let mut f = Flow::new(None);
        let (tab, key, mut grid) = editable(&mut f);
        let (ctx, _) = f.w.edit_context(key).unwrap();
        assert_eq!(ctx.resolved_schema.as_deref(), Some("main"));
        let generation = f.w.edit_generation();
        // A new run replaces the grids, so their answers are dropped until the probe answers.
        let _grids = f.run(tab, "SELECT 1");
        assert!(f.w.edit_generation() > generation);
        let (ctx, _) = f.w.edit_context(key).unwrap();
        assert_eq!(
            ctx.resolved_schema, None,
            "a statement without a table has no probe"
        );
        grid.edit_parts().1.discard();
    }

    #[test]
    fn the_context_loads_the_table_once() {
        let mut f = Flow::new(None);
        let tab = f.open();
        let key = key_of(tab);
        let _grid = f.run(tab, "SELECT * FROM t");
        let (_, first) = f.w.edit_context(key).unwrap();
        assert!(matches!(first, Effect::LoadCatalog { .. }), "{first:?}");
        let (_, again) = f.w.edit_context(key).unwrap();
        assert!(matches!(again, Effect::None), "asked twice: {again:?}");
        f.pump(first);
        let (ctx, none) = f.w.edit_context(key).unwrap();
        assert!(ctx.table.is_some());
        assert!(matches!(none, Effect::None));
    }

    #[test]
    fn a_view_is_read_only_with_its_kind() {
        let mut f = Flow::new(None);
        f.exec("CREATE VIEW v AS SELECT * FROM t");
        let tab = f.open();
        let key = key_of(tab);
        let mut grid = f.run(tab, "SELECT * FROM v").remove(0);
        f.context(key, &mut grid);
        let EditState::ReadOnly(reason) = grid.edit_state() else {
            panic!("a view must be read-only");
        };
        assert!(
            reason.message().to_lowercase().contains("view"),
            "{}",
            reason.message()
        );
    }

    #[test]
    fn the_flag_is_dropped_when_the_session_is_lost_or_the_tab_closes() {
        let mut f = Flow::new(None);
        let tab = f.open();
        f.run(tab, "BEGIN");
        assert!(f.w.in_transaction(tab));
        let Effect::RunQuery { job, .. } =
            f.w.run_query(tab, RunMode::All, BufferSnapshot::at_cursor("SELECT 1", 0))
        else {
            panic!("expected a run");
        };
        // The job died with the session.
        f.w.query_lost(tab, job.run);
        assert!(!f.w.in_transaction(tab));
        assert!(!f.w.has_session(tab));

        let tab = f.open();
        f.run(tab, "BEGIN");
        assert!(f.w.in_transaction(tab));
        let effect = f.w.dispatch(WorkspaceAction::CloseTab(tab));
        f.pump(effect);
        assert!(!f.w.in_transaction(tab));
    }

    #[test]
    fn the_gate_asks_again_for_every_attempt_even_after_a_confirmed_one() {
        let mut f = Flow::new(Some(Environment::Prod));
        let (_, key, mut grid) = editable(&mut f);
        set_cell(&mut grid, 0, 1, "A");
        assert!(matches!(
            f.w.request_apply(key, grid.edit().plan()),
            Effect::None
        ));
        let Effect::ApplyEdits(job) = f.w.confirm_apply(grid.edit().plan()) else {
            panic!("expected a job");
        };
        f.run_job(key, &mut grid, job);
        // The next change asks again: a confirmation is for one plan only.
        set_cell(&mut grid, 1, 1, "B");
        // (The edited row of the first apply is read-only, so use the other row.)
        assert_eq!(grid.edit().pending(), 1);
        assert!(matches!(
            f.w.request_apply(key, grid.edit().plan()),
            Effect::None
        ));
        assert!(f.w.edit_confirm().is_some());
        assert!(!f.w.is_applying(key.tab));
    }
}
