// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The async job that applies a confirmed plan.
//!
//! It runs on the tokio runtime of the `SessionBridge` (never on the UI thread) and reports one
//! [`ApplyJobDone`]. The session is either lent by the query tab ([`ApplySession::Lent`], the
//! session the result was read on, returned in the answer) or opened for this job
//! ([`ApplySession::Connect`], closed at the end). Only a [`ConfirmedPlan`] can be put into a job,
//! so the production gate of ADR-0030 cannot be skipped here.

use std::sync::{Arc, Mutex, PoisonError};

use sqler_driver_api::{BoxFuture, CancelFlag, CancelHandle, Session};

use super::apply::{ApplyOutcome, apply};
use super::editor::GridKey;
use super::plan::ConfirmedPlan;
use super::staged::RowRef;
use crate::connections::{ConnectRequest, Connector};

/// Identifies one apply of a grid, so the answer of a stale one is recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ApplyId(u64);

impl ApplyId {
    /// An id from its number.
    #[must_use]
    pub fn new(n: u64) -> Self {
        Self(n)
    }

    /// The number.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Where the job gets its session.
#[non_exhaustive]
#[allow(clippy::large_enum_variant)] // one per apply; boxing a public field buys nothing
pub enum ApplySession {
    /// The query tab's own session, on loan until the job returns it.
    Lent(Box<dyn Session>),
    /// A short-lived session the job opens and closes (table-data tabs have none).
    Connect(ConnectRequest),
}

impl std::fmt::Debug for ApplySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Lent(_) => "ApplySession::Lent",
            Self::Connect(_) => "ApplySession::Connect",
        })
    }
}

/// The stop switch of an apply: a flag the job checks between statements, and the cancel handle
/// of the session it runs on, to interrupt a statement that is executing.
///
/// Cloneable; all clones share both. [`ApplyStop::cancel`] sets the flag and returns the handle
/// (once the job has stored it), which the caller sends as a cancel request.
#[derive(Debug, Clone, Default)]
pub struct ApplyStop {
    flag: CancelFlag,
    handle: Arc<Mutex<Option<CancelHandle>>>,
}

impl ApplyStop {
    /// A switch that is not set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<CancelHandle>> {
        self.handle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the flag and returns the running session's cancel handle, if the job has one yet.
    #[must_use]
    pub fn cancel(&self) -> Option<CancelHandle> {
        self.flag.cancel();
        self.slot().clone()
    }

    /// Whether [`ApplyStop::cancel`] was called on this switch or a clone.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.is_cancelled()
    }

    fn set_handle(&self, handle: CancelHandle) {
        *self.slot() = Some(handle);
    }

    /// Forgets the handle: the session is gone or back with its tab.
    fn clear_handle(&self) {
        *self.slot() = None;
    }
}

/// Applies a confirmed plan on a session.
pub struct ApplyJob {
    /// The grid the edits belong to.
    pub key: GridKey,
    /// This apply.
    pub run: ApplyId,
    /// Where the session comes from.
    pub session: ApplySession,
    /// The plan; the job runs exactly its statements.
    pub plan: ConfirmedPlan,
    /// Stops the apply.
    pub stop: ApplyStop,
}

impl std::fmt::Debug for ApplyJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyJob")
            .field("key", &self.key)
            .field("run", &self.run)
            .field("session", &self.session)
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

/// What a finished [`ApplyJob`] reports.
pub struct ApplyJobDone {
    /// The grid the edits belong to.
    pub key: GridKey,
    /// The apply.
    pub run: ApplyId,
    /// How it ended.
    pub outcome: ApplyOutcome,
    /// The lent session, back for its tab; `None` for a session the job opened itself.
    pub session: Option<Box<dyn Session>>,
}

impl std::fmt::Debug for ApplyJobDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyJobDone")
            .field("key", &self.key)
            .field("run", &self.run)
            .field("outcome", &self.outcome)
            .field("session", &self.session.is_some())
            .finish()
    }
}

fn not_started(message: impl Into<String>) -> ApplyOutcome {
    ApplyOutcome::NotStarted {
        message: message.into(),
    }
}

impl ApplyJob {
    /// The row each data statement of the plan is for, in execution order: the rows the editor
    /// marks if statement `i` fails.
    #[must_use]
    pub fn statement_rows(&self) -> Vec<RowRef> {
        self.plan
            .plan()
            .statements()
            .iter()
            .map(|s| s.row)
            .collect()
    }

    /// Runs the job.
    ///
    /// `Lent`: applies on the lent session and hands it back. If the stop switch is already set,
    /// nothing is sent ([`ApplyOutcome::NotStarted`]).
    ///
    /// `Connect`: if the stop switch is already set the connector is not called; if connecting
    /// fails, or the stop switch was set meanwhile, the outcome is [`ApplyOutcome::NotStarted`]
    /// with "Could not connect: ...". On success the job stores the session's cancel handle,
    /// applies, forgets the handle and closes the session (a close error is ignored).
    ///
    /// The job never panics and never runs anything but the plan.
    pub fn run(self, connector: Arc<dyn Connector>) -> BoxFuture<'static, ApplyJobDone> {
        Box::pin(async move {
            let Self {
                key,
                run,
                session,
                plan,
                stop,
            } = self;
            match session {
                ApplySession::Lent(mut session) => {
                    let outcome = if stop.is_cancelled() {
                        not_started("The apply was cancelled before it started.")
                    } else {
                        stop.set_handle(session.cancel_handle());
                        let outcome = apply(&mut *session, plan, &stop.flag).await;
                        stop.clear_handle();
                        outcome
                    };
                    ApplyJobDone {
                        key,
                        run,
                        outcome,
                        session: Some(session),
                    }
                }
                ApplySession::Connect(request) => {
                    let outcome = connect_and_apply(&*connector, request, plan, &stop).await;
                    ApplyJobDone {
                        key,
                        run,
                        outcome,
                        session: None,
                    }
                }
            }
        })
    }
}

async fn connect_and_apply(
    connector: &dyn Connector,
    request: ConnectRequest,
    plan: ConfirmedPlan,
    stop: &ApplyStop,
) -> ApplyOutcome {
    if stop.is_cancelled() {
        return not_started("Could not connect: the apply was cancelled.");
    }
    let mut session = match connector.connect(request).await {
        Ok(session) => session,
        Err(e) => return not_started(format!("Could not connect: {e}")),
    };
    let outcome = if stop.is_cancelled() {
        not_started("Could not connect: the apply was cancelled.")
    } else {
        stop.set_handle(session.cancel_handle());
        let outcome = apply(&mut *session, plan, &stop.flag).await;
        stop.clear_handle();
        outcome
    };
    let _ = session.close().await;
    outcome
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_config::ConnectionSpec;
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, Value};
    use sqler_driver_api::{DriverError, DriverRegistry, QueryStream, collect};
    use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};

    use super::*;
    use crate::connections::{ConnectFailure, HostKeyPrompt, SecretSource};
    use crate::grid::edit::testing::{row, target};
    use crate::grid::edit::{ModelRow, StagedEdits};
    use crate::tabs::{TabConnection, TabKind, TabStrip};

    fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn grid_key() -> GridKey {
        let connection = TabConnection {
            name: "t".to_owned(),
            driver: DriverId::new(DRIVER_ID).unwrap(),
            color: None,
            environment: None,
        };
        GridKey {
            tab: TabStrip::default().open(connection, TabKind::Query),
            result: Some(0),
        }
    }

    static FILES: AtomicUsize = AtomicUsize::new(0);

    /// A database file that is removed when dropped.
    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new() -> Self {
            let n = FILES.fetch_add(1, Ordering::SeqCst);
            Self(std::env::temp_dir().join(format!("sqler-job-{}-{n}.db", std::process::id())))
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

    async fn open(path: &str) -> Box<dyn Session> {
        let mut registry = DriverRegistry::new();
        registry.register(Box::new(SqliteDriver::new())).unwrap();
        let config = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), path);
        registry.connect(&config).await.unwrap()
    }

    async fn exec(s: &mut dyn Session, sql: &str) {
        let q = Query::new(sql);
        let mut stream = s.execute(&q).await.unwrap();
        collect(&mut *stream).await.unwrap();
    }

    async fn names(s: &mut dyn Session) -> Vec<Value> {
        let q = Query::new("SELECT name FROM t ORDER BY id");
        let mut stream = s.execute(&q).await.unwrap();
        let rows = collect(&mut *stream).await.unwrap().rows().to_vec();
        rows.into_iter().map(|r| r.0[0].clone()).collect()
    }

    /// `t(id, name)` with rows 1 "a" and 2 "b".
    async fn seeded(db: &TempDb) -> Box<dyn Session> {
        let mut s = open(&db.path()).await;
        exec(
            &mut *s,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        )
        .await;
        exec(&mut *s, "INSERT INTO t VALUES (1, 'a'), (2, 'b')").await;
        s
    }

    /// An update of row 1 and a delete of row 2.
    fn plan() -> ConfirmedPlan {
        let mut s = StagedEdits::new(target(false));
        s.set_cell(ModelRow(0), &row(1, Some("a")), 1, Value::Text("A".into()))
            .unwrap();
        s.delete_row(ModelRow(1), &row(2, Some("b"))).unwrap();
        s.plan().unwrap().confirm_non_production()
    }

    fn job(session: ApplySession, stop: &ApplyStop) -> ApplyJob {
        ApplyJob {
            key: grid_key(),
            run: ApplyId::new(7),
            session,
            plan: plan(),
            stop: stop.clone(),
        }
    }

    fn request(path: &str) -> ConnectRequest {
        ConnectRequest {
            spec: ConnectionSpec::new("c", DriverId::new(DRIVER_ID).unwrap(), path),
            db_password: SecretSource::None,
            ssh_secret: SecretSource::None,
        }
    }

    /// Opens real `SQLite` sessions on the requested path and counts the calls.
    struct FileConnector {
        calls: AtomicUsize,
        fail: bool,
    }

    impl FileConnector {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail,
            })
        }
    }

    impl Connector for FileConnector {
        fn driver_ids(&self) -> Vec<DriverId> {
            vec![DriverId::new(DRIVER_ID).unwrap()]
        }

        fn connect(
            &self,
            request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let fail = self.fail;
            Box::pin(async move {
                if fail {
                    return Err(ConnectFailure::Aborted);
                }
                Ok(open(&request.spec.target).await)
            })
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    #[test]
    fn statement_rows_follow_the_plan_order() {
        let stop = ApplyStop::new();
        let j = job(ApplySession::Connect(request("x")), &stop);
        // The delete comes first, then the update.
        assert_eq!(
            j.statement_rows(),
            [RowRef::Model(ModelRow(1)), RowRef::Model(ModelRow(0)),]
        );
    }

    #[test]
    fn lent_session_applies_and_comes_back() {
        block_on(async {
            let db = TempDb::new();
            let session = seeded(&db).await;
            let stop = ApplyStop::new();
            let done = job(ApplySession::Lent(session), &stop)
                .run(FileConnector::new(false))
                .await;
            assert_eq!(done.run, ApplyId::new(7));
            assert_eq!(done.outcome, ApplyOutcome::Committed { applied: 2 });
            let mut back = done.session.expect("the lent session is returned");
            assert_eq!(names(&mut *back).await, [Value::Text("A".into())]);
            // The handle is forgotten once the session is back with its tab.
            assert!(stop.cancel().is_none());
        });
    }

    #[test]
    fn connect_path_opens_applies_and_closes() {
        block_on(async {
            let db = TempDb::new();
            let mut seed = seeded(&db).await;
            let connector = FileConnector::new(false);
            let stop = ApplyStop::new();
            let done = job(ApplySession::Connect(request(&db.path())), &stop)
                .run(connector.clone())
                .await;
            assert_eq!(done.outcome, ApplyOutcome::Committed { applied: 2 });
            assert!(done.session.is_none());
            assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
            assert_eq!(names(&mut *seed).await, [Value::Text("A".into())]);
        });
    }

    #[test]
    fn cancel_before_connect_never_connects() {
        block_on(async {
            let connector = FileConnector::new(false);
            let stop = ApplyStop::new();
            assert!(stop.cancel().is_none());
            let done = job(ApplySession::Connect(request("unused")), &stop)
                .run(connector.clone())
                .await;
            let ApplyOutcome::NotStarted { message } = done.outcome else {
                panic!("{:?}", done.outcome);
            };
            assert!(message.starts_with("Could not connect: "), "{message}");
            assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn cancel_before_a_lent_apply_sends_nothing() {
        block_on(async {
            let db = TempDb::new();
            let session = seeded(&db).await;
            let stop = ApplyStop::new();
            let _ = stop.cancel();
            let done = job(ApplySession::Lent(session), &stop)
                .run(FileConnector::new(false))
                .await;
            assert!(matches!(done.outcome, ApplyOutcome::NotStarted { .. }));
            let mut back = done.session.unwrap();
            assert_eq!(
                names(&mut *back).await,
                [Value::Text("a".into()), Value::Text("b".into())]
            );
        });
    }

    #[test]
    fn a_failed_connect_is_not_started() {
        block_on(async {
            let connector = FileConnector::new(true);
            let stop = ApplyStop::new();
            let done = job(ApplySession::Connect(request("unused")), &stop)
                .run(connector.clone())
                .await;
            assert_eq!(
                done.outcome,
                ApplyOutcome::NotStarted {
                    message: "Could not connect: connect aborted".to_owned()
                }
            );
            assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
        });
    }

    /// Forwards to a real session and, on every statement, asks the stop switch for the handle and
    /// sets it, as the user pressing Cancel would.
    struct Cancelling {
        inner: Box<dyn Session>,
        stop: ApplyStop,
        handles: Arc<Mutex<Vec<bool>>>,
    }

    impl Session for Cancelling {
        fn cancel_handle(&self) -> CancelHandle {
            self.inner.cancel_handle()
        }

        fn execute<'s>(
            &'s mut self,
            query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            if query.sql.starts_with("UPDATE") {
                let handle = self.stop.cancel();
                self.handles.lock().unwrap().push(handle.is_some());
            }
            self.inner.execute(query)
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            self.inner.close()
        }
    }

    #[test]
    fn cancel_during_the_apply_has_a_handle_and_rolls_back() {
        block_on(async {
            let db = TempDb::new();
            let inner = seeded(&db).await;
            let stop = ApplyStop::new();
            let handles = Arc::new(Mutex::new(Vec::new()));
            let session = Box::new(Cancelling {
                inner,
                stop: stop.clone(),
                handles: Arc::clone(&handles),
            });
            let done = job(ApplySession::Lent(session), &stop)
                .run(FileConnector::new(false))
                .await;
            // The delete ran, the flag was set by the update's statement, the update itself
            // was already sent; the cancel is seen before COMMIT.
            assert!(
                matches!(done.outcome, ApplyOutcome::RolledBack { .. }),
                "{:?}",
                done.outcome
            );
            assert_eq!(*handles.lock().unwrap(), [true]);
            let mut back = done.session.unwrap();
            assert_eq!(
                names(&mut *back).await,
                [Value::Text("a".into()), Value::Text("b".into())]
            );
        });
    }

    #[test]
    fn debug_prints_no_statement_text_or_values() {
        let stop = ApplyStop::new();
        let mut s = StagedEdits::new(target(false));
        s.set_cell(
            ModelRow(0),
            &row(424_242, Some("sekrit-old")),
            1,
            Value::Text("sekrit-new".into()),
        )
        .unwrap();
        let j = ApplyJob {
            key: grid_key(),
            run: ApplyId::new(1),
            session: ApplySession::Connect(request("x")),
            plan: s.plan().unwrap().confirm_non_production(),
            stop,
        };
        let done = ApplyJobDone {
            key: j.key,
            run: j.run,
            outcome: ApplyOutcome::NotStarted {
                message: "sekrit".to_owned(),
            },
            session: None,
        };
        let shown = format!("{j:?} {done:?} {:?}", j.stop);
        for secret in ["sekrit", "424242", "UPDATE"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
}
