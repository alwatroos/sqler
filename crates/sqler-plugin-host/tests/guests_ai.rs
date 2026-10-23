// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end tests of `ai-tools.invoke` with the real `ai-probe` guest (`plugins/fixtures/ai-probe`), built by
//! `cargo run -p xtask -- plugins build`.
//!
//! They run when `SQLER_PLUGIN_FIXTURES` points at the staged directory (`target/plugins/<profile>`, relative paths are
//! taken from the workspace root). Without it each test prints a `skipped:` line and passes;
//! `SQLER_TEST_REQUIRE_PLUGINS=1` turns a missing fixture into a failure. Every await is bounded by a 30 s hang guard.
#![cfg(feature = "cl")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query};
use sqler_driver_api::{BoxFuture, Driver, Session, collect};
use sqler_driver_sqlite::SqliteDriver;
use sqler_plugin_api::{Capability, CapabilitySet, PluginId, SettingValue, ToolName};
use sqler_plugin_host::{
    CallError, DataError, DataService, Decision, ErrorCode, GrantSource, HostOptions,
    MAX_AI_ARGUMENT_BYTES, MemorySettings, NoEgress, PluginConnection, PluginData, PluginDirs,
    PluginHost, PluginState, QueryApprover, Services, WriteRequest, discover,
};

use common::{Grants, Scratch, block_on, runtime, test_limits};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const INSERT: &str = "insert into users (id, name) values (3, 'cy')";

/// The staged fixture directory, or `None` (after saying why) when the test must be skipped.
fn fixtures(test: &str) -> Option<PathBuf> {
    let required = std::env::var("SQLER_TEST_REQUIRE_PLUGINS").is_ok_and(|v| v == "1");
    let dir = std::env::var_os("SQLER_PLUGIN_FIXTURES").map(PathBuf::from);
    let resolved = dir.map(|d| {
        if d.is_absolute() {
            d
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(d)
        }
    });
    let complete = resolved
        .as_ref()
        .filter(|d| d.join("ai-probe/ai-probe.wasm").is_file());
    if let Some(d) = complete {
        return Some(d.clone());
    }
    assert!(
        !required,
        "{test}: SQLER_TEST_REQUIRE_PLUGINS=1 but SQLER_PLUGIN_FIXTURES is unset or lacks the built ai-probe plugin \
         (run `cargo run -p xtask -- plugins build`)"
    );
    eprintln!(
        "skipped: {test}: set SQLER_PLUGIN_FIXTURES=target/plugins/<profile> to run the guest tests"
    );
    None
}

/// Answers the write dialog with a fixed decision and counts the requests.
struct Approver {
    decision: Decision,
    asked: Mutex<Vec<String>>,
}

impl Approver {
    fn new(decision: Decision) -> Arc<Self> {
        Arc::new(Self {
            decision,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl QueryApprover for Approver {
    fn approve<'a>(&'a self, request: &'a WriteRequest) -> BoxFuture<'a, Decision> {
        self.asked.lock().unwrap().push(request.sql.clone());
        Box::pin(std::future::ready(self.decision))
    }
}

/// One `SQLite` connection `db` with a `users` table of two rows.
struct Db {
    path: PathBuf,
    conn: PluginConnection,
}

async fn open(path: &Path) -> Box<dyn Session> {
    let id = DriverId::new("sqlite").unwrap();
    let config = ConnectionConfig::new("t", id, path.to_str().unwrap());
    SqliteDriver::new().connect(&config).await.unwrap()
}

impl Db {
    async fn new(label: &str) -> Arc<Self> {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "sqler-plugin-host-ai-{label}-{}-{n}.db",
            std::process::id()
        ));
        let mut seeder = open(&path).await;
        for sql in [
            "create table users (id integer primary key, name text not null)",
            "insert into users (id, name) values (1,'ann'),(2,'bob')",
        ] {
            let query = Query::new(sql);
            let mut stream = seeder.execute(&query).await.unwrap();
            collect(&mut *stream).await.unwrap();
        }
        drop(seeder);
        let conn = PluginConnection::new("db", "Test DB", DriverId::new("sqlite").unwrap(), None);
        Arc::new(Self { path, conn })
    }

    async fn user_count(&self) -> usize {
        let mut session = open(&self.path).await;
        let query = Query::new("select * from users");
        let mut stream = session.execute(&query).await.unwrap();
        collect(&mut *stream).await.unwrap().rows().len()
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = fs::remove_file(p);
        }
    }
}

impl DataService for Db {
    fn connections(&self) -> Vec<PluginConnection> {
        vec![self.conn.clone()]
    }

    fn catalog(&self, _connection: &str) -> Option<Catalog> {
        None
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DataError>> {
        Box::pin(async move {
            if connection != self.conn.id {
                return Err(DataError::UnknownConnection(connection.to_owned()));
            }
            Ok(open(&self.path).await)
        })
    }
}

struct World {
    _scratch: Scratch,
    rt: tokio::runtime::Runtime,
    host: PluginHost,
    id: PluginId,
    grants: Arc<Grants>,
    key: sqler_plugin_api::GrantKey,
    db: Arc<Db>,
}

/// What the grant source approves for the probe.
enum Approve {
    Declared,
    Only(&'static [&'static str]),
}

/// A host over a scratch copy of the staged `ai-probe` with the `ai_tool` deadline `deadline`.
fn world(label: &str, fixtures: &Path, approve: &Approve, deadline: Duration) -> World {
    let scratch = Scratch::new(&format!("ai-{label}"));
    let dir = scratch.path().join("ai-probe");
    fs::create_dir_all(&dir).unwrap();
    for file in ["sqler.toml", "ai-probe.wasm"] {
        fs::copy(fixtures.join("ai-probe").join(file), dir.join(file)).unwrap();
    }
    let rt = runtime();
    let db = block_on(&rt, Db::new(label));
    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let plugin = &d.plugins[0];
    let id = plugin.manifest.id().clone();
    let key = plugin.key.clone();
    let grants = Arc::new(Grants::default());
    match approve {
        Approve::Declared => grants.approve_declared(&d),
        Approve::Only(list) => {
            let set: CapabilitySet = list
                .iter()
                .map(|c| c.parse::<Capability>().unwrap())
                .collect();
            grants.approve(&plugin.key, set);
        }
    }
    let mut limits = test_limits(Duration::from_secs(30));
    limits.deadlines.ai_tool = deadline;
    let host = PluginHost::new(HostOptions {
        runtime: rt.handle().clone(),
        grants: Arc::clone(&grants) as Arc<dyn GrantSource>,
        settings: Arc::new(MemorySettings::default()),
        egress: Arc::new(NoEgress),
        limits,
        services: Services::new(),
    });
    let problems = host.load(d);
    assert_eq!(problems.len(), 0, "{problems:?}");
    let w = World {
        _scratch: scratch,
        rt,
        host,
        id,
        grants,
        key,
        db,
    };
    w.set("connection", "db");
    w
}

impl World {
    fn set(&self, key: &str, value: &str) {
        self.host
            .set_setting(&self.id, key, SettingValue::Text(value.to_owned()))
            .unwrap();
    }

    /// The data lent for one call: the database, with an approver when given.
    fn data(&self, approver: Option<&Arc<Approver>>) -> PluginData {
        let data = PluginData::new(Arc::clone(&self.db) as Arc<dyn DataService>);
        match approver {
            Some(a) => data.with_approver(Arc::clone(a) as Arc<dyn QueryApprover>),
            None => data,
        }
    }

    fn invoke(&self, tool: &str, arguments: &str, data: PluginData) -> Result<String, CallError> {
        let name: ToolName = tool.parse().unwrap();
        block_on(
            &self.rt,
            self.host
                .invoke_ai_tool(&self.id, &name, arguments.to_owned(), data),
        )
    }

    fn users(&self) -> usize {
        block_on(&self.rt, self.db.user_count())
    }

    fn done(self) {
        self.host.shutdown();
    }
}

#[test]
fn the_declared_tools_are_listed_with_their_model_names() {
    let Some(fx) = fixtures("ai_listed") else {
        return;
    };
    let w = world("listed", &fx, &Approve::Declared, Duration::from_secs(30));
    let names: Vec<String> = w
        .host
        .ai_tools()
        .iter()
        .map(sqler_plugin_host::PluginAiTool::model_name)
        .collect();
    assert_eq!(
        names,
        [
            "ai-probe__echo",
            "ai-probe__query",
            "ai-probe__trap",
            "ai-probe__spin"
        ]
    );
    assert!(w.host.is_idle(), "listing must not start the engine");
    w.done();
}

#[test]
fn echo_returns_its_arguments() {
    let Some(fx) = fixtures("ai_echo") else {
        return;
    };
    let w = world("echo", &fx, &Approve::Declared, Duration::from_secs(30));
    let r = w.invoke("echo", r#"{"text":"hi"}"#, w.data(None)).unwrap();
    assert_eq!(r, r#"{"text":"hi"}"#);
    assert_eq!(w.host.plugins()[0].state, PluginState::Live);
    w.done();
}

#[test]
fn a_read_runs_without_an_approver() {
    let Some(fx) = fixtures("ai_read") else {
        return;
    };
    let w = world("read", &fx, &Approve::Declared, Duration::from_secs(30));
    w.set("sql", "select id, name from users order by id");
    let r = w.invoke("query", "{}", w.data(None)).unwrap();
    assert_eq!(
        r,
        r#"{"columns":["id","name"],"rows":[[1,"ann"],[2,"bob"]]}"#
    );
    w.done();
}

#[test]
fn a_write_is_declined_without_an_approver_and_runs_with_an_approving_one() {
    let Some(fx) = fixtures("ai_write") else {
        return;
    };
    let w = world("write", &fx, &Approve::Declared, Duration::from_secs(30));
    w.set("sql", INSERT);
    let r = w.invoke("query", "{}", w.data(None)).unwrap();
    assert_eq!(r, r#"{"error":"declined"}"#);
    assert_eq!(w.users(), 2);

    let yes = Approver::new(Decision::Approve);
    let r = w.invoke("query", "{}", w.data(Some(&yes))).unwrap();
    assert_eq!(r, r#"{"columns":[],"rows":[]}"#);
    assert_eq!(yes.asked(), [INSERT]);
    assert_eq!(w.users(), 3);
    w.done();
}

#[test]
fn an_earlier_approver_does_not_leak_into_the_next_call() {
    let Some(fx) = fixtures("ai_no_leak") else {
        return;
    };
    let w = world("leak", &fx, &Approve::Declared, Duration::from_secs(30));
    w.set("sql", "insert into users (id, name) values (10, 'x')");
    let yes = Approver::new(Decision::Approve);
    let r = w.invoke("query", "{}", w.data(Some(&yes))).unwrap();
    assert!(r.contains("columns"), "{r}");
    assert_eq!(w.users(), 3);

    // The same instance serves the next call: a denying approver decides, and the approving one is not asked again.
    w.set("sql", "insert into users (id, name) values (11, 'y')");
    let no = Approver::new(Decision::Deny);
    let r = w.invoke("query", "{}", w.data(Some(&no))).unwrap();
    assert_eq!(r, r#"{"error":"declined"}"#);
    assert_eq!(no.asked().len(), 1);
    assert_eq!(yes.asked().len(), 1);
    assert_eq!(w.users(), 3);

    // And a call without any approver is declined, not served by an earlier one.
    let r = w.invoke("query", "{}", w.data(None)).unwrap();
    assert_eq!(r, r#"{"error":"declined"}"#);
    assert_eq!(yes.asked().len(), 1);
    w.done();
}

#[test]
fn a_trap_is_a_trap() {
    let Some(fx) = fixtures("ai_trap") else {
        return;
    };
    let w = world("trap", &fx, &Approve::Declared, Duration::from_secs(30));
    let e = w.invoke("trap", "{}", w.data(None)).unwrap_err();
    assert!(matches!(e, CallError::Trap { .. }), "{e:?}");
    // The instance was dropped; the next call relinks and works.
    assert_eq!(w.invoke("echo", "{}", w.data(None)).unwrap(), "{}");
    w.done();
}

#[test]
fn a_spinning_tool_times_out() {
    let Some(fx) = fixtures("ai_spin") else {
        return;
    };
    let w = world("spin", &fx, &Approve::Declared, Duration::from_millis(500));
    // Warm up so that the spin, not the compile, meets the deadline.
    w.invoke("echo", "{}", w.data(None)).unwrap();
    let e = w.invoke("spin", "{}", w.data(None)).unwrap_err();
    assert!(matches!(e, CallError::Timeout { .. }), "{e:?}");
    w.done();
}

#[test]
fn a_revoked_grant_hides_the_tools_and_refuses_calls_without_starting_the_plugin() {
    let Some(fx) = fixtures("ai_revoked") else {
        return;
    };
    let w = world(
        "revoked",
        &fx,
        &Approve::Only(&["db.read"]),
        Duration::from_secs(30),
    );
    assert_eq!(w.host.ai_tools().len(), 0);
    let e = w.invoke("echo", "{}", w.data(None)).unwrap_err();
    match e {
        CallError::Guest(g) => assert_eq!(g.code, ErrorCode::CapabilityDenied),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.host.plugins()[0].state, PluginState::Idle);
    assert!(w.host.is_idle());

    // Granted, used, then revoked.
    w.grants.approve(&w.key, World::declared_capabilities(&fx));
    assert_eq!(w.host.ai_tools().len(), 4);
    w.invoke("echo", "{}", w.data(None)).unwrap();
    w.grants.revoke(&w.key);
    w.host.refresh_grants(&w.id);
    assert_eq!(w.host.ai_tools().len(), 0);
    assert!(w.invoke("echo", "{}", w.data(None)).is_err());
    assert_eq!(w.host.plugins()[0].state, PluginState::Idle);
    w.done();
}

#[test]
fn oversize_arguments_are_refused() {
    let Some(fx) = fixtures("ai_oversize") else {
        return;
    };
    let w = world("oversize", &fx, &Approve::Declared, Duration::from_secs(30));
    let big = "x".repeat(MAX_AI_ARGUMENT_BYTES + 1);
    let e = w.invoke("echo", &big, w.data(None)).unwrap_err();
    match e {
        CallError::Guest(g) => assert_eq!(g.code, ErrorCode::InvalidArgument),
        other => panic!("{other:?}"),
    }
    w.done();
}

impl World {
    /// Every capability the fixture's manifest declares.
    fn declared_capabilities(fixtures: &Path) -> CapabilitySet {
        let bytes = fs::read(fixtures.join("ai-probe/sqler.toml")).unwrap();
        sqler_plugin_api::Manifest::parse_bytes(&bytes)
            .unwrap()
            .capabilities()
            .clone()
    }
}
