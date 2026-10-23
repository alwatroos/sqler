// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The driver conformance scenarios (`sqler-driver-api/tests/mock_driver.rs`) against the real guest driver
//! `plugins/fixtures/driver` (driver id `fixture-db`), through `DriverRegistry` only.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use sqler_driver_api::sqler_core::{
    ConnectionConfig, DbObject, DriverId, Listing, ObjectKind, ObjectPath, Query, Value,
};
use sqler_driver_api::{DriverError, DriverRegistry, QueryEvent, Session, collect};
use sqler_plugin_api::{GrantKey, PluginId};
use sqler_plugin_host::{HostEngines, PluginDirs, PluginDrivers, PluginHost, discover};

use super::common::{Grants, Scratch, block_on, host, runtime, test_limits};

const SESSION_ENDED: &str = "the plugin driver session ended (the plugin was restarted, disabled or its permissions changed)";

struct Rig {
    _scratch: Scratch,
    rt: tokio::runtime::Runtime,
    host: PluginHost,
    grants: Arc<Grants>,
    key: GrantKey,
    id: PluginId,
    registry: DriverRegistry,
}

/// The staged driver fixture, or `None` (after saying why) when the test must be skipped.
fn staged(test: &str) -> Option<std::path::PathBuf> {
    let fixtures = super::fixtures(test)?;
    let dir = fixtures.join("driver-fixture");
    if dir.join("driver.wasm").is_file() {
        return Some(dir);
    }
    assert!(
        std::env::var("SQLER_TEST_REQUIRE_PLUGINS").map_or(true, |v| v != "1"),
        "{test}: the driver fixture is not built (run `cargo run -p xtask -- plugins build`)"
    );
    eprintln!("skipped: {test}: the driver fixture is not staged");
    None
}

fn rig(test: &str) -> Option<Rig> {
    let staged = staged(test)?;
    let scratch = Scratch::new(&format!("driver-{test}"));
    let dir = scratch.path().join("driver-fixture");
    fs::create_dir_all(&dir).unwrap();
    for file in ["sqler.toml", "driver.wasm"] {
        fs::copy(staged.join(file), dir.join(file)).unwrap();
    }
    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let grants = Arc::new(Grants::default());
    grants.approve_declared(&d);
    let key = d.plugins[0].key.clone();
    let id = d.plugins[0].manifest.id().clone();
    let rt = runtime();
    let host = host(&rt, grants.clone(), &test_limits(Duration::from_secs(30)));
    assert_eq!(host.load(d).len(), 0);
    let drivers = PluginDrivers::new(HostEngines::new());
    drivers.attach(&host);
    let mut registry = DriverRegistry::new();
    registry.add_source(drivers);
    Some(Rig {
        _scratch: scratch,
        rt,
        host,
        grants,
        key,
        id,
        registry,
    })
}

impl Rig {
    fn connect(&self, target: &str) -> Result<Box<dyn Session>, DriverError> {
        let config =
            ConnectionConfig::new("t", DriverId::new("fixture-db").unwrap(), target.to_owned());
        block_on(&self.rt, self.registry.connect(&config))
    }

    fn open(&self) -> Box<dyn Session> {
        self.connect("fixture://ann:s3cret@db.example.com:5433/sales?a=1&b=2")
            .unwrap()
    }

    fn run(
        &self,
        session: &mut Box<dyn Session>,
        sql: &str,
    ) -> Result<sqler_driver_api::sqler_core::ResultSet, DriverError> {
        let q = Query::new(sql);
        block_on(&self.rt, async {
            let mut stream = session.execute(&q).await?;
            collect(&mut *stream).await
        })
    }
}

#[test]
fn the_guest_receives_the_connect_parameters_and_nothing_else() {
    let Some(r) = rig("params") else { return };
    let mut s = r.open();
    let rs = r.run(&mut s, "whoami").unwrap();
    let row = &rs.rows()[0];
    assert_eq!(row.get(0), Some(&Value::Text("db.example.com".into())));
    assert_eq!(row.get(1), Some(&Value::Text("ann".into())));
    assert_eq!(row.get(2), Some(&Value::Text("sales".into())));
    assert_eq!(row.get(3), Some(&Value::Text("s3cret".into())));
    assert_eq!(row.get(4), Some(&Value::Int(2)));
    assert_eq!(row.get(5), Some(&Value::Int(5433)));
    block_on(&r.rt, s.close()).unwrap();
}

#[test]
fn a_missing_password_is_refused_by_the_guest() {
    let Some(r) = rig("no-password") else { return };
    let err = r
        .connect("fixture://ann@db.example.com/sales")
        .err()
        .unwrap();
    assert!(
        matches!(&err, DriverError::Connection(t) if t == "credentials are required"),
        "{err:?}"
    );
}

#[test]
fn rows_stream_in_batches() {
    let Some(r) = rig("stream") else { return };
    let mut s = r.open();
    let rs = r.run(&mut s, "rows 1203").unwrap();
    assert_eq!(rs.columns()[0].name, "n");
    assert_eq!(rs.columns()[1].name, "s");
    assert_eq!(rs.len(), 1203);
    assert_eq!(rs.rows()[1202].get(0), Some(&Value::Int(1203)));
    assert_eq!(
        rs.rows()[1202].get(1),
        Some(&Value::Text("row 1203".into()))
    );
    let empty = r.run(&mut s, "rows 0").unwrap();
    assert_eq!(empty.len(), 0);
    block_on(&r.rt, s.close()).unwrap();
}

#[test]
fn affected_rows_have_no_columns() {
    let Some(r) = rig("affected") else { return };
    let mut s = r.open();
    let rs = r.run(&mut s, "affected 7").unwrap();
    assert_eq!(rs.columns().len(), 0);
    assert_eq!(rs.rows_affected(), Some(7));
}

#[test]
fn parameters_reach_the_guest_as_values() {
    let Some(r) = rig("params-values") else {
        return;
    };
    let mut s = r.open();
    let q = Query::new("echo").with_params(vec![
        Value::Int(5),
        Value::Text("x".into()),
        Value::Null,
        Value::Bool(true),
        Value::Bytes(vec![1, 2]),
    ]);
    let rs = block_on(&r.rt, async {
        let mut stream = s.execute(&q).await?;
        collect(&mut *stream).await
    })
    .unwrap();
    let got: Vec<_> = rs
        .rows()
        .iter()
        .map(|row| row.get(0).unwrap().clone())
        .collect();
    assert_eq!(
        got,
        [
            Value::Int(5),
            Value::Text("x".into()),
            Value::Null,
            Value::Bool(true),
            Value::Bytes(vec![1, 2]),
        ]
    );
}

#[test]
fn a_guest_error_is_a_query_error_and_the_session_survives() {
    let Some(r) = rig("error") else { return };
    let mut s = r.open();
    let err = r.run(&mut s, "error").unwrap_err();
    assert!(
        matches!(&err, DriverError::Query(t) if t == "no such table fixture"),
        "{err:?}"
    );
    assert_eq!(r.run(&mut s, "rows 2").unwrap().len(), 2);
}

#[test]
fn a_guest_error_that_echoes_the_password_is_replaced() {
    let Some(r) = rig("leak") else { return };
    let err = r.connect("fixture://ann:s3cret@h/leak").err().unwrap();
    let DriverError::Connection(text) = &err else {
        panic!("{err:?}");
    };
    assert_eq!(text, "the database denied the request");
    assert!(!format!("{err:?}").contains("s3cret"));
}

#[test]
fn cancel_while_the_guest_spins_ends_the_stream_and_the_session() {
    let Some(r) = rig("cancel-spin") else { return };
    let mut s = r.open();
    let handle = s.cancel_handle();
    let q = Query::new("spin");
    block_on(&r.rt, async {
        let mut stream = s.execute(&q).await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Columns(_)))
        ));
        let (event, ()) = tokio::join!(stream.next_event(), async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            handle.cancel().await.unwrap();
        });
        assert!(
            matches!(event, Some(Err(DriverError::Cancelled))),
            "{event:?}"
        );
        assert!(stream.next_event().await.is_none());
        assert!(stream.next_event().await.is_none());
    });
    // The instance died with the cancel: the old session is over, a new connection works.
    let err = r.run(&mut s, "rows 1").unwrap_err();
    assert!(
        matches!(&err, DriverError::Unavailable(t) if t == SESSION_ENDED),
        "{err:?}"
    );
    let mut again = r.open();
    assert_eq!(r.run(&mut again, "rows 3").unwrap().len(), 3);
    block_on(&r.rt, s.close()).unwrap();
}

#[test]
fn cancel_between_batches_only_ends_the_stream() {
    let Some(r) = rig("cancel-between") else {
        return;
    };
    let mut s = r.open();
    let handle = s.cancel_handle();
    let q = Query::new("rows 1000");
    block_on(&r.rt, async {
        let mut stream = s.execute(&q).await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Columns(_)))
        ));
        let Some(Ok(QueryEvent::Rows(rows))) = stream.next_event().await else {
            panic!("a batch");
        };
        assert_eq!(rows.len(), 500);
        handle.cancel().await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Err(DriverError::Cancelled))
        ));
        assert!(stream.next_event().await.is_none());
    });
    // Same instance, same session.
    assert_eq!(r.run(&mut s, "rows 2").unwrap().len(), 2);
}

#[test]
fn a_stream_dropped_early_leaves_the_session_usable() {
    let Some(r) = rig("drop-early") else { return };
    let mut s = r.open();
    let q = Query::new("rows 1000");
    block_on(&r.rt, async {
        let mut stream = s.execute(&q).await.unwrap();
        let _ = stream.next_event().await;
        let _ = stream.next_event().await;
        drop(stream);
    });
    assert_eq!(r.run(&mut s, "rows 4").unwrap().len(), 4);
}

#[test]
fn introspection_lists_describes_and_defines() {
    let Some(r) = rig("introspect") else { return };
    let mut s = r.open();
    let intro = s.introspection().expect("the fixture has driver-catalog");
    assert!(intro.supports(ObjectKind::Table));
    assert!(intro.supports(ObjectKind::Schema));
    assert!(!intro.supports(ObjectKind::Role));
    let root = ObjectPath::root();
    block_on(&r.rt, async {
        assert_eq!(
            intro.list(&root, ObjectKind::Role).await.unwrap(),
            Listing::Unsupported
        );
        let Listing::Objects(tables) = intro.list(&root, ObjectKind::Table).await.unwrap() else {
            panic!("tables are supported");
        };
        let names: Vec<_> = tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["orders", "users"]);
        assert_eq!(tables[0].kind, ObjectKind::Table);

        let users = intro.table(&tables[1]).await.unwrap().unwrap();
        assert_eq!(users.columns.len(), 2);
        let id = users.column("id").unwrap();
        assert!(!id.nullable);
        assert_eq!(id.native_type.as_deref(), Some("INTEGER"));
        let name = users.column("name").unwrap();
        assert!(name.nullable);
        assert_eq!(name.native_type, None);
        assert_eq!(name.default.as_deref(), Some("'anon'"));

        assert!(intro.table(&tables[0]).await.unwrap().is_none());
        assert!(
            intro
                .definition(&tables[1])
                .await
                .unwrap()
                .unwrap()
                .starts_with("CREATE TABLE users")
        );
        let missing = DbObject::new(ObjectKind::Table, root.clone(), "nope");
        assert!(intro.definition(&missing).await.unwrap().is_none());
    });
}

#[test]
fn a_trap_in_the_second_batch_ends_the_stream_with_an_error() {
    let Some(r) = rig("trap") else { return };
    let mut s = r.open();
    let q = Query::new("trap-second");
    block_on(&r.rt, async {
        let mut stream = s.execute(&q).await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Columns(_)))
        ));
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Rows(_)))
        ));
        let event = stream.next_event().await;
        assert!(
            matches!(&event, Some(Err(DriverError::Query(t))) if t == "the plugin driver crashed"),
            "{event:?}"
        );
        assert!(stream.next_event().await.is_none());
    });
    // The host is alive; the old session is over; a new connect works.
    let err = r.run(&mut s, "rows 1").unwrap_err();
    assert!(
        matches!(&err, DriverError::Unavailable(t) if t == SESSION_ENDED),
        "{err:?}"
    );
    let mut again = r.open();
    assert_eq!(r.run(&mut again, "rows 3").unwrap().len(), 3);
}

#[test]
fn malformed_batches_poison_the_stream() {
    let Some(r) = rig("malformed") else { return };
    for sql in ["wide", "many", "bad-date"] {
        let mut s = r.open();
        let q = Query::new(sql);
        block_on(&r.rt, async {
            let mut stream = s.execute(&q).await.unwrap();
            assert!(matches!(
                stream.next_event().await,
                Some(Ok(QueryEvent::Columns(_)))
            ));
            let event = stream.next_event().await;
            assert!(
                matches!(&event, Some(Err(DriverError::Protocol(t)))
                    if t == "the plugin driver sent malformed rows"),
                "{sql}: {event:?}"
            );
            assert!(stream.next_event().await.is_none(), "{sql}");
        });
        // A malformed answer is the guest's fault in the protocol, not a host fault: the session lives on.
        assert_eq!(r.run(&mut s, "rows 1").unwrap().len(), 1, "{sql}");
    }
}

#[test]
fn an_unapproved_driver_is_unavailable() {
    let Some(r) = rig("unapproved") else { return };
    r.grants.revoke(&r.key);
    let err = r.connect("fixture://a:b@h/d").err().unwrap();
    assert!(
        matches!(&err, DriverError::Unavailable(t) if t
            == "driver `fixture-db` needs the permission `driver.guest` of plugin `driver-fixture`, which is not approved"),
        "{err:?}"
    );
    assert_eq!(r.registry.available_ids().len(), 0);
}

#[test]
fn refreshing_the_grants_ends_open_sessions() {
    let Some(r) = rig("refresh") else { return };
    let mut s = r.open();
    assert_eq!(r.run(&mut s, "rows 1").unwrap().len(), 1);
    r.grants.revoke(&r.key);
    r.host.refresh_grants(&r.id);
    let err = r.run(&mut s, "rows 1").unwrap_err();
    assert!(
        matches!(&err, DriverError::Unavailable(t) if t == SESSION_ENDED),
        "{err:?}"
    );
    // Closing a session that is already over is not an error.
    block_on(&r.rt, s.close()).unwrap();
}

#[test]
fn disabling_the_plugin_ends_open_sessions_and_hides_the_driver() {
    let Some(r) = rig("disable") else { return };
    let mut s = r.open();
    r.host.disable(&r.id);
    let err = r.run(&mut s, "rows 1").unwrap_err();
    assert!(matches!(err, DriverError::Unavailable(_)), "{err:?}");
    assert_eq!(r.registry.available_ids().len(), 0);
    r.host.enable(&r.id);
    assert_eq!(r.registry.available_ids().len(), 1);
    let mut again = r.open();
    assert_eq!(r.run(&mut again, "rows 2").unwrap().len(), 2);
}

#[test]
fn close_after_a_cancel_between_batches_keeps_the_other_sessions() {
    let Some(r) = rig("cancel-then-close") else {
        return;
    };
    let mut s = r.open();
    let mut other = r.open();
    let handle = s.cancel_handle();
    let q = Query::new("rows 1000");
    block_on(&r.rt, async {
        let mut stream = s.execute(&q).await.unwrap();
        let _ = stream.next_event().await;
        let _ = stream.next_event().await;
        handle.cancel().await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Err(DriverError::Cancelled))
        ));
    });
    block_on(&r.rt, s.close()).unwrap();
    assert_eq!(r.run(&mut other, "rows 2").unwrap().len(), 2);
}
