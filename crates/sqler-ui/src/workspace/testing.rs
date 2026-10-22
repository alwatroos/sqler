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

/// Whole-flow tests of the AI session tab on a real `SQLite` file (ADR-0053). The provider is
/// always a scripted one; no network, no keychain. Every wait is capped.
#[cfg(test)]
mod ai_session_flow {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use sqler_ai_api::mock::ScriptedProvider;
    use sqler_ai_api::{
        AiError, AiProvider, Capabilities, ChatEvent, ChatRequest, ChatStream, ModelInfo,
        StopReason, ToolCall, Usage,
    };
    use sqler_ai_tools::session::{
        LimitKind, Notice, SessionState, ToolCallStatus, TranscriptEntry, TurnOutcome,
    };
    use sqler_ai_tools::{ApprovalStage, ToolError};
    use sqler_config::{
        AiConfig, ConnectionAi, ConnectionSpec, MemoryStore, ModelChoice, ProviderConfig,
        ProviderId, ProviderKind,
    };
    use sqler_driver_api::sqler_core::{ConnectionConfig, Environment, Value};
    use sqler_driver_api::{DriverRegistry, collect};
    use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
    use tokio::runtime::{Builder, Runtime};
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::oneshot;

    use super::*;
    use crate::ai::session::{
        AiSessionStart, AiSessionTab, ENTRY_GONE, HostMsg, MAX_ENTRIES, NO_TOOLS, SESSION_AI_OFF,
        SESSION_BUSY, SESSION_CONFIG_BROKEN, SESSION_EMPTY, SESSION_NO_CONNECTION,
        SESSION_NOT_BUILT, SESSION_STARTING, STALE_ANSWER, SessionPhase, TAB_CLOSED, TurnDone,
        UiCall, UiRequest, WRONG_CONNECTION, build_session, sql_footer,
    };
    use crate::ai::{AiAction, AiProviderFactory, AiStartup, AiState, NoProviders};
    use crate::query::{Phase, QueryTab, RunMode};
    use crate::sidebar::SidebarModel;
    use crate::tabs::{TabId, TabKind};
    use crate::workbench::SideView;
    use crate::workspace::{Effect, SessionBridge, Workspace, WorkspaceAction};

    const WAIT: Duration = Duration::from_secs(10);
    static FILES: AtomicUsize = AtomicUsize::new(0);
    const UPDATE: &str = "update customers set country = 'GB' where id = 2";
    /// A plain read that takes minutes: a cross join of the table with itself.
    const SLOW: &str = "select count(*) from customers a, customers b, customers c, customers d, \
        customers e, customers f, customers g, customers h, customers i, customers j, customers k";

    struct TempDb(PathBuf);

    impl TempDb {
        fn new() -> Self {
            let n = FILES.fetch_add(1, Ordering::SeqCst);
            Self(std::env::temp_dir().join(format!("sqler-ai-flow-{}-{n}.db", std::process::id())))
        }

        fn path(&self) -> String {
            self.0.display().to_string()
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

    async fn open_sqlite(path: &str) -> Box<dyn Session> {
        let mut registry = DriverRegistry::new();
        registry.register(Box::new(SqliteDriver::new())).unwrap();
        let config = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), path);
        registry.connect(&config).await.unwrap()
    }

    async fn rows_of(path: &str, sql: &str) -> Vec<Vec<Value>> {
        let mut session = open_sqlite(path).await;
        let q = Query::new(sql);
        let mut stream = session.execute(&q).await.unwrap();
        let result = collect(&mut *stream).await.unwrap();
        let rows = result.rows().iter().map(|r| r.0.clone()).collect();
        drop(stream);
        session.close().await.unwrap();
        rows
    }

    /// Counts when it is dropped, which happens when a session is closed or dropped.
    struct Released(Arc<AtomicUsize>);

    impl Drop for Released {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A real session that counts its release (a `close`, or a drop when a cancelled tool call is
    /// abandoned by the engine).
    struct Counted {
        inner: Box<dyn Session>,
        _released: Released,
    }

    impl Session for Counted {
        fn cancel_handle(&self) -> CancelHandle {
            self.inner.cancel_handle()
        }

        fn execute<'s>(
            &'s mut self,
            query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            self.inner.execute(query)
        }

        fn introspection(&mut self) -> Option<&mut dyn sqler_driver_api::Introspect> {
            self.inner.introspection()
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            self.inner.close()
        }
    }

    /// Opens real `SQLite` sessions on the target of the request; counts the releases.
    struct SqliteConnector {
        closes: Arc<AtomicUsize>,
    }

    impl Connector for SqliteConnector {
        fn driver_ids(&self) -> Vec<DriverId> {
            vec![DriverId::new(DRIVER_ID).unwrap()]
        }

        fn connect(
            &self,
            request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            let closes = Arc::clone(&self.closes);
            Box::pin(async move {
                let inner = open_sqlite(&request.spec.target).await;
                Ok(Box::new(Counted {
                    inner,
                    _released: Released(closes),
                }) as Box<dyn Session>)
            })
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    /// Hands out one provider.
    struct Fixed(Arc<dyn AiProvider>);

    impl AiProviderFactory for Fixed {
        fn kinds(&self) -> Vec<ProviderKind> {
            vec![ProviderKind::Ollama]
        }

        fn build(
            &self,
            _provider: &ProviderConfig,
            _key: Option<&sqler_config::Secret>,
        ) -> Result<Arc<dyn AiProvider>, AiError> {
            Ok(Arc::clone(&self.0))
        }
    }

    /// A provider whose stream never ends; counts the open streams.
    struct StallProvider {
        open: Arc<AtomicUsize>,
    }

    struct StallStream(Arc<AtomicUsize>);

    impl ChatStream for StallStream {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>> {
            Box::pin(std::future::pending())
        }
    }

    impl Drop for StallStream {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl AiProvider for StallProvider {
        fn id(&self) -> &'static str {
            "stall"
        }

        fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
            Box::pin(std::future::ready(Ok(Vec::new())))
        }

        fn stream(&self, _request: ChatRequest) -> Box<dyn ChatStream + '_> {
            self.open.fetch_add(1, Ordering::SeqCst);
            Box::new(StallStream(Arc::clone(&self.open)))
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::new(true, false, false, None)
        }
    }

    type Script = Vec<Result<ChatEvent, AiError>>;

    fn text(t: &str) -> Script {
        vec![
            Ok(ChatEvent::TextDelta(t.to_owned())),
            Ok(ChatEvent::Usage(Usage::new(10, 5))),
            Ok(ChatEvent::Done {
                stop: StopReason::EndTurn,
            }),
        ]
    }

    fn call(id: &str, tool: &str, args: &serde_json::Value) -> Script {
        vec![
            Ok(ChatEvent::ToolCall(ToolCall::new(
                id,
                tool,
                args.to_string(),
            ))),
            Ok(ChatEvent::Usage(Usage::new(10, 5))),
            Ok(ChatEvent::Done {
                stop: StopReason::ToolUse,
            }),
        ]
    }

    fn run_query(id: &str, sql: &str) -> Script {
        call(
            id,
            "run_query",
            &serde_json::json!({"connection": "db", "sql": sql}),
        )
    }

    fn scripted() -> Arc<ScriptedProvider> {
        Arc::new(
            ScriptedProvider::new("mock")
                .with_capabilities(Capabilities::new(true, false, false, None)),
        )
    }

    fn config() -> AiConfig {
        let mut config = AiConfig::default();
        let id = ProviderId::new("local").unwrap();
        config
            .providers
            .push(ProviderConfig::new(id.clone(), ProviderKind::Ollama).with_models(["m"]));
        config.models.exploration = Some(ModelChoice::new(id, "m"));
        config
    }

    /// What the effects of the flow did, in order.
    #[derive(Debug, Clone, PartialEq)]
    enum Seen {
        Connect { seed: Option<String> },
        Turn,
        CancelQuery,
        CloseSession,
        CloseSessions(usize),
        OpenData { tab: TabId, sql: String },
        Run { sql: String },
        Quit,
    }

    /// The workspace with one real connection, standing in for the view.
    struct Chat {
        rt: Runtime,
        bridge: SessionBridge,
        w: Workspace,
        db: TempDb,
        factory: Arc<Fixed>,
        host: Option<UnboundedReceiver<HostMsg>>,
        tab: Option<TabId>,
        job: Option<tokio::task::JoinHandle<()>>,
        seen: Vec<Seen>,
        asks: usize,
        workers: Vec<oneshot::Receiver<()>>,
        closes: Arc<AtomicUsize>,
    }

    impl Chat {
        fn new(env: Option<Environment>, provider: Arc<dyn AiProvider>) -> Self {
            Self::with(
                env,
                provider,
                AiState::new(config(), vec![ProviderKind::Ollama], None),
                true,
            )
        }

        fn with(
            env: Option<Environment>,
            provider: Arc<dyn AiProvider>,
            ai: AiState,
            ai_on: bool,
        ) -> Self {
            let rt = Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let db = TempDb::new();
            rt.block_on(async {
                let mut s = open_sqlite(&db.path()).await;
                for sql in [
                    "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT, country TEXT)",
                    "INSERT INTO customers VALUES (1,'Acme','US'),(2,'Contoso','DE'),\
                     (3,'Globex','FR'),(4,'Initech','US'),(5,'Umbrella','UK'),\
                     (6,'Hooli','US'),(7,'Soylent','NL'),(8,'Stark','US')",
                ] {
                    let q = Query::new(sql);
                    let mut stream = s.execute(&q).await.unwrap();
                    collect(&mut *stream).await.unwrap();
                }
                s.close().await.unwrap();
            });
            let closes = Arc::new(AtomicUsize::new(0));
            let bridge = SessionBridge::new(
                Arc::new(SqliteConnector {
                    closes: Arc::clone(&closes),
                }),
                Arc::new(MemoryStore::new()),
                rt.handle().clone(),
            );
            let mut spec = ConnectionSpec::new("db", DriverId::new(DRIVER_ID).unwrap(), db.path())
                .with_ai(ConnectionAi::new(ai_on));
            if let Some(env) = env {
                spec = spec.with_environment(env);
            }
            let mut sidebar = SidebarModel::default();
            sidebar.add_connection(spec, None).unwrap();
            Self {
                rt,
                bridge,
                w: Workspace::new(sidebar).with_ai(ai),
                db,
                factory: Arc::new(Fixed(provider)),
                host: None,
                tab: None,
                job: None,
                seen: Vec::new(),
                asks: 0,
                workers: Vec::new(),
                closes,
            }
        }

        fn pump(&mut self, effect: Effect) {
            match effect {
                Effect::None | Effect::LoadCatalog { .. } | Effect::RecordHistory(_) => {}
                Effect::Batch(effects) => effects.into_iter().for_each(|e| self.pump(e)),
                Effect::Connect { request, connect } => {
                    let seed = self.w.editor_seeds.get(&request).cloned();
                    self.seen.push(Seen::Connect { seed });
                    let result = self.rt.block_on(self.bridge.connect(connect));
                    let next = self.w.connect_finished(request, result);
                    self.pump(next);
                }
                Effect::StartCatalog { worker, .. } => {
                    let done = self.bridge.run_catalog(worker);
                    self.workers.push(done);
                }
                Effect::StartAiSession { start, host } => {
                    let tab = start.tab;
                    let built = self.rt.block_on(build_session(
                        *start,
                        self.factory.clone(),
                        self.bridge.connector(),
                    ));
                    self.host = Some(host);
                    self.tab = Some(tab);
                    let next = self.w.ai_session_ready(tab, built);
                    self.pump(next);
                }
                Effect::RunAiTurn(job) => {
                    self.seen.push(Seen::Turn);
                    self.job = Some(self.rt.spawn(job.run()));
                }
                Effect::CancelQuery(handle) => {
                    self.seen.push(Seen::CancelQuery);
                    let _ = self.rt.block_on(handle.cancel());
                }
                Effect::CloseSession(session) => {
                    self.seen.push(Seen::CloseSession);
                    let _ = self.rt.block_on(session.close());
                }
                Effect::CloseSessions(sessions) => {
                    self.seen.push(Seen::CloseSessions(sessions.len()));
                    for session in sessions {
                        let _ = self.rt.block_on(session.close());
                    }
                }
                Effect::OpenData { tab, job, .. } => {
                    self.seen.push(Seen::OpenData { tab, sql: job.sql });
                }
                Effect::RunQuery {
                    job, mut events, ..
                } => {
                    self.seen.push(Seen::Run {
                        sql: job.statements[0].sql.clone(),
                    });
                    let (tab, run) = (job.tab, job.run);
                    let done = self.rt.block_on(job.run());
                    while let Ok(event) = events.try_recv() {
                        let next = self.w.query_event(tab, run, event);
                        self.pump(next);
                    }
                    let next = self.w.query_finished(done);
                    self.pump(next);
                }
                Effect::Quit => self.seen.push(Seen::Quit),
                other => panic!("unexpected effect {other:?}"),
            }
        }

        fn tab(&self) -> TabId {
            self.tab.expect("a session tab")
        }

        fn session(&self) -> &AiSessionTab {
            self.w.ai_session(self.tab()).expect("the session tab")
        }

        /// Opens the connection (a query tab, with its browser) and an AI session tab.
        fn open_session(&mut self) -> TabId {
            let effect = self.w.dispatch(WorkspaceAction::OpenConnection(0));
            self.pump(effect);
            let effect = self.w.dispatch_ai(AiAction::OpenSession);
            self.pump(effect);
            self.tab()
        }

        fn say(&mut self, text: &str) {
            let tab = self.tab();
            self.w
                .dispatch_ai(AiAction::SetSessionDraft(tab, text.to_owned()));
            let effect = self.w.dispatch_ai(AiAction::SendMessage(tab));
            self.pump(effect);
        }

        fn recv(&mut self) -> HostMsg {
            let host = self.host.as_mut().expect("a host channel");
            self.rt
                .block_on(async { tokio::time::timeout(WAIT, host.recv()).await })
                .expect("timed out waiting for the session")
                .expect("the host channel closed")
        }

        /// Applies host messages until `done` holds. The wait has a deadline.
        fn until(&mut self, what: &str, done: impl Fn(&Workspace, TabId) -> bool) {
            let tab = self.tab();
            let deadline = Instant::now() + WAIT;
            while !done(&self.w, tab) {
                assert!(Instant::now() < deadline, "timed out: {what}");
                let message = self.host.as_mut().expect("a host channel").try_recv();
                match message {
                    Ok(message) => {
                        if matches!(message, HostMsg::Ask(_)) {
                            self.asks += 1;
                        }
                        let next = self.w.ai_session_message(tab, message);
                        self.pump(next);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }

        fn until_ready(&mut self) {
            self.until("the session is ready", |w, t| {
                w.ai_session(t)
                    .is_some_and(|s| *s.phase() == SessionPhase::Ready)
            });
        }

        /// The id of the ask at the front of the queue.
        fn front(&self) -> u64 {
            self.session()
                .approvals()
                .current()
                .expect("an ask waits")
                .id
        }

        /// Approves the front ask by its id, as the card's button does.
        fn approve(&mut self, tab: TabId) -> Effect {
            let id = self.front();
            self.w.dispatch_ai(AiAction::ApproveStatement(tab, id))
        }

        /// Denies the front ask by its id, as the card's button does.
        fn deny(&mut self, tab: TabId) -> Effect {
            let id = self.front();
            self.w.dispatch_ai(AiAction::DenyStatement(tab, id))
        }

        fn until_asked(&mut self) {
            self.until("an approval waits", |w, t| {
                w.ai_session(t).is_some_and(|s| s.approvals().is_waiting())
            });
        }

        fn ask(&mut self, text: &str) {
            self.say(text);
            self.until_ready();
        }

        /// Waits for the turn job; messages that are left are applied (they are dropped when the
        /// tab is gone).
        fn finish_job(&mut self) {
            if let Some(job) = self.job.take() {
                self.rt
                    .block_on(async { tokio::time::timeout(WAIT, job).await })
                    .expect("the turn did not end")
                    .expect("the turn panicked");
            }
            let tab = self.tab();
            let mut rest = Vec::new();
            if let Some(host) = self.host.as_mut() {
                while let Ok(message) = host.try_recv() {
                    rest.push(message);
                }
            }
            for message in rest {
                let next = self.w.ai_session_message(tab, message);
                self.pump(next);
            }
        }

        fn country(&self, id: i64) -> String {
            let rows = self.rt.block_on(rows_of(
                &self.db.path(),
                &format!("select country from customers where id = {id}"),
            ));
            match &rows[0][0] {
                Value::Text(t) => t.clone(),
                other => panic!("{other:?}"),
            }
        }

        fn entries(&self) -> &[TranscriptEntry] {
            self.session().entries()
        }

        fn sqls(&self) -> Vec<&sqler_ai_tools::session::SqlEntry> {
            self.entries()
                .iter()
                .filter_map(|e| match e {
                    TranscriptEntry::Sql(s) => Some(s),
                    _ => None,
                })
                .collect()
        }

        fn tool_statuses(&self) -> Vec<ToolCallStatus> {
            self.entries()
                .iter()
                .filter_map(|e| match e {
                    TranscriptEntry::ToolCall(c) => Some(c.status.clone()),
                    _ => None,
                })
                .collect()
        }

        fn notice(&mut self) -> Option<String> {
            self.w.take_notice()
        }
    }

    fn denied(status: &ToolCallStatus) -> bool {
        matches!(status, ToolCallStatus::Error { code, .. } if code == "denied")
    }

    fn assistant_texts(chat: &Chat) -> Vec<String> {
        chat.entries()
            .iter()
            .filter_map(|e| match e {
                TranscriptEntry::Assistant { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_multi_step_turn_lists_queries_and_answers() {
        let p = scripted();
        p.push_script(call(
            "c1",
            "list_tables",
            &serde_json::json!({"connection": "db"}),
        ));
        p.push_script(run_query(
            "c2",
            "select id, name from customers order by id",
        ));
        p.push_script(text("There are eight customers."));
        let mut chat = Chat::new(None, p.clone());
        let tab = chat.open_session();
        assert_eq!(chat.session().phase(), &SessionPhase::Ready);
        assert_eq!(chat.w.tabs().get(tab).unwrap().kind(), TabKind::AiSession);
        assert_eq!(chat.w.tabs().get(tab).unwrap().title(), "AI: db (2)");
        assert_eq!(
            chat.w.breadcrumbs(tab).unwrap().items,
            ["db".to_owned(), "AI session".to_owned()]
        );
        chat.say("How many customers?");
        assert!(chat.session().is_running());
        assert_eq!(chat.session().draft(), "");
        chat.until_ready();
        let entries = chat.entries();
        assert!(
            matches!(&entries[0], TranscriptEntry::User { text } if text == "How many customers?")
        );
        assert_eq!(
            chat.tool_statuses(),
            [ToolCallStatus::Ok, ToolCallStatus::Ok]
        );
        let sqls = chat.sqls();
        assert_eq!(sqls.len(), 1);
        assert_eq!(sqls[0].rows, 8);
        assert!(!sqls[0].approved);
        assert_eq!(sql_footer(sqls[0]), "8 rows");
        assert_eq!(assistant_texts(&chat), ["There are eight customers."]);
        assert_eq!(chat.session().streaming(), "");
        assert_eq!(chat.asks, 0);
        assert!(chat.session().session.is_some());
        // The turn reached the model with the connection's tools.
        assert_eq!(p.requests().len(), 3);
        // A second turn works on the same session.
        p.push_script(text("Anything else?"));
        chat.ask("thanks");
        assert_eq!(assistant_texts(&chat).len(), 2);
    }

    #[test]
    fn sending_is_refused_while_busy_empty_or_starting() {
        let p = scripted();
        p.push_script(text("ok"));
        let mut chat = Chat::new(None, p);
        chat.open_session();
        let tab = chat.tab();
        // Empty draft.
        let effect = chat.w.dispatch_ai(AiAction::SendMessage(tab));
        assert!(matches!(effect, Effect::None));
        assert_eq!(chat.notice().as_deref(), Some(SESSION_EMPTY));
        chat.say("hello");
        // Busy.
        chat.w
            .dispatch_ai(AiAction::SetSessionDraft(tab, "again".into()));
        assert!(matches!(
            chat.w.dispatch_ai(AiAction::SendMessage(tab)),
            Effect::None
        ));
        assert_eq!(chat.notice().as_deref(), Some(SESSION_BUSY));
        assert_eq!(chat.session().draft(), "again");
        chat.until_ready();
        // Turning AI off for the connection blocks the next send.
        chat.w.dispatch_ai(AiAction::ToggleConnection);
        let effect = chat.w.dispatch_ai(AiAction::SendMessage(tab));
        assert!(matches!(effect, Effect::None));
        assert_eq!(chat.notice().as_deref(), Some(SESSION_AI_OFF));
    }

    #[test]
    fn a_session_that_is_still_starting_refuses_a_message() {
        let mut chat = Chat::new(None, scripted());
        let effect = chat.w.dispatch_ai(AiAction::OpenSession);
        // Do not build: keep the start effect.
        let Effect::Batch(mut effects) = effect else {
            panic!("expected a batch");
        };
        let start = effects.pop().unwrap();
        let Effect::StartAiSession { start, host } = start else {
            panic!("expected StartAiSession");
        };
        let tab = start.tab;
        assert_eq!(
            chat.w.ai_session(tab).unwrap().phase(),
            &SessionPhase::Starting
        );
        chat.w
            .dispatch_ai(AiAction::SetSessionDraft(tab, "hi".into()));
        assert!(matches!(
            chat.w.dispatch_ai(AiAction::SendMessage(tab)),
            Effect::None
        ));
        assert_eq!(chat.notice().as_deref(), Some(SESSION_STARTING));
        // A session built for a tab that was closed meanwhile is dropped.
        let built = chat.rt.block_on(build_session(
            *start,
            chat.factory.clone(),
            chat.bridge.connector(),
        ));
        let effect = chat.w.dispatch(WorkspaceAction::CloseTab(tab));
        chat.pump(effect);
        let effect = chat.w.ai_session_ready(tab, built);
        chat.pump(effect);
        assert!(chat.w.ai_session(tab).is_none());
        drop(host);
    }

    #[test]
    fn a_model_without_tools_fails_the_session() {
        let p = Arc::new(ScriptedProvider::new("plain"));
        let mut chat = Chat::new(None, p);
        chat.open_session();
        assert_eq!(
            chat.session().phase(),
            &SessionPhase::Failed(NO_TOOLS.to_owned())
        );
        assert_eq!(chat.session().label(), "Failed");
        chat.w
            .dispatch_ai(AiAction::SetSessionDraft(chat.tab(), "hi".into()));
        let effect = chat.w.dispatch_ai(AiAction::SendMessage(chat.tab()));
        assert!(matches!(effect, Effect::None));
        assert_eq!(chat.notice().as_deref(), Some(NO_TOOLS));
    }

    #[test]
    fn an_approved_write_runs_and_a_denied_one_does_not() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("Contoso is now in the UK."));
        let mut chat = Chat::new(Some(Environment::Dev), p.clone());
        chat.open_session();
        chat.say("Move Contoso to GB");
        chat.until_asked();
        let tab = chat.tab();
        {
            let pending = chat.session().approvals().current().unwrap();
            assert_eq!(pending.request.stage, ApprovalStage::Confirm);
            assert_eq!(pending.request.sql, UPDATE);
            assert_eq!(
                crate::ai::card_title(&pending.request),
                "Approve statement 1 of 1 \u{b7} write"
            );
        }
        assert_eq!(chat.session().label(), "Waiting for your approval");
        assert_eq!(chat.country(2), "DE", "nothing runs before the answer");
        chat.approve(tab);
        chat.until_ready();
        assert_eq!(chat.country(2), "GB");
        let sqls = chat.sqls();
        assert_eq!(sqls.len(), 1);
        assert!(sqls[0].approved);
        assert!(sql_footer(sqls[0]).ends_with("approved"));
        assert_eq!(assistant_texts(&chat).len(), 1);

        // Deny the next one.
        p.push_script(run_query(
            "c2",
            "update customers set country = 'XX' where id = 3",
        ));
        p.push_script(text("Understood, I left it."));
        chat.say("and Globex");
        chat.until_asked();
        chat.deny(tab);
        chat.until_ready();
        assert_eq!(chat.country(3), "FR");
        assert!(chat.tool_statuses().iter().any(denied));
        assert_eq!(chat.sqls().len(), 1);
        assert_eq!(
            assistant_texts(&chat).last().unwrap(),
            "Understood, I left it."
        );
    }

    const UPDATE_B: &str = "update customers set country = 'XX' where id = 3";

    #[test]
    fn a_stale_click_after_the_queue_advanced_approves_nothing() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("one"));
        let mut chat = Chat::new(Some(Environment::Dev), p.clone());
        let tab = chat.open_session();
        chat.say("first");
        chat.until_asked();
        let first = chat.front();
        chat.approve(tab);
        chat.until_ready();
        assert_eq!(chat.country(2), "GB");

        p.push_script(run_query("c2", UPDATE_B));
        p.push_script(text("two"));
        chat.say("second");
        chat.until_asked();
        assert_ne!(chat.front(), first);
        // The click on the first card lands now.
        let effect = chat.w.dispatch_ai(AiAction::ApproveStatement(tab, first));
        assert!(matches!(effect, Effect::None));
        assert!(
            chat.session().approvals().is_waiting(),
            "the ask still waits"
        );
        assert_eq!(chat.country(3), "FR", "the second statement did not run");
        assert!(chat.w.take_notice().is_some_and(|n| n == STALE_ANSWER));
        chat.w.dispatch_ai(AiAction::DenyStatement(tab, first));
        assert!(chat.session().approvals().is_waiting());
        chat.deny(tab);
        chat.until_ready();
        assert_eq!(chat.country(3), "FR");
    }

    #[test]
    fn the_palette_answers_only_an_ask_whose_card_was_shown() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("done"));
        let mut chat = Chat::new(Some(Environment::Dev), p);
        let tab = chat.open_session();
        chat.say("go");
        chat.until_asked();
        let approve = crate::command::CommandId::new("ai:approve-statement").unwrap();
        let deny = crate::command::CommandId::new("ai:deny-statement").unwrap();
        // The view has not drawn the card yet.
        assert_eq!(crate::ai::action_for(&approve, &chat.w), None);
        assert_eq!(crate::ai::action_for(&deny, &chat.w), None);
        assert_eq!(chat.country(2), "DE");
        // The view draws it.
        let id = chat.front();
        chat.session().approvals().mark_shown(id);
        assert_eq!(
            crate::ai::action_for(&approve, &chat.w),
            Some(AiAction::ApproveStatement(tab, id))
        );
        chat.w.dispatch_ai(AiAction::ApproveStatement(tab, id));
        chat.until_ready();
        assert_eq!(chat.country(2), "GB");
    }

    #[test]
    fn a_duplicate_answer_to_one_ask_is_ignored() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("done"));
        let mut chat = Chat::new(Some(Environment::Dev), p);
        let tab = chat.open_session();
        chat.say("go");
        chat.until_asked();
        let id = chat.front();
        chat.w.dispatch_ai(AiAction::DenyStatement(tab, id));
        // Approving the same, already answered ask changes nothing.
        chat.w.dispatch_ai(AiAction::ApproveStatement(tab, id));
        chat.until_ready();
        assert_eq!(chat.country(2), "DE");
        assert_eq!(chat.sqls().len(), 0);
    }

    #[test]
    fn a_rerun_is_keyed_by_a_stable_entry_number() {
        let p = scripted();
        p.push_script(run_query("c1", "select name from customers where id = 1"));
        p.push_script(text("Acme"));
        let mut chat = Chat::new(Some(Environment::Dev), p);
        let tab = chat.open_session();
        chat.ask("who is 1?");
        let no = chat.session().dropped()
            + chat
                .entries()
                .iter()
                .position(|e| matches!(e, TranscriptEntry::Sql(_)))
                .unwrap();
        // The turn ended and replaced the entries by the engine transcript; the number still
        // names the same statement.
        let TranscriptEntry::Sql(entry) = chat.session().entry(no).unwrap() else {
            panic!("not a statement");
        };
        assert_eq!(entry.rerun().sql, "select name from customers where id = 1");
        // Entries that were dropped are gone, not shifted.
        chat.w.ai_sessions.tabs.get_mut(&tab).unwrap().dropped = no + 1;
        assert!(chat.session().entry(no).is_none());
        assert!(matches!(
            chat.w.dispatch_ai(AiAction::RerunSql(tab, no)),
            Effect::None
        ));
        assert_eq!(chat.w.take_notice().as_deref(), Some(ENTRY_GONE));
    }

    #[test]
    fn the_transcript_is_capped_and_numbers_stay_stable() {
        let p = scripted();
        let mut chat = Chat::new(Some(Environment::Dev), p);
        let tab = chat.open_session();
        let many: Vec<TranscriptEntry> = (0..MAX_ENTRIES + 10)
            .map(|i| TranscriptEntry::User {
                text: format!("m{i}"),
            })
            .collect();
        chat.w
            .ai_sessions
            .tabs
            .get_mut(&tab)
            .unwrap()
            .set_entries(&many);
        let t = chat.session();
        assert_eq!(t.entries().len(), MAX_ENTRIES);
        assert_eq!(t.dropped(), 10);
        assert_eq!(
            t.entry(10),
            Some(&TranscriptEntry::User { text: "m10".into() })
        );
        assert!(t.entry(9).is_none());
        chat.w
            .ai_sessions
            .tabs
            .get_mut(&tab)
            .unwrap()
            .push_entry(TranscriptEntry::User {
                text: "last".into(),
            });
        let t = chat.session();
        assert_eq!(t.entries().len(), MAX_ENTRIES);
        assert_eq!(t.dropped(), 11);
        assert_eq!(
            t.entry(MAX_ENTRIES + 10),
            Some(&TranscriptEntry::User {
                text: "last".into()
            })
        );
    }

    #[test]
    fn a_stray_prod_confirm_does_not_answer_the_card() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("done"));
        let mut chat = Chat::new(Some(Environment::Dev), p);
        chat.open_session();
        chat.say("go");
        chat.until_asked();
        let tab = chat.tab();
        // The gate is idle: confirm and dismiss do nothing to the waiting card.
        assert!(matches!(chat.w.confirm_query(tab), Effect::None));
        chat.w.dismiss_query_prompt(tab);
        assert!(chat.session().approvals().is_waiting());
        assert_eq!(chat.country(2), "DE");
        chat.deny(tab);
        chat.until_ready();
    }

    #[test]
    fn production_adds_the_gate_as_a_second_confirmation() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("done"));
        let mut chat = Chat::new(Some(Environment::Prod), p.clone());
        chat.open_session();
        chat.say("Move Contoso");
        chat.until_asked();
        let tab = chat.tab();
        assert_eq!(
            chat.session().approvals().current().unwrap().request.stage,
            ApprovalStage::Confirm
        );
        assert_eq!(chat.w.query_tab(tab).unwrap().phase(), &Phase::Idle);
        chat.approve(tab);
        // The second stage opens the ADR-0024 gate with the exact SQL.
        chat.until("the gate opens", |w, t| {
            matches!(
                w.query_tab(t).map(QueryTab::phase),
                Some(Phase::Confirming(_))
            )
        });
        {
            let Phase::Confirming(list) = chat.w.query_tab(tab).unwrap().phase() else {
                unreachable!()
            };
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].sql, UPDATE);
        }
        assert_eq!(
            chat.session().approvals().current().unwrap().request.stage,
            ApprovalStage::ProdSecond
        );
        // The card commands do not answer the second stage.
        chat.approve(tab);
        assert!(chat.session().approvals().is_waiting());
        assert_eq!(chat.country(2), "DE");
        assert!(matches!(chat.w.confirm_query(tab), Effect::None));
        assert_eq!(chat.w.query_tab(tab).unwrap().phase(), &Phase::Idle);
        chat.until_ready();
        assert_eq!(chat.country(2), "GB");
        assert!(chat.sqls()[0].approved);

        // Dismissing the gate denies.
        p.push_script(run_query(
            "c2",
            "update customers set country = 'XX' where id = 4",
        ));
        p.push_script(text("ok, not run"));
        chat.say("Initech");
        chat.until_asked();
        chat.approve(tab);
        chat.until("the gate opens", |w, t| {
            matches!(
                w.query_tab(t).map(QueryTab::phase),
                Some(Phase::Confirming(_))
            )
        });
        chat.w.dismiss_query_prompt(tab);
        chat.until_ready();
        assert_eq!(chat.country(4), "US");
        assert!(chat.tool_statuses().iter().any(denied));
    }

    #[test]
    fn a_read_on_production_asks_nothing() {
        let p = scripted();
        p.push_script(run_query("c1", "select count(*) from customers"));
        p.push_script(text("eight"));
        let mut chat = Chat::new(Some(Environment::Prod), p);
        chat.open_session();
        chat.ask("count");
        assert_eq!(chat.asks, 0);
        assert_eq!(chat.sqls().len(), 1);
        assert_eq!(chat.w.query_tab(chat.tab()).unwrap().phase(), &Phase::Idle);
    }

    #[test]
    fn the_ui_tools_open_an_editor_and_a_results_tab() {
        let p = scripted();
        p.push_script(call(
            "c1",
            "open_in_editor",
            &serde_json::json!({"sql": "select 1 -- seed"}),
        ));
        p.push_script(call(
            "c2",
            "open_results_tab",
            &serde_json::json!({"connection": "db", "sql": "select * from customers", "title": "Everyone"}),
        ));
        p.push_script(call(
            "c3",
            "open_results_tab",
            &serde_json::json!({"connection": "db", "sql": UPDATE}),
        ));
        p.push_script(text("Opened."));
        let mut chat = Chat::new(None, p);
        chat.open_session();
        let before = chat.seen.len();
        chat.ask("show me everyone");
        let seen = chat.seen[before..].to_vec();
        assert!(seen.contains(&Seen::Connect {
            seed: Some("select 1 -- seed".into())
        }));
        let Some(Seen::OpenData { tab, sql }) = seen
            .iter()
            .find(|s| matches!(s, Seen::OpenData { .. }))
            .cloned()
        else {
            panic!("no OpenData in {seen:?}");
        };
        assert_eq!(sql, "select * from customers");
        let opened = chat.w.tabs().get(tab).unwrap();
        assert_eq!(opened.kind(), TabKind::Grid);
        assert_eq!(opened.title(), "Results: Everyone");
        assert_eq!(opened.mode(), crate::tabs::TabMode::Pinned);
        assert!(chat.w.grid_cancels.contains_key(&tab));
        assert_eq!(
            seen.iter()
                .filter(|s| matches!(s, Seen::OpenData { .. }))
                .count(),
            1,
            "the write is refused by the tool set"
        );
        let statuses = chat.tool_statuses();
        assert_eq!(statuses.len(), 3);
        assert_eq!(statuses[0], ToolCallStatus::Ok);
        assert_eq!(statuses[1], ToolCallStatus::Ok);
        assert!(matches!(statuses[2], ToolCallStatus::Error { .. }));
        assert_eq!(chat.country(2), "DE");
    }

    #[test]
    fn ui_calls_for_another_connection_or_a_gone_tab_are_errors() {
        let mut chat = Chat::new(None, scripted());
        let tab = chat.open_session();
        let ask = |chat: &mut Chat, request: UiRequest, tab: TabId| {
            let (reply, answer) = oneshot::channel();
            let effect = chat
                .w
                .ai_session_message(tab, HostMsg::Ui(UiCall { request, reply }));
            (effect, answer.blocking_recv().unwrap())
        };
        let (effect, result) = ask(
            &mut chat,
            UiRequest::OpenInEditor {
                connection: Some("other".into()),
                sql: "select 1".into(),
            },
            tab,
        );
        assert!(matches!(effect, Effect::None));
        assert!(matches!(result, Err(ToolError::Host(m)) if m == WRONG_CONNECTION));
        let (effect, result) = ask(
            &mut chat,
            UiRequest::OpenResults {
                connection: "other".into(),
                sql: "select 1".into(),
                title: None,
            },
            tab,
        );
        assert!(matches!(effect, Effect::None));
        assert!(matches!(result, Err(ToolError::Host(m)) if m == WRONG_CONNECTION));
        // Without a title the results tab gets a default one.
        let id = chat.session().info().id.clone();
        let (effect, result) = ask(
            &mut chat,
            UiRequest::OpenResults {
                connection: id,
                sql: "select 1".into(),
                title: None,
            },
            tab,
        );
        assert!(result.is_ok());
        let Effect::OpenData { tab: grid, .. } = effect else {
            panic!("expected OpenData");
        };
        assert_eq!(
            chat.w.tabs().get(grid).unwrap().title(),
            "Results: AI query"
        );
        // A closed tab answers with an error.
        let effect = chat.w.dispatch(WorkspaceAction::CloseTab(tab));
        chat.pump(effect);
        let (effect, result) = ask(
            &mut chat,
            UiRequest::OpenInEditor {
                connection: None,
                sql: "select 1".into(),
            },
            tab,
        );
        assert!(matches!(effect, Effect::None));
        assert!(matches!(result, Err(ToolError::Host(m)) if m == TAB_CLOSED));
    }

    #[test]
    fn cancel_stops_the_stream_and_a_stale_turn_is_ignored() {
        let open = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(StallProvider {
            open: Arc::clone(&open),
        });
        let mut chat = Chat::new(None, p);
        let tab = chat.open_session();
        // Nothing runs: cancel does nothing.
        assert!(matches!(
            chat.w.dispatch_ai(AiAction::CancelSession(tab)),
            Effect::None
        ));
        chat.say("think forever");
        chat.until("the model is thinking", |_, _| {
            open.load(Ordering::SeqCst) == 1
        });
        chat.until("the state shows thinking", |w, t| {
            w.ai_session(t)
                .is_some_and(|s| s.state() == SessionState::Streaming)
        });
        assert_eq!(chat.session().label(), "Thinking\u{2026}");
        let effect = chat.w.dispatch_ai(AiAction::CancelSession(tab));
        assert!(matches!(effect, Effect::None), "no statement was in flight");
        chat.until_ready();
        assert_eq!(chat.session().state(), SessionState::Cancelled);
        assert!(
            chat.entries()
                .contains(&TranscriptEntry::Notice(Notice::Cancelled))
        );
        assert_eq!(open.load(Ordering::SeqCst), 0);
        let entries = chat.entries().to_vec();

        // A TurnDone of an older turn changes nothing.
        let stale = chat.rt.block_on(async {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let start = AiSessionStart {
                tab,
                provider: config().providers[0].clone(),
                model: "m".into(),
                info: chat.session().info().clone(),
                request: chat.w.request_for(chat.session().conn()).unwrap(),
                config: config(),
                audit: None,
                tx,
            };
            build_session(start, chat.factory.clone(), chat.bridge.connector())
                .await
                .unwrap()
        });
        let effect = chat.w.ai_session_message(
            tab,
            HostMsg::TurnDone(Box::new(TurnDone {
                turn: 99,
                session: stale.session,
                outcome: TurnOutcome::Completed,
            })),
        );
        assert!(matches!(effect, Effect::None));
        assert_eq!(chat.entries(), entries.as_slice());
        assert_eq!(chat.session().phase(), &SessionPhase::Ready);
    }

    #[test]
    fn cancel_while_a_statement_runs_asks_the_database_to_stop() {
        let p = scripted();
        p.push_script(run_query("c1", SLOW));
        let mut chat = Chat::new(None, p);
        let tab = chat.open_session();
        chat.say("count forever");
        chat.until("a statement is in flight", |w, t| {
            w.ai_session(t).is_some_and(|s| s.in_flight().is_some())
        });
        let effect = chat.w.dispatch_ai(AiAction::CancelSession(tab));
        assert!(matches!(effect, Effect::CancelQuery(_)), "{effect:?}");
        chat.pump(effect);
        chat.until_ready();
        let statuses = chat.tool_statuses();
        assert_eq!(statuses, vec![ToolCallStatus::Cancelled], "{statuses:?}");
        assert!(
            chat.entries()
                .contains(&TranscriptEntry::Notice(Notice::Cancelled))
        );
    }

    #[test]
    fn closing_mid_statement_cancels_it_and_ends_the_turn() {
        let p = scripted();
        p.push_script(run_query("c1", SLOW));
        let mut chat = Chat::new(None, p);
        let tab = chat.open_session();
        chat.say("count forever");
        chat.until("a statement is in flight", |w, t| {
            w.ai_session(t).is_some_and(|s| s.in_flight().is_some())
        });
        let effect = chat.w.dispatch(WorkspaceAction::CloseTab(tab));
        let text = format!("{effect:?}");
        assert!(text.contains("CancelQuery"), "{text}");
        chat.pump(effect);
        assert!(chat.w.ai_session(tab).is_none());
        assert!(chat.w.query_tab(tab).is_none());
        chat.finish_job();
        assert!(chat.w.ai_session(tab).is_none());
        // The session on loan is released once the turn ends (the cancelled call is abandoned, so it
        // is dropped, not closed); the editor's session stays open.
        assert_eq!(chat.closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn closing_a_session_with_an_idle_connection_closes_it() {
        let p = scripted();
        p.push_script(run_query("c1", "select 1"));
        p.push_script(text("one"));
        let mut chat = Chat::new(None, p);
        let tab = chat.open_session();
        chat.ask("one?");
        let effect = chat.w.dispatch(WorkspaceAction::CloseTab(tab));
        assert!(matches!(effect, Effect::CloseSession(_)), "{effect:?}");
        chat.pump(effect);
        assert_eq!(chat.seen.last(), Some(&Seen::CloseSession));
        assert!(chat.w.ai_session(tab).is_none());
    }

    #[test]
    fn closing_denies_a_waiting_approval() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        let mut chat = Chat::new(None, p);
        let tab = chat.open_session();
        chat.say("move it");
        chat.until_asked();
        let effect = chat.w.dispatch(WorkspaceAction::CloseTab(tab));
        chat.pump(effect);
        assert!(chat.w.ai_session(tab).is_none());
        chat.finish_job();
        assert_eq!(chat.country(2), "DE");
    }

    #[test]
    fn a_session_survives_a_split_and_a_move() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("done"));
        let mut chat = Chat::new(None, p.clone());
        let tab = chat.open_session();
        chat.say("move Contoso");
        chat.until_asked();
        chat.w.dispatch(WorkspaceAction::SplitRight);
        let second = chat.w.tabs().groups()[1].id();
        chat.w.dispatch(WorkspaceAction::MoveTab {
            id: tab,
            group: second,
            index: 0,
        });
        assert_eq!(chat.w.tabs().groups().len(), 2);
        assert!(chat.w.tabs().groups()[1].tabs().contains(&tab));
        assert!(chat.session().approvals().is_waiting());
        assert_ne!(chat.entries().len(), 0);
        chat.approve(tab);
        chat.until_ready();
        assert_eq!(chat.country(2), "GB");
        // A send works from the new group.
        p.push_script(text("second answer"));
        chat.ask("again");
        assert_eq!(assistant_texts(&chat).last().unwrap(), "second answer");
        // And a close there cleans up.
        let effect = chat.w.dispatch(WorkspaceAction::CloseTab(tab));
        chat.pump(effect);
        assert!(chat.w.ai_session(tab).is_none());
        assert!(chat.w.query_tab(tab).is_none());
        assert!(
            !chat
                .w
                .tabs()
                .groups()
                .iter()
                .any(|g| g.tabs().contains(&tab))
        );
    }

    #[test]
    fn the_gate_blocks_a_session_and_the_hub_says_why() {
        // AI off for the connection.
        let mut chat = Chat::with(
            None,
            scripted(),
            AiState::new(config(), vec![ProviderKind::Ollama], None),
            false,
        );
        let effect = chat.w.dispatch(WorkspaceAction::OpenConnection(0));
        chat.pump(effect);
        let tabs = chat.w.tabs().groups()[0].tabs().len();
        let effect = chat.w.dispatch_ai(AiAction::OpenSession);
        assert!(matches!(effect, Effect::None));
        assert_eq!(chat.notice().as_deref(), Some(SESSION_AI_OFF));
        assert_eq!(chat.w.tabs().groups()[0].tabs().len(), tabs);
        assert_eq!(chat.w.hub_model().new_session_blocked, Some(SESSION_AI_OFF));
        assert_eq!(chat.w.hub_model().sessions.len(), 0);

        // A broken ai.toml.
        let broken = AiState::from_startup(&AiStartup {
            config: Err("bad".into()),
            dir: None,
            factory: Arc::new(Fixed(scripted())),
        });
        let mut chat = Chat::with(None, scripted(), broken, true);
        let effect = chat.w.dispatch(WorkspaceAction::OpenConnection(0));
        chat.pump(effect);
        assert!(matches!(
            chat.w.dispatch_ai(AiAction::OpenSession),
            Effect::None
        ));
        assert_eq!(chat.notice().as_deref(), Some(SESSION_CONFIG_BROKEN));
        assert_eq!(
            chat.w.hub_model().new_session_blocked,
            Some(SESSION_CONFIG_BROKEN)
        );

        // No model: the AI view opens with the setup notice.
        let none = AiState::new(AiConfig::default(), vec![ProviderKind::Ollama], None);
        let mut chat = Chat::with(None, scripted(), none, true);
        let effect = chat.w.dispatch(WorkspaceAction::OpenConnection(0));
        chat.pump(effect);
        assert!(matches!(
            chat.w.dispatch_ai(AiAction::OpenSession),
            Effect::None
        ));
        assert_eq!(
            chat.notice().as_deref(),
            Some(super::super::ai::NO_PROVIDER)
        );
        assert_eq!(chat.w.layout().side_view(), SideView::Ai);
        assert_eq!(
            chat.w.hub_model().new_session_blocked,
            Some(super::super::ai::NO_PROVIDER)
        );

        // The kind is not in this build.
        let unbuilt = AiState::new(config(), Vec::new(), None);
        let mut chat = Chat::with(None, scripted(), unbuilt, true);
        let effect = chat.w.dispatch(WorkspaceAction::OpenConnection(0));
        chat.pump(effect);
        assert!(matches!(
            chat.w.dispatch_ai(AiAction::OpenSession),
            Effect::None
        ));
        assert_eq!(chat.notice().as_deref(), Some(SESSION_NOT_BUILT));
        assert_eq!(
            chat.w.hub_model().new_session_blocked,
            Some(SESSION_NOT_BUILT)
        );

        // No connection at all.
        let mut empty = Workspace::new(SidebarModel::default());
        assert!(matches!(
            empty.dispatch_ai(AiAction::OpenSession),
            Effect::None
        ));
        assert_eq!(empty.take_notice().as_deref(), Some(SESSION_NO_CONNECTION));
        assert_eq!(
            empty.hub_model().new_session_blocked,
            Some(SESSION_NO_CONNECTION)
        );
        let _ = NoProviders;
    }

    #[test]
    fn the_hub_lists_sessions_with_their_state() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("done"));
        let mut chat = Chat::new(None, p);
        let tab = chat.open_session();
        let model = chat.w.hub_model();
        assert_eq!(model.new_session_blocked, None);
        assert_eq!(model.sessions.len(), 1);
        assert_eq!(model.sessions[0].tab, tab);
        assert_eq!(model.sessions[0].title, "AI: db (2)");
        assert_eq!(model.sessions[0].connection, "db");
        assert_eq!(model.sessions[0].label, "Ready");
        chat.say("go");
        chat.until_asked();
        assert_eq!(
            chat.w.hub_model().sessions[0].label,
            "Waiting for your approval"
        );
        chat.approve(tab);
        chat.until_ready();
    }

    #[test]
    fn a_rerun_runs_the_exact_sql_in_a_new_editor() {
        let p = scripted();
        p.push_script(run_query("c1", "select name from customers where id = 1"));
        p.push_script(text("Acme"));
        let mut chat = Chat::new(Some(Environment::Dev), p);
        let tab = chat.open_session();
        chat.ask("who is 1?");
        let index = chat
            .entries()
            .iter()
            .position(|e| matches!(e, TranscriptEntry::Sql(_)))
            .unwrap();
        let before = chat.seen.len();
        let effect = chat.w.dispatch_ai(AiAction::RerunSql(tab, index));
        assert!(matches!(effect, Effect::Connect { .. }));
        chat.pump(effect);
        let seen = chat.seen[before..].to_vec();
        assert_eq!(
            seen,
            [
                Seen::Connect {
                    seed: Some("select name from customers where id = 1".into())
                },
                Seen::Run {
                    sql: "select name from customers where id = 1".into()
                }
            ]
        );
        // Open in editor only seeds.
        let before = chat.seen.len();
        let effect = chat.w.dispatch_ai(AiAction::OpenSqlInEditor(tab, index));
        chat.pump(effect);
        assert_eq!(chat.seen[before..].len(), 1);
        assert!(chat.w.ai_sessions.reruns.is_empty());
        // An index that is not a statement does nothing.
        assert!(matches!(
            chat.w.dispatch_ai(AiAction::RerunSql(tab, 0)),
            Effect::None
        ));
    }

    #[test]
    fn a_rerun_of_a_prod_write_waits_at_the_gate_of_the_new_tab() {
        let p = scripted();
        p.push_script(run_query("c1", UPDATE));
        p.push_script(text("done"));
        let mut chat = Chat::new(Some(Environment::Prod), p);
        let tab = chat.open_session();
        chat.say("go");
        chat.until_asked();
        chat.approve(tab);
        chat.until("the gate opens", |w, t| {
            matches!(
                w.query_tab(t).map(QueryTab::phase),
                Some(Phase::Confirming(_))
            )
        });
        chat.w.confirm_query(tab);
        chat.until_ready();
        assert_eq!(chat.country(2), "GB");
        chat.exec("update customers set country = 'DE' where id = 2");
        let index = chat
            .entries()
            .iter()
            .position(|e| matches!(e, TranscriptEntry::Sql(_)))
            .unwrap();
        let effect = chat.w.dispatch_ai(AiAction::RerunSql(tab, index));
        chat.pump(effect);
        let editor = chat.w.tabs().active().unwrap().id();
        assert_ne!(editor, tab);
        assert!(
            !chat.seen.iter().any(|s| matches!(s, Seen::Run { .. })),
            "the production gate stops the re-run"
        );
        let Phase::Confirming(list) = chat.w.query_tab(editor).unwrap().phase() else {
            panic!(
                "not confirming: {:?}",
                chat.w.query_tab(editor).unwrap().phase()
            );
        };
        assert_eq!(list[0].sql, UPDATE);
        assert_eq!(chat.country(2), "DE");
        let _ = RunMode::All;
    }

    #[test]
    fn quitting_cancels_the_turn_and_closes_the_sessions() {
        let p = scripted();
        p.push_script(run_query("c1", SLOW));
        let mut chat = Chat::new(None, p);
        chat.open_session();
        chat.say("count forever");
        chat.until("a statement is in flight", |w, t| {
            w.ai_session(t).is_some_and(|s| s.in_flight().is_some())
        });
        let effect = chat.w.dispatch(WorkspaceAction::Quit);
        let text = format!("{effect:?}");
        assert!(text.contains("CancelQuery"), "{text}");
        assert!(text.contains("CloseSessions"), "{text}");
        chat.pump(effect);
        chat.finish_job();
        assert!(chat.seen.contains(&Seen::CancelQuery));
    }

    #[test]
    fn quitting_closes_the_idle_session_of_a_tab() {
        let p = scripted();
        p.push_script(run_query("c1", "select 1"));
        p.push_script(text("one"));
        let mut chat = Chat::new(None, p);
        chat.open_session();
        chat.ask("one?");
        let effect = chat.w.dispatch(WorkspaceAction::Quit);
        // The query tab's session, the browser's and the idle AI one.
        let Effect::CloseSessions(sessions) = &effect else {
            panic!("{effect:?}");
        };
        assert!(sessions.len() >= 2, "{}", sessions.len());
        chat.pump(effect);
    }

    #[test]
    fn turning_ai_off_cancels_the_running_turn() {
        let open = Arc::new(AtomicUsize::new(0));
        let mut chat = Chat::new(
            None,
            Arc::new(StallProvider {
                open: Arc::clone(&open),
            }),
        );
        chat.open_session();
        chat.say("wait");
        chat.until("the model is thinking", |_, _| {
            open.load(Ordering::SeqCst) == 1
        });
        let effect = chat.w.dispatch_ai(AiAction::ToggleConnection);
        assert!(
            matches!(effect, Effect::SaveConnections { .. }),
            "{effect:?}"
        );
        chat.until_ready();
        assert_eq!(chat.session().state(), SessionState::Cancelled);
        assert_eq!(open.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_lost_turn_fails_the_session() {
        let open = Arc::new(AtomicUsize::new(0));
        let mut chat = Chat::new(
            None,
            Arc::new(StallProvider {
                open: Arc::clone(&open),
            }),
        );
        let tab = chat.open_session();
        chat.say("wait");
        let turn = match chat.session().phase() {
            SessionPhase::Running { turn } => *turn,
            other => panic!("{other:?}"),
        };
        chat.w.ai_turn_lost(tab, turn + 1);
        assert!(chat.session().is_running(), "another turn is ignored");
        chat.w.ai_turn_lost(tab, turn);
        assert!(matches!(chat.session().phase(), SessionPhase::Failed(_)));
        // Cancel the stuck job so nothing outlives the test.
        if let Some(job) = chat.job.take() {
            job.abort();
        }
        let _ = LimitKind::Time;
    }

    #[test]
    fn debug_output_never_holds_the_message_or_the_sql() {
        const MSG: &str = "canary-message-4417";
        const SQL: &str = "update customers set name = 'canary-sql-9921' where id = 1";
        let p = scripted();
        p.push_script(run_query("c1", SQL));
        p.push_script(text("canary-answer-3308"));
        let mut chat = Chat::new(None, p);
        let effect = chat.w.dispatch(WorkspaceAction::OpenConnection(0));
        chat.pump(effect);
        let effect = chat.w.dispatch_ai(AiAction::OpenSession);
        let start_text = format!("{effect:?}");
        assert!(start_text.contains("StartAiSession"));
        chat.pump(effect);
        let tab = chat.tab();
        let draft = AiAction::SetSessionDraft(tab, MSG.to_owned());
        let mut seen_text = vec![format!("{draft:?}")];
        chat.w.dispatch_ai(draft);
        let effect = chat.w.dispatch_ai(AiAction::SendMessage(tab));
        seen_text.push(format!("{effect:?}"));
        let Effect::RunAiTurn(job) = effect else {
            panic!("expected RunAiTurn");
        };
        seen_text.push(format!("{job:?}"));
        chat.job = Some(chat.rt.spawn(job.run()));
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out");
            let message = chat.recv();
            seen_text.push(format!("{message:?}"));
            if matches!(message, HostMsg::Ask(_)) {
                chat.asks += 1;
                seen_text.push(format!("{:?}", chat.w.ai_session_message(tab, message)));
                seen_text.push(format!("{:?}", chat.session()));
                seen_text.push(format!(
                    "{:?}",
                    chat.session().approvals().current().unwrap()
                ));
                chat.deny(tab);
                continue;
            }
            let done = matches!(message, HostMsg::TurnDone(_));
            let next = chat.w.ai_session_message(tab, message);
            seen_text.push(format!("{next:?}"));
            if done {
                break;
            }
        }
        seen_text.push(format!("{:?}", chat.session()));
        let Effect::Connect { request, .. } = chat.w.dispatch(WorkspaceAction::OpenConnection(0))
        else {
            panic!("expected Connect");
        };
        chat.w.ai_sessions.reruns.insert(request, SQL.to_owned());
        seen_text.push(format!("{:?}", chat.w.ai_sessions));
        for text in seen_text {
            for canary in [MSG, "canary-sql-9921", "canary-answer-3308"] {
                assert!(!text.contains(canary), "{canary} leaked into {text}");
            }
        }
        assert_eq!(chat.asks, 1);
    }

    impl Chat {
        /// Runs SQL on the database file directly.
        fn exec(&self, sql: &str) {
            let path = self.db.path();
            self.rt.block_on(async {
                let mut s = open_sqlite(&path).await;
                let q = Query::new(sql);
                let mut stream = s.execute(&q).await.unwrap();
                collect(&mut *stream).await.unwrap();
                drop(stream);
                s.close().await.unwrap();
            });
        }
    }
}
