// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The probe's `query` and `catalog` calls over a real SQLite file (ADR-0058).
//!
//! The database is a unique file in the temp directory (process id + counter in the name, nothing Windows reserves),
//! removed with its journal files on drop. `Sqlite` is the `DataService` the application would provide, with counters
//! for lent and returned sessions; `Approver` answers the write dialog from a closure and records every request.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::{
    ConnectionConfig, DbObject, DriverId, Environment, Listing, ObjectKind, ObjectPath, Query,
    Table,
};
use sqler_driver_api::{BoxFuture, Driver, DriverError, Introspect, Session, collect};
use sqler_driver_sqlite::SqliteDriver;
use sqler_plugin_host::{
    ApprovalStage, DataError, DataService, Decision, PluginConnection, QueryApprover, WriteRequest,
};

use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

const SEED: &[&str] = &[
    "create table users (id integer primary key, name text not null, email text default 'none')",
    "create table orders (id integer primary key, user_id integer references users(id), total real)",
    "create view user_names as select name from users",
    "insert into users (id, name, email) values (1,'ann','a@x'),(2,'bob','b@x'),(3,'cy','c@x'),(4,'di','d@x'),(5,'ed','e@x')",
];

/// A plain read that runs for hours (5^16 joined rows) unless the driver is cancelled. The classifier treats `WITH`
/// as possibly writing, so the long statements here are plain selects over the seeded `users`.
const FOREVER: &str = "select count(*) from users a, users b, users c, users d, users e, users f, users g, users h, \
     users i, users j, users k, users l, users m, users n, users o, users p";
const INSERT: &str = "insert into users (id, name) values (6, 'fay')";

type Decide = dyn Fn(&WriteRequest) -> Decision + Send + Sync;

/// Answers the write dialog and records what it was asked.
pub struct Approver {
    requests: Mutex<Vec<WriteRequest>>,
    decide: Box<Decide>,
}

impl Approver {
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

impl QueryApprover for Approver {
    fn approve<'a>(&'a self, request: &'a WriteRequest) -> BoxFuture<'a, Decision> {
        self.requests.lock().unwrap().push(request.clone());
        let decision = (self.decide)(request);
        Box::pin(std::future::ready(decision))
    }
}

/// What a data test lends to the probe.
#[derive(Default)]
pub struct DataOpts {
    pub environment: Option<Environment>,
    pub approver: Option<Arc<Approver>>,
}

/// Forwards introspection to a SQLite session for the catalog worker.
struct Forward(Box<dyn Session>);

impl Introspect for Forward {
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
        _object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

/// One SQLite connection `db`, lent to the probe.
pub struct Sqlite {
    path: PathBuf,
    conn: PluginConnection,
    catalog: Catalog,
    pub acquires: AtomicUsize,
    pub releases: AtomicUsize,
}

async fn open(path: &Path) -> Box<dyn Session> {
    let id = DriverId::new("sqlite").unwrap();
    let config = ConnectionConfig::new("t", id, path.to_str().unwrap());
    SqliteDriver::new().connect(&config).await.unwrap()
}

impl Sqlite {
    /// Needs a running tokio runtime (the catalog worker is spawned on it).
    pub async fn new(label: &str, opts: &DataOpts) -> Arc<Self> {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "sqler-plugin-host-guest-{label}-{}-{n}.db",
            std::process::id()
        ));
        let mut seeder = open(&path).await;
        for sql in SEED {
            let query = Query::new(*sql);
            let mut stream = seeder.execute(&query).await.unwrap();
            collect(&mut *stream).await.unwrap();
        }
        drop(seeder);
        let source: Box<dyn Introspect> = Box::new(Forward(open(&path).await));
        let (catalog, worker) = Catalog::new(source);
        tokio::spawn(worker.run());
        let conn = PluginConnection::new(
            "db",
            "Test DB",
            DriverId::new("sqlite").unwrap(),
            opts.environment,
        );
        Arc::new(Self {
            path,
            conn,
            catalog,
            acquires: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
        })
    }

    /// The rows of `select * from users`, read through a separate session.
    pub async fn user_count(&self) -> usize {
        let mut session = open(&self.path).await;
        let query = Query::new("select * from users");
        let mut stream = session.execute(&query).await.unwrap();
        collect(&mut *stream).await.unwrap().rows().len()
    }

    /// Waits (bounded by the 30 s hang guard of `block_on`) until `n` sessions have been returned.
    pub async fn wait_released(&self, n: usize) {
        while self.releases.load(Ordering::SeqCst) < n {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Sqlite {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = fs::remove_file(p);
        }
    }
}

impl DataService for Sqlite {
    fn connections(&self) -> Vec<PluginConnection> {
        vec![self.conn.clone()]
    }

    fn catalog(&self, connection: &str) -> Option<Catalog> {
        (connection == self.conn.id).then(|| self.catalog.clone())
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
            Ok(open(&self.path).await)
        })
    }

    fn release(&self, _connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let _ = session.close().await;
        })
    }
}

#[allow(clippy::needless_pass_by_value)] // call sites build the options inline
fn data_world(label: &str, fx: &Path, opts: Opts) -> World {
    let w = world(label, fx, "probe", &opts);
    w.set("connection", "db");
    w
}

fn with(data: DataOpts) -> Opts {
    Opts {
        data: Some(data),
        ..Opts::default()
    }
}

fn read_only_grant() -> Approve {
    Approve::Only(&["db.read", "fs.read:data", "fs.read:plugin"])
}

fn users(w: &World) -> usize {
    block_on(&w.rt, w.data.as_ref().unwrap().user_count())
}

#[test]
fn a_read_with_db_read_returns_rows() {
    let Some(fx) = fixtures("data_read") else {
        return;
    };
    let w = data_world("read", &fx, with(DataOpts::default()));
    w.set("sql", "select id, name from users order by id");
    let r = w.status("query");
    assert!(
        r.starts_with("ok cols=id,name rows=5 truncated=false"),
        "{r}"
    );
    let data = w.data.as_ref().unwrap();
    assert_eq!(data.acquires.load(Ordering::SeqCst), 1);
    block_on(&w.rt, data.wait_released(1));
    w.done();
}

#[test]
fn a_read_without_db_read_is_denied_and_never_reaches_the_database() {
    let Some(fx) = fixtures("data_read_denied") else {
        return;
    };
    for approve in [Approve::Nothing, Approve::Only(&["db.write"])] {
        let opts = Opts {
            approve,
            ..with(DataOpts::default())
        };
        let w = data_world("read-denied", &fx, opts);
        w.set("sql", "select id from users");
        let r = w.status("query");
        assert!(r.starts_with("err:capability-denied:"), "{r}");
        assert!(w.status("catalog").starts_with("err:capability-denied:"));
        assert_eq!(w.data.as_ref().unwrap().acquires.load(Ordering::SeqCst), 0);
        w.done();
    }
}

#[test]
fn a_write_without_db_write_is_refused_without_asking() {
    let Some(fx) = fixtures("data_write_refused") else {
        return;
    };
    let approver = Approver::always(Decision::Approve);
    let opts = Opts {
        approve: read_only_grant(),
        ..with(DataOpts {
            approver: Some(Arc::clone(&approver)),
            ..DataOpts::default()
        })
    };
    let w = data_world("write-refused", &fx, opts);
    w.set("sql", INSERT);
    let r = w.status("query");
    assert!(r.starts_with("err:capability-denied:"), "{r}");
    assert_eq!(approver.requests().len(), 0);
    assert_eq!(users(&w), 5);
    w.done();
}

#[test]
fn a_write_needs_the_approver_to_see_the_exact_statement() {
    let Some(fx) = fixtures("data_write_approved") else {
        return;
    };
    let approver = Approver::always(Decision::Approve);
    let w = data_world(
        "write-approved",
        &fx,
        with(DataOpts {
            approver: Some(Arc::clone(&approver)),
            ..DataOpts::default()
        }),
    );
    w.set("sql", INSERT);
    let r = w.status("query");
    assert!(r.starts_with("ok "), "{r}");
    assert!(r.ends_with("affected=1"), "{r}");
    let seen = approver.requests();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].sql, INSERT);
    assert_eq!(seen[0].params.len(), 0);
    assert_eq!(seen[0].plugin.as_str(), "probe");
    assert_eq!(seen[0].stage, ApprovalStage::Confirm);
    assert_eq!(seen[0].connection.id, "db");
    assert_eq!(users(&w), 6);
    w.done();
}

#[test]
fn a_denied_write_leaves_the_database_unchanged() {
    let Some(fx) = fixtures("data_write_denied") else {
        return;
    };
    let approver = Approver::always(Decision::Deny);
    let w = data_world(
        "write-denied",
        &fx,
        with(DataOpts {
            approver: Some(Arc::clone(&approver)),
            ..DataOpts::default()
        }),
    );
    w.set("sql", INSERT);
    let r = w.status("query");
    assert!(r.starts_with("err:declined:"), "{r}");
    assert_eq!(approver.requests().len(), 1);
    assert_eq!(users(&w), 5);
    w.done();
}

#[test]
fn a_write_with_no_approver_is_declined() {
    let Some(fx) = fixtures("data_write_no_approver") else {
        return;
    };
    let w = data_world("write-none", &fx, with(DataOpts::default()));
    w.set("sql", INSERT);
    let r = w.status("query");
    assert!(r.starts_with("err:declined:"), "{r}");
    assert_eq!(users(&w), 5);
    w.done();
}

#[test]
fn prod_asks_twice_and_a_second_denial_runs_nothing() {
    let Some(fx) = fixtures("data_prod") else {
        return;
    };
    let second_denied = Approver::new(|r| match r.stage {
        ApprovalStage::ProdSecond => Decision::Deny,
        _ => Decision::Approve,
    });
    let w = data_world(
        "prod-deny",
        &fx,
        with(DataOpts {
            environment: Some(Environment::Prod),
            approver: Some(Arc::clone(&second_denied)),
        }),
    );
    w.set("sql", INSERT);
    assert!(w.status("query").starts_with("err:declined:"));
    let stages: Vec<_> = second_denied.requests().iter().map(|r| r.stage).collect();
    assert_eq!(stages, [ApprovalStage::Confirm, ApprovalStage::ProdSecond]);
    assert_eq!(users(&w), 5);
    w.done();

    let both = Approver::always(Decision::Approve);
    let w = data_world(
        "prod-ok",
        &fx,
        with(DataOpts {
            environment: Some(Environment::Prod),
            approver: Some(Arc::clone(&both)),
        }),
    );
    w.set("sql", INSERT);
    assert!(w.status("query").starts_with("ok "));
    assert_eq!(both.requests().len(), 2);
    assert_eq!(users(&w), 6);
    w.done();
}

#[test]
fn explain_analyze_counts_as_a_write() {
    let Some(fx) = fixtures("data_explain_analyze") else {
        return;
    };
    let approver = Approver::always(Decision::Deny);
    let w = data_world(
        "explain",
        &fx,
        with(DataOpts {
            approver: Some(Arc::clone(&approver)),
            ..DataOpts::default()
        }),
    );
    w.set("sql", "explain analyze select * from users");
    let r = w.status("query");
    assert!(r.starts_with("err:declined:"), "{r}");
    let seen = approver.requests();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].sql, "explain analyze select * from users");
    w.done();
}

#[test]
fn more_than_one_statement_is_refused() {
    let Some(fx) = fixtures("data_multi") else {
        return;
    };
    let approver = Approver::always(Decision::Approve);
    let w = data_world(
        "multi",
        &fx,
        with(DataOpts {
            approver: Some(Arc::clone(&approver)),
            ..DataOpts::default()
        }),
    );
    w.set("sql", "select 1; delete from users");
    let r = w.status("query");
    assert!(r.starts_with("err:invalid-argument:"), "{r}");
    assert_eq!(approver.requests().len(), 0);
    assert_eq!(users(&w), 5);
    w.done();
}

#[test]
fn the_row_cap_truncates() {
    let Some(fx) = fixtures("data_row_cap") else {
        return;
    };
    let w = data_world("rows", &fx, with(DataOpts::default()));
    w.set(
        "sql",
        "select a.id from users a, users b, users c, users d, users e",
    );
    // The default cap (1000 rows) applies when the request names none.
    let r = w.status("query");
    assert!(r.contains(" rows=1000 truncated=true"), "{r}");
    let one = w.status("query-one");
    assert!(one.contains(" rows=1 truncated=true"), "{one}");
    w.done();
}

#[test]
fn the_byte_cap_truncates() {
    let Some(fx) = fixtures("data_byte_cap") else {
        return;
    };
    let w = data_world("bytes", &fx, with(DataOpts::default()));
    // Five rows of about 6 MB each (a joined `group_concat`): the 8 MiB cap is passed within the first two rows.
    w.set(
        "sql",
        "select (select group_concat(a.name) from users a, users b, users c, users d, users e, users f, users g, \
         users h, users i) from users",
    );
    let r = w.status("query");
    assert!(r.contains(" truncated=true"), "{r}");
    assert!(!r.contains(" rows=5 "), "{r}");
    w.done();
}

#[test]
fn a_timeout_is_an_error_without_partial_rows_and_returns_the_session() {
    let Some(fx) = fixtures("data_timeout") else {
        return;
    };
    let opts = Opts {
        query_timeout: Some(Duration::from_millis(300)),
        ..with(DataOpts::default())
    };
    let w = data_world("timeout", &fx, opts);
    w.set("sql", FOREVER);
    let r = w.status("query");
    assert!(r.starts_with("err:timeout:"), "{r}");
    assert!(!r.contains("rows="), "{r}");
    block_on(&w.rt, w.data.as_ref().unwrap().wait_released(1));
    w.done();
}

#[test]
fn a_dropped_call_releases_the_session() {
    let Some(fx) = fixtures("data_dropped") else {
        return;
    };
    // The status call (300 ms) ends before the query (30 s default timeout) does: the host drops the call mid-query.
    let opts = Opts {
        status: Duration::from_millis(300),
        ..with(DataOpts::default())
    };
    let w = data_world("dropped", &fx, opts);
    w.set("sql", FOREVER);
    let r = block_on(&w.rt, w.host.status(&w.id, &"query".parse().unwrap()));
    assert!(matches!(r, Err(CallError::Timeout { .. })), "{r:?}");
    let data = w.data.as_ref().unwrap();
    assert_eq!(data.acquires.load(Ordering::SeqCst), 1);
    block_on(&w.rt, data.wait_released(1));
    assert_eq!(data.releases.load(Ordering::SeqCst), 1);
    w.done();
}

#[test]
fn the_catalog_lists_connections_objects_and_columns() {
    let Some(fx) = fixtures("data_catalog") else {
        return;
    };
    let w = data_world("catalog", &fx, with(DataOpts::default()));
    let listing = w.status("catalog");
    assert!(listing.starts_with("conns=db objs="), "{listing}");
    for expected in ["table:users", "table:orders", "view:user_names"] {
        assert!(listing.contains(expected), "{expected} in {listing}");
    }
    w.set("table", "users");
    let columns = w.status("columns");
    for name in ["id:", "name:", "email:"] {
        assert!(columns.contains(name), "{columns}");
    }
    w.set("table", "missing");
    assert!(w.status("columns").starts_with("err:not-found:"));
    w.done();
}

#[test]
fn a_capability_revoked_mid_run_stops_the_next_call() {
    let Some(fx) = fixtures("data_revoked") else {
        return;
    };
    let w = data_world("revoked", &fx, with(DataOpts::default()));
    w.set("sql", "select id from users");
    assert!(w.status("query").starts_with("ok "));
    w.grants.revoke(&w.key);
    w.host.refresh_grants(&w.id);
    let r = w.status("query");
    assert!(r.starts_with("err:capability-denied:"), "{r}");
    assert_eq!(w.data.as_ref().unwrap().acquires.load(Ordering::SeqCst), 1);
    w.done();
}
