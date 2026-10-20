// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Browses a real `SQLite` file: expand the tree, generate DDL, open data, drop a table and
//! reload the tree, with the catalog and the browser jobs on separate sessions.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqler_catalog::{CacheKey, Catalog, IntrospectSource, RefreshEvent, RefreshScope};
use sqler_config::ConnectionSpec;
use sqler_driver_api::sqler_core::{ConnectionConfig, DbObject, DriverId, ObjectKind, Query};
use sqler_driver_api::{BoxFuture, DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_sql::{Dialect, Script};
use sqler_ui::browser::{
    ConnectFailure, ConnectRequest, Connector, DropFlow, HostKeyPrompt, ObjectTree, SecretSource,
    TreeNodeKind, generate_script, open_data, run_drop,
};
use sqler_ui::grid::{IngestState, grid_channel};

/// A database file that is removed with the guard.
struct TempDb(PathBuf);

impl TempDb {
    fn new(tag: &str) -> Self {
        let name = format!("sqler-browser-{}-{tag}.db", std::process::id());
        let path = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut name = self.0.clone().into_os_string();
            name.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(name));
        }
    }
}

/// The smallest `Connector` over a driver registry: no secrets, no tunnel.
struct RegistryConnector(Arc<DriverRegistry>);

impl Connector for RegistryConnector {
    fn driver_ids(&self) -> Vec<DriverId> {
        self.0.ids().cloned().collect()
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
        let registry = Arc::clone(&self.0);
        let config = ConnectionConfig::new(
            request.spec.name.clone(),
            request.spec.driver.clone(),
            request.spec.target.clone(),
        );
        Box::pin(async move {
            registry
                .connect(&config)
                .await
                .map_err(ConnectFailure::Driver)
        })
    }

    fn trust_host_key(
        &self,
        _key: HostKeyPrompt,
    ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn request(db: &TempDb) -> ConnectRequest {
    ConnectRequest {
        spec: ConnectionSpec::new(
            "test",
            DriverId::new(DRIVER_ID).unwrap(),
            db.0.to_string_lossy(),
        ),
        db_password: SecretSource::None,
        ssh_secret: SecretSource::None,
    }
}

async fn execute(session: &mut dyn Session, sql: &str) {
    let query = Query::new(sql);
    let mut stream = session.execute(&query).await.unwrap();
    collect(&mut *stream).await.unwrap();
}

/// Answers every key the tree asks for, and the keys those answers ask for.
async fn resolve(catalog: &Catalog, tree: &mut ObjectTree, keys: Vec<CacheKey>) {
    let mut queue: VecDeque<CacheKey> = keys.into();
    while let Some(key) = queue.pop_front() {
        match key {
            CacheKey::Children(node) => {
                let answer = catalog.children(node.clone()).await;
                queue.extend(tree.children_loaded(&node, answer));
            }
            CacheKey::Table(obj) => {
                let answer = catalog.table(obj.clone()).await;
                tree.table_loaded(&obj, answer);
            }
            _ => unreachable!("only children and tables are asked for"),
        }
    }
}

/// Expands everything expandable, level by level.
async fn expand_all(catalog: &Catalog, tree: &mut ObjectTree) {
    for _ in 0..8 {
        let todo: Vec<_> = tree
            .rows()
            .iter()
            .filter(|r| r.expandable && !r.expanded)
            .map(|r| r.id)
            .collect();
        if todo.is_empty() {
            return;
        }
        for id in todo {
            let keys = tree.expand(id);
            resolve(catalog, tree, keys).await;
        }
    }
}

fn labels(tree: &ObjectTree) -> Vec<String> {
    tree.rows().iter().map(|r| r.label.clone()).collect()
}

fn find_table(tree: &ObjectTree, name: &str) -> Option<DbObject> {
    tree.rows().iter().find_map(|row| match tree.kind(row.id) {
        Some(TreeNodeKind::Object(obj)) if obj.kind == ObjectKind::Table && obj.name == name => {
            Some(obj.clone())
        }
        _ => None,
    })
}

#[test]
fn browse_generate_open_drop_and_reload() {
    let db = TempDb::new("flow");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    rt.block_on(async {
        let mut registry = DriverRegistry::new();
        registry.register(Box::new(SqliteDriver::new())).unwrap();
        let connector: Arc<dyn Connector> = Arc::new(RegistryConnector(Arc::new(registry)));
        let req = request(&db);

        let mut setup = connector.connect(req.clone()).await.unwrap();
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE INDEX users_name ON users (name)",
            "CREATE TABLE audit (id INTEGER PRIMARY KEY)",
            "INSERT INTO users (name) VALUES ('a'), ('b'), ('c')",
        ] {
            execute(&mut *setup, sql).await;
        }
        setup.close().await.unwrap();

        // The catalog gets its own session, like the workspace's browser session.
        let session = connector.connect(req.clone()).await.unwrap();
        let (catalog, worker) = Catalog::new(session);
        let worker = tokio::spawn(worker.run());

        let mut tree = ObjectTree::new();
        let root = tree.root();
        let keys = tree.expand(root);
        resolve(&catalog, &mut tree, keys).await;
        expand_all(&catalog, &mut tree).await;
        let shown = labels(&tree);
        assert!(
            shown.iter().any(|l| l.starts_with("Tables (2)")),
            "{shown:?}"
        );
        let users = find_table(&tree, "users").expect("users is listed");
        assert!(find_table(&tree, "audit").is_some());
        assert!(shown.iter().any(|l| l == "Columns (2)"), "{shown:?}");
        assert!(shown.iter().any(|l| l == "name  TEXT"), "{shown:?}");
        assert!(shown.iter().any(|l| l.starts_with("Indexes")), "{shown:?}");

        // Generated scripts: CREATE with the index, SELECT, DROP.
        let script = |s| {
            generate_script(
                Arc::clone(&connector),
                req.clone(),
                catalog.clone(),
                Dialect::Sqlite,
                users.clone(),
                s,
                sqler_driver_api::CancelFlag::new(),
            )
        };
        let create = script(Script::Create).await.unwrap();
        assert!(
            create.contains("CREATE TABLE \"main\".\"users\""),
            "{create}"
        );
        assert!(create.contains("users_name"), "{create}");
        let select = script(Script::SelectStar).await.unwrap();
        let drop_sql = script(Script::Drop).await.unwrap();
        assert_eq!(drop_sql, "DROP TABLE \"main\".\"users\";\n");

        // Open data: the SELECT * text streams into a grid.
        let (mut grid, sink) = grid_channel();
        open_data(Arc::clone(&connector), req.clone(), select, sink)
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while matches!(grid.state(), IngestState::Waiting | IngestState::Streaming) {
            assert!(Instant::now() < deadline, "grid did not finish");
            grid.poll(usize::MAX);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(grid.row_count(), 3);

        // Drop through the confirmation flow, then refresh and reload the tree.
        let id = sqler_config::ConnectionStore::new()
            .add_connection(req.spec.clone(), None)
            .unwrap();
        let mut flow =
            DropFlow::start(id, "test".into(), users.clone(), Dialect::Sqlite, false).unwrap();
        let sql = flow.confirm().expect("not production: one confirmation");
        assert_eq!(sql, drop_sql);
        run_drop(Arc::clone(&connector), req.clone(), sql)
            .await
            .unwrap();

        let mut refresh = catalog.refresh(RefreshScope::All);
        loop {
            match refresh.next_event().await {
                Some(RefreshEvent::Finished(_)) | None => break,
                Some(_) => {}
            }
        }
        let keys = tree.reload_keys();
        resolve(&catalog, &mut tree, keys).await;
        assert!(find_table(&tree, "users").is_none(), "{:?}", labels(&tree));
        assert!(find_table(&tree, "audit").is_some());
        assert!(labels(&tree).iter().any(|l| l.starts_with("Tables (1)")));

        catalog.shutdown();
        if let IntrospectSource::Session(session) = worker.await.unwrap() {
            session.close().await.unwrap();
        }
    });
}

#[test]
fn drop_failure_is_reported_and_the_table_stays() {
    let db = TempDb::new("fail");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    rt.block_on(async {
        let mut registry = DriverRegistry::new();
        registry.register(Box::new(SqliteDriver::new())).unwrap();
        let connector: Arc<dyn Connector> = Arc::new(RegistryConnector(Arc::new(registry)));
        let req = request(&db);
        let mut setup = connector.connect(req.clone()).await.unwrap();
        execute(&mut *setup, "CREATE TABLE t (id INTEGER)").await;
        setup.close().await.unwrap();

        let err = run_drop(
            Arc::clone(&connector),
            req.clone(),
            "DROP TABLE \"missing\";\n".into(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");

        let mut check = connector.connect(req).await.unwrap();
        execute(&mut *check, "SELECT * FROM t").await;
        check.close().await.unwrap();
    });
}
