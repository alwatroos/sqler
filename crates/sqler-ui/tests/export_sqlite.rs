// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Exports a real `SQLite` result through the whole pipeline: the workspace decides, a fresh run
//! of the statement feeds the writer, and the file appears only when the writer has finished.
//! Covers every compiled-in format, cancel, an unwritable target and the refusal to run a
//! non-read statement again.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use sqler_config::ConnectionSpec;
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query};
use sqler_driver_api::{
    BoxFuture, CancelHandle, DriverError, DriverRegistry, QueryStream, Session, collect,
};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_export::{ExportOptions, Registry};
use sqler_sql::{Dialect, StatementClass};
use sqler_ui::browser::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt, SecretSource};
use sqler_ui::export::{
    ExportFailure, ExportJob, ExportOutcome, ExportScope, ExportSource, ExportStop, ExportTarget,
    FEED_CAPACITY, FreshRun, GridFacts, WriteSpec, fresh_rows, write_feed,
};
use sqler_ui::query::BoundStatement;
use sqler_ui::sidebar::SidebarModel;
use sqler_ui::workspace::{Effect, Workspace, WorkspaceAction};
use tokio::runtime::Builder;
use tokio::sync::mpsc;

const ROWS: usize = 200_000;

/// A scratch folder (and database file) that is removed with the guard.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sqler-export-sqlite-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Counts what the connector was asked.
#[derive(Default)]
struct Counts {
    connects: AtomicUsize,
    closed: AtomicUsize,
}

/// A session that counts its `close`.
struct Counted {
    inner: Box<dyn Session>,
    counts: Arc<Counts>,
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

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        let Counted { inner, counts } = *self;
        counts.closed.fetch_add(1, Ordering::SeqCst);
        inner.close()
    }
}

/// The smallest `Connector` over the `SQLite` driver: no secrets, no tunnel. Counts connects and
/// closes.
struct CountingConnector {
    registry: Arc<DriverRegistry>,
    counts: Arc<Counts>,
}

impl CountingConnector {
    fn pair() -> (Arc<dyn Connector>, Arc<Counts>) {
        let mut registry = DriverRegistry::new();
        registry.register(Box::new(SqliteDriver::new())).unwrap();
        let counts = Arc::new(Counts::default());
        let connector: Arc<dyn Connector> = Arc::new(Self {
            registry: Arc::new(registry),
            counts: Arc::clone(&counts),
        });
        (connector, counts)
    }
}

impl Connector for CountingConnector {
    fn driver_ids(&self) -> Vec<DriverId> {
        self.registry.ids().cloned().collect()
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
        self.counts.connects.fetch_add(1, Ordering::SeqCst);
        let registry = Arc::clone(&self.registry);
        let counts = Arc::clone(&self.counts);
        let config = ConnectionConfig::new(
            request.spec.name.clone(),
            request.spec.driver.clone(),
            request.spec.target.clone(),
        );
        Box::pin(async move {
            let inner = registry
                .connect(&config)
                .await
                .map_err(ConnectFailure::Driver)?;
            Ok(Box::new(Counted { inner, counts }) as Box<dyn Session>)
        })
    }

    fn trust_host_key(
        &self,
        _key: HostKeyPrompt,
    ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn request(target: &str) -> ConnectRequest {
    ConnectRequest {
        spec: ConnectionSpec::new("test", DriverId::new(DRIVER_ID).unwrap(), target),
        db_password: SecretSource::None,
        ssh_secret: SecretSource::None,
    }
}

fn big_query() -> Query {
    Query::new(format!(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {ROWS}) \
         SELECT i AS id, 'name' || i AS name FROM n"
    ))
}

fn options() -> ExportOptions {
    let mut options = ExportOptions::default();
    "t".clone_into(&mut options.sql.table);
    options.sql.dialect = Dialect::Sqlite;
    options
}

/// Runs the pipeline the way the app does: the writer on a thread, the producer on a runtime.
fn export_fresh(
    connector: Arc<dyn Connector>,
    spec: WriteSpec,
    stop: &ExportStop,
    progress: Box<dyn FnMut(sqler_export::Progress) + Send>,
    request: ConnectRequest,
    query: Query,
) -> ExportOutcome {
    let (tx, rx) = mpsc::channel(FEED_CAPACITY);
    let writer = {
        let stop = stop.clone();
        thread::spawn(move || write_feed(rx, spec, &stop, progress))
    };
    let runtime = Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(fresh_rows(connector, request, query, tx, stop.clone()));
    writer.join().unwrap()
}

fn spec(dir: &Scratch, id: &str, ext: &str) -> WriteSpec {
    WriteSpec {
        format_id: id.to_owned(),
        options: options(),
        formats: Arc::new(Registry::with_builtin()),
        path: dir.0.join(format!("out.{ext}")),
        job: 3,
    }
}

#[test]
fn a_fresh_run_goes_to_every_format() {
    let registry = Registry::with_builtin();
    let formats: Vec<(String, String)> = registry
        .formats()
        .map(|f| (f.id().to_owned(), f.extension().to_owned()))
        .collect();
    // The dev-dependency turns the binary formats on.
    assert!(formats.len() >= 6, "{formats:?}");
    for (id, ext) in formats {
        let dir = Scratch::new(&format!("all-{id}"));
        let (connector, counts) = CountingConnector::pair();
        let spec = spec(&dir, &id, &ext);
        let path = spec.path.clone();
        let summary = export_fresh(
            connector,
            spec,
            &ExportStop::new(),
            Box::new(|_| {}),
            request(":memory:"),
            big_query(),
        )
        .unwrap_or_else(|err| panic!("{id}: {err}"));
        assert_eq!(summary.rows, ROWS as u64, "{id}");
        assert_eq!(summary.path, path);
        assert_eq!(counts.connects.load(Ordering::SeqCst), 1, "{id}");
        assert_eq!(counts.closed.load(Ordering::SeqCst), 1, "{id}");
        assert_eq!(
            dir.names(),
            [format!("out.{ext}")],
            "{id}: partial left behind"
        );
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, summary.bytes, "{id}");
        match id.as_str() {
            "csv" => {
                let text = String::from_utf8(bytes).unwrap();
                assert!(
                    text.starts_with("id,name\r\n1,name1\r\n2,name2\r\n3,name3\r\n"),
                    "{}",
                    &text[..60]
                );
                assert_eq!(text.matches('\n').count(), ROWS + 1);
                assert!(text.ends_with(&format!("{ROWS},name{ROWS}\r\n")));
            }
            "json" => {
                let parsed: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(parsed.len(), ROWS);
                assert_eq!(parsed[0]["id"], 1);
                assert_eq!(parsed[0]["name"], "name1");
                assert_eq!(parsed[ROWS - 1]["id"], ROWS);
            }
            "sql" => {
                let text = String::from_utf8(bytes).unwrap();
                assert!(text.starts_with("INSERT INTO "), "{}", &text[..40]);
                assert!(text.contains("\"t\""));
                assert!(text.trim_end().ends_with(';'));
            }
            "markdown" => {
                let text = String::from_utf8(bytes).unwrap();
                assert!(text.starts_with("| id | name |"), "{}", &text[..40]);
                assert_eq!(text.lines().count(), ROWS + 2);
            }
            "parquet" => {
                assert_eq!(&bytes[..4], b"PAR1");
                assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
            }
            "xlsx" => assert_eq!(&bytes[..2], b"PK"),
            other => panic!("format {other} has no check"),
        }
    }
}

#[test]
fn cancel_after_the_first_progress_leaves_nothing() {
    let dir = Scratch::new("cancel");
    let (connector, counts) = CountingConnector::pair();
    let stop = ExportStop::new();
    let progress: Box<dyn FnMut(sqler_export::Progress) + Send> = {
        let stop = stop.clone();
        Box::new(move |p| {
            if p.rows > 0 {
                stop.cancel();
            }
        })
    };
    let err = export_fresh(
        connector,
        spec(&dir, "csv", "csv"),
        &stop,
        progress,
        request(":memory:"),
        big_query(),
    )
    .unwrap_err();
    assert_eq!(err, ExportFailure::Cancelled);
    assert_eq!(counts.connects.load(Ordering::SeqCst), 1);
    assert_eq!(counts.closed.load(Ordering::SeqCst), 1);
    assert_eq!(dir.names(), Vec::<String>::new());
}

#[test]
fn cancel_leaves_an_existing_target_untouched() {
    let dir = Scratch::new("cancel-keep");
    std::fs::write(dir.0.join("out.csv"), b"old").unwrap();
    let (connector, _) = CountingConnector::pair();
    let stop = ExportStop::new();
    stop.cancel();
    let err = export_fresh(
        connector,
        spec(&dir, "csv", "csv"),
        &stop,
        Box::new(|_| {}),
        request(":memory:"),
        big_query(),
    )
    .unwrap_err();
    assert_eq!(err, ExportFailure::Cancelled);
    assert_eq!(std::fs::read(dir.0.join("out.csv")).unwrap(), b"old");
    assert_eq!(dir.names(), ["out.csv"]);
}

#[test]
fn a_target_in_a_missing_folder_fails_before_any_connection() {
    let dir = Scratch::new("missing");
    let (connector, counts) = CountingConnector::pair();
    let mut spec = spec(&dir, "csv", "csv");
    spec.path = dir.0.join("nope").join("out.csv");
    let stop = ExportStop::new();
    let (tx, rx) = mpsc::channel(FEED_CAPACITY);
    // The writer fails at once, before any row is asked for. When the producer starts after that
    // (the app starts both together; a connect that is already under way is dropped), it does
    // not connect.
    let err = write_feed(rx, spec, &stop, Box::new(|_| {})).unwrap_err();
    let ExportFailure::Create(text) = &err else {
        panic!("expected Create, got {err:?}");
    };
    assert!(!text.contains("nope"), "{text}");
    assert!(!text.contains("sqler-export-sqlite"), "{text}");
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(fresh_rows(
        connector,
        request(":memory:"),
        big_query(),
        tx,
        stop,
    ));
    assert_eq!(counts.connects.load(Ordering::SeqCst), 0);
    assert_eq!(counts.closed.load(Ordering::SeqCst), 0);
    assert_eq!(dir.names(), Vec::<String>::new());
}

#[test]
fn a_failing_statement_is_reported_and_the_session_is_closed() {
    let dir = Scratch::new("bad-sql");
    let (connector, counts) = CountingConnector::pair();
    let err = export_fresh(
        connector,
        spec(&dir, "csv", "csv"),
        &ExportStop::new(),
        Box::new(|_| {}),
        request(":memory:"),
        Query::new("select * from no_such_table"),
    )
    .unwrap_err();
    assert!(matches!(err, ExportFailure::Driver(_)), "{err:?}");
    assert_eq!(counts.connects.load(Ordering::SeqCst), 1);
    assert_eq!(counts.closed.load(Ordering::SeqCst), 1);
    assert_eq!(dir.names(), Vec::<String>::new());
}

#[test]
fn failure_texts_keep_the_database_message_but_hold_no_secret() {
    let secret = "S3CR3T-literal";
    // A statement that fails while it runs, and one that cannot be prepared.
    for sql in [
        format!("select abs(-9223372036854775808), '{secret}'"),
        format!("select '{secret}' from no_such_table"),
    ] {
        let dir = Scratch::new("secret-sql");
        let (connector, _) = CountingConnector::pair();
        let err = export_fresh(
            connector,
            spec(&dir, "csv", "csv"),
            &ExportStop::new(),
            Box::new(|_| {}),
            request(":memory:"),
            Query::new(sql.as_str()),
        )
        .unwrap_err();
        let ExportFailure::Driver(text) = &err else {
            panic!("expected Driver, got {err:?}");
        };
        assert!(!text.contains(secret), "{text}");
        assert!(!text.contains(&sql), "{text}");
        assert!(text.starts_with("query failed: "), "{text}");
        assert_eq!(dir.names(), Vec::<String>::new());
    }
    // A hostile connection: a URL that carries a password, and a path in a folder that is missing.
    for target in [
        format!("sqlite://user:{secret}@no-such-host/{secret}/x.db"),
        format!("/no-such-folder-{secret}/{secret}/x.db"),
        format!("sqlite:/no-such-folder-{secret}/x.db?mode=ro"),
    ] {
        let dir = Scratch::new("secret-conn");
        let (connector, _) = CountingConnector::pair();
        let err = export_fresh(
            connector,
            spec(&dir, "csv", "csv"),
            &ExportStop::new(),
            Box::new(|_| {}),
            request(&target),
            Query::new("select 1"),
        )
        .unwrap_err();
        let ExportFailure::Connect(text) = &err else {
            panic!("expected Connect for {target}, got {err:?}");
        };
        assert!(!text.contains(secret), "{text}");
        assert!(!text.contains("no-such"), "{text}");
        assert_eq!(dir.names(), Vec::<String>::new());
    }
}

/// A workspace with one `SQLite` connection on `db` and a connected tab.
fn workspace(db: &Path) -> (Workspace, sqler_ui::tabs::TabId) {
    let driver = DriverId::new(DRIVER_ID).unwrap();
    let mut w = Workspace::new(SidebarModel::from_connections(vec![ConnectionConfig::new(
        "dev",
        driver,
        db.to_string_lossy(),
    )]));
    let Effect::Connect { request, .. } = w.dispatch(WorkspaceAction::OpenConnection(0)) else {
        panic!("expected Connect");
    };
    let (connector, _) = CountingConnector::pair();
    let session = Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(connector.connect(ConnectRequest {
            spec: ConnectionSpec::new(
                "dev",
                DriverId::new(DRIVER_ID).unwrap(),
                db.to_string_lossy(),
            ),
            db_password: SecretSource::None,
            ssh_secret: SecretSource::None,
        }))
        .unwrap();
    w.connect_finished(request, Ok(session));
    let tab = w.tabs().active().map(sqler_ui::tabs::Tab::id).unwrap();
    (w, tab)
}

fn statement(sql: &str, class: StatementClass) -> BoundStatement {
    BoundStatement {
        index: 0,
        sql: sql.to_owned(),
        class,
        query: Query::new(sql),
        values: Vec::new(),
    }
}

fn setup_db(dir: &Scratch) -> PathBuf {
    let path = dir.0.join("data.db");
    let (connector, _) = CountingConnector::pair();
    Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let mut session = connector
                .connect(request(&path.to_string_lossy()))
                .await
                .unwrap();
            for sql in [
                "create table t (id integer, name text)",
                "insert into t values (1, 'a'), (2, 'b'), (3, 'c')",
            ] {
                let query = Query::new(sql);
                let mut stream = session.execute(&query).await.unwrap();
                collect(&mut *stream).await.unwrap();
            }
            session.close().await.unwrap();
        });
    path
}

fn row_count(path: &Path) -> i64 {
    let (connector, _) = CountingConnector::pair();
    Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let mut session = connector
                .connect(request(&path.to_string_lossy()))
                .await
                .unwrap();
            let query = Query::new("select count(*) from t");
            let mut stream = session.execute(&query).await.unwrap();
            let set = collect(&mut *stream).await.unwrap();
            drop(stream);
            session.close().await.unwrap();
            match set.rows()[0].0[0].clone() {
                sqler_driver_api::sqler_core::Value::Int(n) => n,
                other => panic!("unexpected {other:?}"),
            }
        })
}

fn facts() -> GridFacts {
    GridFacts {
        columns: 2,
        rows: 3,
        displayed: None,
        complete: false,
        selection: None,
    }
}

#[test]
fn a_non_read_statement_is_never_run_again() {
    let dir = Scratch::new("refuse");
    let db = setup_db(&dir);
    let (mut w, tab) = workspace(&db);
    w.remember_run(tab, &[statement("delete from t", StatementClass::Write)]);
    let target = ExportTarget {
        tab,
        result: Some(0),
    };
    w.open_export(target, &facts());
    let dialog = w.export_dialog().unwrap();
    let FreshRun::Refused(reason) = &dialog.fresh else {
        panic!("expected Refused, got {:?}", dialog.fresh);
    };
    assert!(reason.contains("class: write"), "{reason}");
    assert_eq!(dialog.form.scope, ExportScope::Loaded);

    // Forcing the fresh scope is rejected: no save dialog, no job.
    w.export_dialog_mut().unwrap().set_scope(ExportScope::Fresh);
    let form = w.export_dialog().unwrap().form.clone();
    assert!(matches!(w.submit_export(form), Effect::None));
    assert!(w.export_dialog().unwrap().error.is_some());
    let out = dir.0.join("out.csv");
    assert!(matches!(
        w.export_path_chosen(w.export_dialog().unwrap().generation, Some(out.clone())),
        Effect::None
    ));
    assert!(!out.exists());
    assert_eq!(row_count(&db), 3, "the statement must not have run");

    // The loaded rows can still be exported, and that job holds no statement.
    w.export_dialog_mut()
        .unwrap()
        .set_scope(ExportScope::Loaded);
    let form = w.export_dialog().unwrap().form.clone();
    assert!(matches!(
        w.submit_export(form),
        Effect::PickExportPath { .. }
    ));
    let Effect::RunExport(job) =
        w.export_path_chosen(w.export_dialog().unwrap().generation, Some(out))
    else {
        panic!("expected RunExport");
    };
    assert!(matches!(job.source, ExportSource::Loaded(_)));
    assert_eq!(row_count(&db), 3);
}

#[test]
fn a_read_statement_runs_again_from_the_workspace_to_the_file() {
    let dir = Scratch::new("flow");
    let db = setup_db(&dir);
    let (mut w, tab) = workspace(&db);
    w.remember_run(
        tab,
        &[statement(
            "select id, name from t order by id",
            StatementClass::Read,
        )],
    );
    w.open_export(
        ExportTarget {
            tab,
            result: Some(0),
        },
        &facts(),
    );
    assert_eq!(w.export_dialog().unwrap().fresh, FreshRun::Allowed);
    let form = w.export_dialog().unwrap().form.clone();
    assert_eq!(form.scope, ExportScope::Fresh);
    let Effect::PickExportPath { suggested_name, .. } = w.submit_export(form) else {
        panic!("expected PickExportPath");
    };
    assert_eq!(suggested_name, "result-1.csv");
    let out = dir.0.join(&suggested_name);
    let Effect::RunExport(ExportJob {
        tab: job_tab,
        id,
        source,
        spec,
        stop,
        ..
    }) = w.export_path_chosen(w.export_dialog().unwrap().generation, Some(out.clone()))
    else {
        panic!("expected RunExport");
    };
    let ExportSource::Fresh { request, query } = source else {
        panic!("expected a fresh source");
    };
    let (connector, counts) = CountingConnector::pair();
    let outcome = export_fresh(connector, spec, &stop, Box::new(|_| {}), request, query);
    w.export_finished(job_tab, id, outcome);
    assert_eq!(
        w.take_notice().as_deref(),
        Some("Exported 3 rows to result-1.csv")
    );
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "id,name\r\n1,a\r\n2,b\r\n3,c\r\n"
    );
    assert_eq!(counts.connects.load(Ordering::SeqCst), 1);
    assert_eq!(counts.closed.load(Ordering::SeqCst), 1);
    assert!(w.export_status(tab).is_none());
}

#[test]
fn the_fresh_session_refuses_a_write() {
    let dir = Scratch::new("read-only");
    let (connector, counts) = CountingConnector::pair();
    let err = export_fresh(
        connector,
        spec(&dir, "csv", "csv"),
        &ExportStop::new(),
        Box::new(|_| {}),
        request(":memory:"),
        Query::new("create table w (a integer)"),
    )
    .unwrap_err();
    assert!(matches!(err, ExportFailure::Driver(_)), "{err:?}");
    assert_eq!(counts.closed.load(Ordering::SeqCst), 1);
    assert_eq!(dir.names(), Vec::<String>::new());
}
