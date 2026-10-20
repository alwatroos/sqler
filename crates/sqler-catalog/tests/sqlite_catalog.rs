// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The catalog on the real `SQLite` driver. A file database lets two sessions share the data:
//! session A runs the DDL, the catalog owns session B. No Docker.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::path::PathBuf;

use sqler_catalog::{
    CacheKey, Catalog, IntrospectSource, NodeKey, RefreshEvent, RefreshOutcome, RefreshScope,
};
use sqler_driver_api::sqler_core::{
    ConnectionConfig, DataType, DbObject, DriverId, Listing, ObjectKind, ObjectPath, Query,
};
use sqler_driver_api::{DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};

fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

/// A unique database file that is removed on drop, also when a test fails.
struct TempDb(PathBuf);

impl TempDb {
    fn new(tag: &str) -> Self {
        let name = format!("sqler-catalog-{tag}-{}.db", std::process::id());
        Self(std::env::temp_dir().join(name))
    }

    async fn open(&self) -> Box<dyn Session> {
        let mut registry = DriverRegistry::new();
        registry.register(Box::new(SqliteDriver::new())).unwrap();
        let id = DriverId::new(DRIVER_ID).unwrap();
        let target = self.0.to_str().unwrap();
        registry
            .connect(&ConnectionConfig::new("t", id, target))
            .await
            .unwrap()
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

async fn exec(s: &mut dyn Session, text: &str) {
    let q = Query::new(text);
    let mut stream = s.execute(&q).await.unwrap();
    collect(&mut *stream).await.unwrap();
}

fn main_path() -> ObjectPath {
    ObjectPath::root().in_schema("main")
}

fn names(listing: &Listing) -> Vec<String> {
    match listing {
        Listing::Objects(o) => o.iter().map(|o| o.name.clone()).collect(),
        Listing::Unsupported => panic!("unsupported"),
    }
}

#[test]
fn lists_schemas_tables_columns_and_unsupported_kinds() {
    let db = TempDb::new("list");
    run(async {
        let mut a = db.open().await;
        exec(
            &mut *a,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        )
        .await;
        exec(
            &mut *a,
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER)",
        )
        .await;
        let (catalog, worker) = Catalog::new(db.open().await);
        let join = tokio::spawn(worker.run());

        let schemas = NodeKey::new(ObjectPath::root(), ObjectKind::Schema);
        assert_eq!(
            names(&catalog.children(schemas.clone()).await.unwrap()),
            ["main"]
        );
        let tables = NodeKey::new(main_path(), ObjectKind::Table);
        let listing = catalog.children(tables.clone()).await.unwrap();
        assert_eq!(names(&listing), ["orders", "users"]);
        assert!(catalog.cached(&tables).is_some());

        let users = DbObject::new(ObjectKind::Table, main_path(), "users");
        let table = catalog.table(users.clone()).await.unwrap().unwrap();
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.columns[0].name, "id");
        assert_eq!(table.columns[0].data_type, DataType::Integer);
        assert!(!table.columns[1].nullable);
        assert_eq!(table.primary_key.as_ref().unwrap().columns, ["id"]);
        assert!(catalog.cached_table(&users).is_some());

        let roles = NodeKey::new(ObjectPath::root(), ObjectKind::Role);
        let unsupported = catalog.children(roles.clone()).await.unwrap();
        assert_eq!(*unsupported, Listing::Unsupported);
        assert!(catalog.cached(&roles).is_some());
        assert_eq!(catalog.cached_len(), 4);

        catalog.shutdown();
        let IntrospectSource::Session(session) = join.await.unwrap() else {
            panic!("expected the session back");
        };
        session.close().await.unwrap();
        a.close().await.unwrap();
    });
}

#[test]
fn a_refresh_shows_tables_created_and_dropped_by_another_session() {
    let db = TempDb::new("refresh");
    run(async {
        let mut a = db.open().await;
        exec(&mut *a, "CREATE TABLE keep (id INTEGER)").await;
        exec(&mut *a, "CREATE TABLE gone (id INTEGER)").await;
        let (catalog, worker) = Catalog::new(db.open().await);
        let join = tokio::spawn(worker.run());

        let tables = NodeKey::new(main_path(), ObjectKind::Table);
        catalog.children(tables.clone()).await.unwrap();
        let gone = DbObject::new(ObjectKind::Table, main_path(), "gone");
        catalog.table(gone.clone()).await.unwrap().unwrap();
        assert_eq!(catalog.cached_len(), 2);

        exec(&mut *a, "DROP TABLE gone").await;
        exec(&mut *a, "CREATE TABLE fresh (id INTEGER, extra TEXT)").await;

        // Still the old snapshot until the refresh runs.
        assert_eq!(names(&catalog.cached(&tables).unwrap()), ["gone", "keep"]);
        let mut handle = catalog.refresh(RefreshScope::Subtree(main_path()));
        let mut last = None;
        while let Some(event) = handle.next_event().await {
            last = Some(event);
            if matches!(last, Some(RefreshEvent::Finished(_))) {
                break;
            }
        }
        assert_eq!(
            last,
            Some(RefreshEvent::Finished(RefreshOutcome::Applied {
                refreshed: 2,
                failed: 0
            }))
        );
        assert_eq!(names(&catalog.cached(&tables).unwrap()), ["fresh", "keep"]);
        // The dropped table was pruned with its details.
        assert!(catalog.cached_table(&gone).is_none());
        assert_eq!(catalog.node_error(&CacheKey::Table(gone)), None);
        let fresh = DbObject::new(ObjectKind::Table, main_path(), "fresh");
        let table = catalog.table(fresh).await.unwrap().unwrap();
        assert_eq!(table.columns.len(), 2);

        catalog.shutdown();
        let IntrospectSource::Session(session) = join.await.unwrap() else {
            panic!("expected the session back");
        };
        session.close().await.unwrap();
        a.close().await.unwrap();
    });
}
