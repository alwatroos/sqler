// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin drivers through the registry (ADR-0061), without a built guest: a fake in-memory host engine behind a
//! declarative host driver, the availability rules, and a WAT guest that lacks the `driver` export.

#![cfg(feature = "cl")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqler_driver_api::sqler_core::{
    Column, ColumnInfo, ConnectionConfig, DataType, DbObject, DriverId, Listing, ObjectKind,
    ObjectPath, Query, Row, Table, Value,
};
use sqler_driver_api::{
    BoxFuture, CancelFlag, CancelHandle, Driver, DriverError, DriverRegistry, DriverSource,
    Introspect, QueryEvent, QueryStream, Session, SourceLookup, collect,
};
use sqler_plugin_api::{EngineId, GrantKey, PluginId};
use sqler_plugin_host::{HostEngines, PluginDrivers, PluginHost, PluginState};

use common::{Grants, Scratch, block_on, discover_all, host, runtime, test_limits, wat_component};

// ---------------------------------------------------------------------------------------------------------------
// A fake host engine
// ---------------------------------------------------------------------------------------------------------------

struct Fake;

impl Driver for Fake {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn connect<'a>(
        &'a self,
        _config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async {
            Ok(Box::new(FakeSession {
                flag: CancelFlag::new(),
            }) as Box<dyn Session>)
        })
    }
}

struct FakeSession {
    flag: CancelFlag,
}

impl Session for FakeSession {
    fn cancel_handle(&self) -> CancelHandle {
        self.flag.handle()
    }

    fn execute<'s>(
        &'s mut self,
        _query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        let stream = FakeStream {
            step: 0,
            flag: self.flag.clone(),
            finished: false,
        };
        Box::pin(std::future::ready(Ok(
            Box::new(stream) as Box<dyn QueryStream + 's>
        )))
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        Some(self)
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

impl Introspect for FakeSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(kind, ObjectKind::Table | ObjectKind::Schema)
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        let out = if self.supports(kind) {
            Listing::Objects(vec![DbObject::new(kind, parent.clone(), "users")])
        } else {
            Listing::Unsupported
        };
        Box::pin(std::future::ready(Ok(out)))
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        let t = Table::new(table.clone()).with_columns(vec![
            ColumnInfo::new("id", DataType::Integer).with_nullable(false),
        ]);
        Box::pin(std::future::ready(Ok(Some(t))))
    }

    fn definition<'a>(
        &'a mut self,
        _object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(std::future::ready(Ok(Some(
            "CREATE TABLE users".to_owned(),
        ))))
    }
}

struct FakeStream {
    step: u32,
    flag: CancelFlag,
    finished: bool,
}

impl QueryStream for FakeStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        let event = if self.finished {
            None
        } else if self.step > 0 && self.flag.is_cancelled() {
            self.finished = true;
            Some(Err(DriverError::Cancelled))
        } else {
            let step = self.step;
            self.step += 1;
            Some(Ok(match step {
                0 => QueryEvent::Columns(vec![Column::new("n", DataType::Integer)]),
                1..=3 => QueryEvent::Rows(vec![Row(vec![Value::Int(i64::from(step))])]),
                _ => {
                    self.finished = true;
                    QueryEvent::Done {
                        rows_affected: None,
                    }
                }
            }))
        };
        Box::pin(std::future::ready(event))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Plugins on disk
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Kind {
    /// `kind = "host"`, engine `fake`, no component.
    Host,
    /// Like `Host`, but the engine is one the build does not link.
    HostMissingEngine,
    /// `kind = "guest"` with the WAT component `ok` (no `driver` export).
    Guest,
}

fn write_plugin(root: &Path, id: &str, driver: &str, kind: Kind) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    let (caps, entry, drv) = match kind {
        Kind::Host => (
            "\"driver.host:fake\"",
            "",
            "kind = \"host\"\nengine = \"fake\"\n",
        ),
        Kind::HostMissingEngine => (
            "\"driver.host:nothing\"",
            "",
            "kind = \"host\"\nengine = \"nothing\"\n",
        ),
        Kind::Guest => (
            "\"driver.guest\"",
            "\n[entry]\ncomponent = \"plugin.wasm\"\n",
            "kind = \"guest\"\n",
        ),
    };
    fs::write(
        dir.join("sqler.toml"),
        format!(
            "[plugin]\nid = \"{id}\"\nname = \"Test\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = [{caps}]\n{entry}\n[[drivers]]\nid = \"{driver}\"\nname = \"Driver\"\n{drv}"
        ),
    )
    .unwrap();
    if matches!(kind, Kind::Guest) {
        fs::write(dir.join("plugin.wasm"), wat_component("ok")).unwrap();
    }
    dir
}

struct World {
    _scratch: Scratch,
    rt: tokio::runtime::Runtime,
    host: PluginHost,
    grants: Arc<Grants>,
    keys: Vec<(PluginId, GrantKey)>,
    drivers: Arc<PluginDrivers>,
    registry: DriverRegistry,
}

fn engines() -> HostEngines {
    let mut e = HostEngines::new();
    e.register("fake".parse::<EngineId>().unwrap(), Arc::new(Fake))
        .unwrap();
    e
}

/// A world with the plugins `(id, driver, kind)`, all approved, the source attached and added to the registry.
fn world(test: &str, plugins: &[(&str, &str, Kind)]) -> World {
    let scratch = Scratch::new(test);
    let dirs: Vec<_> = plugins
        .iter()
        .map(|(id, driver, kind)| write_plugin(scratch.path(), id, driver, *kind))
        .collect();
    let d = discover_all(&dirs);
    let grants = Arc::new(Grants::default());
    grants.approve_declared(&d);
    let keys = d
        .plugins
        .iter()
        .map(|p| (p.manifest.id().clone(), p.key.clone()))
        .collect();
    let rt = runtime();
    let host = host(
        &rt,
        grants.clone(),
        &test_limits(std::time::Duration::from_secs(30)),
    );
    assert_eq!(host.load(d).len(), 0);
    let drivers = PluginDrivers::new(engines());
    drivers.attach(&host);
    let mut registry = DriverRegistry::new();
    registry.add_source(drivers.clone());
    World {
        _scratch: scratch,
        rt,
        host,
        grants,
        keys,
        drivers,
        registry,
    }
}

impl World {
    fn key(&self, plugin: &str) -> GrantKey {
        self.keys
            .iter()
            .find(|(id, _)| id.as_str() == plugin)
            .map(|(_, k)| k.clone())
            .unwrap()
    }

    fn connect(&self, driver: &str) -> Result<Box<dyn Session>, DriverError> {
        block_on(&self.rt, self.registry.connect(&config(driver)))
    }
}

fn config(driver: &str) -> ConnectionConfig {
    ConnectionConfig::new("t", DriverId::new(driver).unwrap(), "mem")
}

fn id(s: &str) -> DriverId {
    DriverId::new(s).unwrap()
}

fn unavailable(r: Result<Box<dyn Session>, DriverError>) -> String {
    match r {
        Err(DriverError::Unavailable(t)) => t,
        Err(other) => panic!("expected Unavailable, got {other:?}"),
        Ok(_) => panic!("expected Unavailable, connected"),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn before_attach_the_source_offers_nothing() {
    let drivers = PluginDrivers::new(engines());
    assert_eq!(drivers.ids().len(), 0);
    assert!(matches!(
        drivers.lookup(&id("fake-db")),
        SourceLookup::Unknown
    ));
}

#[test]
fn the_first_attach_wins() {
    let w = world("first-attach", &[("p1", "fake-db", Kind::Host)]);
    let other = {
        let rt = runtime();
        host(
            &rt,
            Arc::new(Grants::default()),
            &test_limits(std::time::Duration::from_secs(1)),
        )
    };
    w.drivers.attach(&other);
    assert_eq!(w.drivers.ids(), [id("fake-db")]);
}

#[test]
fn a_host_driver_connects_streams_and_closes() {
    let w = world("host-connect", &[("p1", "fake-db", Kind::Host)]);
    assert_eq!(w.registry.available_ids(), [id("fake-db")]);
    let mut session = w.connect("fake-db").unwrap();
    let q = Query::new("select n");
    let rs = block_on(&w.rt, async {
        let mut stream = session.execute(&q).await?;
        collect(&mut *stream).await
    })
    .unwrap();
    assert_eq!(rs.columns()[0].name, "n");
    assert_eq!(rs.len(), 3);
    block_on(&w.rt, session.close()).unwrap();
}

#[test]
fn a_host_driver_cancels_mid_stream() {
    let w = world("host-cancel", &[("p1", "fake-db", Kind::Host)]);
    let mut session = w.connect("fake-db").unwrap();
    let handle = session.cancel_handle();
    let q = Query::new("select n");
    block_on(&w.rt, async {
        let mut stream = session.execute(&q).await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Columns(_)))
        ));
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Rows(_)))
        ));
        handle.cancel().await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Err(DriverError::Cancelled))
        ));
        assert!(stream.next_event().await.is_none());
    });
}

#[test]
fn a_host_driver_passes_introspection_through() {
    let w = world("host-introspect", &[("p1", "fake-db", Kind::Host)]);
    let mut session = w.connect("fake-db").unwrap();
    let intro = session.introspection().expect("introspection");
    assert!(intro.supports(ObjectKind::Table));
    assert!(!intro.supports(ObjectKind::Role));
    let root = ObjectPath::root();
    block_on(&w.rt, async {
        assert_eq!(
            intro.list(&root, ObjectKind::Role).await.unwrap(),
            Listing::Unsupported
        );
        let Listing::Objects(tables) = intro.list(&root, ObjectKind::Table).await.unwrap() else {
            panic!("tables are supported");
        };
        assert_eq!(tables[0].name, "users");
        let t = intro.table(&tables[0]).await.unwrap().unwrap();
        assert!(!t.column("id").unwrap().nullable);
        assert_eq!(
            intro.definition(&tables[0]).await.unwrap().as_deref(),
            Some("CREATE TABLE users")
        );
    });
}

#[test]
fn a_host_driver_without_an_entry_never_calls_a_guest() {
    // The plugin has no `[entry]`: a guest call would fail with `NotFound`/`MissingExport`, and the plugin would
    // show up as started.
    let w = world("host-no-guest", &[("p1", "fake-db", Kind::Host)]);
    let mut session = w.connect("fake-db").unwrap();
    block_on(&w.rt, async {
        let q = Query::new("x");
        let mut stream = session.execute(&q).await.unwrap();
        collect(&mut *stream).await.unwrap();
    });
    assert_eq!(w.host.plugins()[0].state, PluginState::Idle);
    assert!(w.host.is_idle());
}

#[test]
fn a_revoke_stops_a_host_session_at_its_next_statement() {
    let w = world("host-revoke", &[("p1", "fake-db", Kind::Host)]);
    let mut session = w.connect("fake-db").unwrap();
    let q = Query::new("x");
    block_on(&w.rt, async {
        let mut stream = session.execute(&q).await.unwrap();
        collect(&mut *stream).await.unwrap();
    });
    w.grants.revoke(&w.key("p1"));
    let err = block_on(&w.rt, async { session.execute(&q).await.err() }).expect("refused");
    assert!(
        matches!(&err, DriverError::Unavailable(t)
        if t == "driver `fake-db` needs the permission `driver.host:fake` of plugin `p1`, which is not approved"),
        "{err:?}"
    );
    // The catalog is gated the same way.
    let intro = session.introspection().expect("introspection");
    let err = block_on(&w.rt, intro.list(&ObjectPath::root(), ObjectKind::Table)).unwrap_err();
    assert!(matches!(err, DriverError::Unavailable(_)), "{err:?}");
    // And a new connection is refused with the same text; the id is gone from the list.
    let text = unavailable(w.connect("fake-db"));
    assert!(text.contains("is not approved"), "{text}");
    assert_eq!(w.registry.available_ids().len(), 0);
}

#[test]
fn disable_and_enable_take_effect_at_the_next_lookup() {
    let w = world("host-disable", &[("p1", "fake-db", Kind::Host)]);
    let p1: PluginId = "p1".parse().unwrap();
    assert!(w.connect("fake-db").is_ok());
    w.host.disable(&p1);
    assert_eq!(
        unavailable(w.connect("fake-db")),
        "driver `fake-db` comes from plugin `p1`, which is disabled"
    );
    assert_eq!(w.registry.available_ids().len(), 0);
    w.host.enable(&p1);
    assert!(w.connect("fake-db").is_ok());
    assert_eq!(w.registry.available_ids(), [id("fake-db")]);
}

#[test]
fn an_engine_the_build_lacks_is_unavailable() {
    let w = world(
        "no-engine",
        &[("p1", "nothing-db", Kind::HostMissingEngine)],
    );
    assert_eq!(
        unavailable(w.connect("nothing-db")),
        "driver `nothing-db` needs the `nothing` engine, which this build does not include"
    );
    assert_eq!(w.registry.available_ids().len(), 0);
}

#[test]
fn an_unapproved_plugin_is_unavailable() {
    let w = world("unapproved", &[("p1", "fake-db", Kind::Host)]);
    w.grants.revoke(&w.key("p1"));
    assert_eq!(
        unavailable(w.connect("fake-db")),
        "driver `fake-db` needs the permission `driver.host:fake` of plugin `p1`, which is not approved"
    );
}

#[test]
fn the_first_plugin_owns_a_contested_id() {
    // Dev paths are loaded in the order given.
    let w = world(
        "collision",
        &[("aaa", "shared", Kind::Host), ("bbb", "shared", Kind::Host)],
    );
    let aaa: PluginId = "aaa".parse().unwrap();
    let bbb: PluginId = "bbb".parse().unwrap();
    assert!(w.drivers.availability(&aaa, "shared").is_ok());
    assert_eq!(
        w.drivers.availability(&bbb, "shared").unwrap_err(),
        "driver `shared` is already provided by plugin `aaa`"
    );
    assert_eq!(w.registry.available_ids(), [id("shared")]);
    assert!(w.connect("shared").is_ok());
    // The owner going away does not hand the id to the other plugin silently: it stays the owner's, with its text.
    w.host.disable(&aaa);
    assert_eq!(
        unavailable(w.connect("shared")),
        "driver `shared` comes from plugin `aaa`, which is disabled"
    );
}

#[test]
fn a_built_in_id_is_reserved() {
    let w = world("reserved", &[("p1", "mysql", Kind::Host)]);
    let p1: PluginId = "p1".parse().unwrap();
    assert_eq!(
        w.drivers.availability(&p1, "mysql").unwrap_err(),
        "driver `mysql` of plugin `p1` is reserved for a built-in driver"
    );
    assert!(w.registry.available_ids().iter().all(|i| i != &id("mysql")));
}

#[test]
fn an_unknown_driver_is_unknown() {
    let w = world("unknown", &[("p1", "fake-db", Kind::Host)]);
    assert!(matches!(
        w.drivers.lookup(&id("nope")),
        SourceLookup::Unknown
    ));
    assert!(matches!(
        w.connect("nope"),
        Err(DriverError::UnknownDriver(_))
    ));
    let p1: PluginId = "p1".parse().unwrap();
    assert!(w.drivers.availability(&p1, "nope").is_err());
}

#[test]
fn a_guest_without_the_driver_export_is_unavailable_and_not_a_fault() {
    let w = world("guest-no-export", &[("g1", "guest-db", Kind::Guest)]);
    assert_eq!(w.registry.available_ids(), [id("guest-db")]);
    // More tries than the three-faults rule allows: none of them counts.
    for _ in 0..4 {
        assert_eq!(
            unavailable(w.connect("guest-db")),
            "the plugin driver is not available"
        );
    }
    assert!(matches!(
        w.host.plugins()[0].state,
        PluginState::Live | PluginState::Idle
    ));
}

#[test]
fn a_guest_driver_with_the_permission_revoked_is_unavailable() {
    let w = world("guest-revoked", &[("g1", "guest-db", Kind::Guest)]);
    w.grants.revoke(&w.key("g1"));
    assert_eq!(
        unavailable(w.connect("guest-db")),
        "driver `guest-db` needs the permission `driver.guest` of plugin `g1`, which is not approved"
    );
}

#[test]
fn the_driver_module_has_no_write_path_of_its_own() {
    // Guard parity (ADR-0061): a plugin driver is only a registry session. Nothing in it names the approval or the
    // guarded-query machinery of the `query` interface.
    fn scan(dir: &Path, hits: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
                for needle in ["QueryApprover", "run_guarded", "WriteRequest"] {
                    if text.contains(needle) {
                        hits.push(format!("{} names {needle}", path.display()));
                    }
                }
            }
        }
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/driver");
    let mut hits = Vec::new();
    scan(&dir, &mut hits);
    assert!(hits.is_empty(), "{hits:?}");
}
