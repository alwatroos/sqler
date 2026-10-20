// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Snapshots, the name index, column loading and foreign-key targets, against a gated fake.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use sqler_catalog::{
    CacheKey, Catalog, CatalogError, CatalogSnapshot, IntrospectSource, MatchQuality, NameKind,
    NameQuery, NodeKey, RefreshEvent, RefreshHandle, RefreshOutcome, RefreshScope, TableState,
};
use sqler_driver_api::sqler_core::{
    ColumnInfo, DataType, DbObject, ForeignKey, Listing, ObjectKind, ObjectPath, Table,
};
use sqler_driver_api::{BoxFuture, DriverError, Introspect};
use tokio::sync::Notify;
use tokio::task::{JoinHandle, yield_now};

fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(fut)
}

/// Lets spawned tasks run until they wait.
async fn settle() {
    for _ in 0..20 {
        yield_now().await;
    }
}

fn path(schema: &str) -> ObjectPath {
    ObjectPath::root().in_schema(schema)
}

fn main_path() -> ObjectPath {
    path("main")
}

fn obj(kind: ObjectKind, schema: &str, name: &str) -> DbObject {
    DbObject::new(kind, path(schema), name)
}

fn tbl(name: &str) -> DbObject {
    obj(ObjectKind::Table, "main", name)
}

fn tables_key() -> NodeKey {
    NodeKey::new(main_path(), ObjectKind::Table)
}

fn columns_table(object: &DbObject, columns: &[&str]) -> Table {
    let cols = columns
        .iter()
        .map(|c| ColumnInfo::new(*c, DataType::Integer))
        .collect();
    Table::new(object.clone()).with_columns(cols)
}

#[derive(Default)]
struct Inner {
    lists: HashMap<(ObjectPath, ObjectKind), Listing>,
    tables: HashMap<DbObject, Table>,
    failing: Vec<CacheKey>,
    log: Vec<CacheKey>,
    gate: Option<Arc<Notify>>,
}

/// A scripted `Introspect`. The test keeps a clone to change data and read the call log.
#[derive(Clone, Default)]
struct Fake {
    inner: Arc<Mutex<Inner>>,
}

impl Fake {
    fn set_list(&self, parent: &ObjectPath, kind: ObjectKind, objs: Vec<DbObject>) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .lists
            .insert((parent.clone(), kind), Listing::Objects(objs));
    }

    fn set_table(&self, table: Table) {
        let mut inner = self.inner.lock().unwrap();
        inner.tables.insert(table.object.clone(), table);
    }

    fn fail(&self, key: CacheKey) {
        self.inner.lock().unwrap().failing.push(key);
    }

    fn log(&self) -> Vec<CacheKey> {
        self.inner.lock().unwrap().log.clone()
    }

    fn table_calls(&self, object: &DbObject) -> usize {
        let key = CacheKey::Table(object.clone());
        self.log().iter().filter(|k| **k == key).count()
    }

    fn clear_log(&self) {
        self.inner.lock().unwrap().log.clear();
    }

    fn gate(&self) {
        self.inner.lock().unwrap().gate = Some(Arc::new(Notify::new()));
    }

    fn ungate(&self) {
        let gate = self.inner.lock().unwrap().gate.take();
        if let Some(gate) = gate {
            gate.notify_one();
        }
    }

    async fn wait_calls(&self, n: usize) {
        while self.log().len() < n {
            yield_now().await;
        }
    }

    async fn enter(&self, key: CacheKey) -> Result<(), DriverError> {
        let gate = {
            let mut inner = self.inner.lock().unwrap();
            inner.log.push(key.clone());
            inner.gate.clone()
        };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        if self.inner.lock().unwrap().failing.contains(&key) {
            return Err(DriverError::Query("scripted failure".into()));
        }
        Ok(())
    }
}

impl Introspect for Fake {
    fn supports(&self, _kind: ObjectKind) -> bool {
        true
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        Box::pin(async move {
            self.enter(CacheKey::Children(NodeKey::new(parent.clone(), kind)))
                .await?;
            let inner = self.inner.lock().unwrap();
            Ok(inner
                .lists
                .get(&(parent.clone(), kind))
                .cloned()
                .unwrap_or(Listing::Objects(Vec::new())))
        })
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        Box::pin(async move {
            self.enter(CacheKey::Table(table.clone())).await?;
            Ok(self.inner.lock().unwrap().tables.get(table).cloned())
        })
    }

    fn definition<'a>(
        &'a mut self,
        _object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

struct Rig {
    catalog: Catalog,
    fake: Fake,
    join: JoinHandle<IntrospectSource>,
}

/// Must run inside `run(...)`, because the worker is spawned.
fn rig(fake: &Fake) -> Rig {
    let source: Box<dyn Introspect> = Box::new(fake.clone());
    let (catalog, worker) = Catalog::new(source);
    Rig {
        catalog,
        fake: fake.clone(),
        join: tokio::spawn(worker.run()),
    }
}

async fn finish(handle: &mut RefreshHandle) -> RefreshOutcome {
    while let Some(event) = handle.next_event().await {
        if let RefreshEvent::Finished(outcome) = event {
            return outcome;
        }
    }
    panic!("no Finished event");
}

/// Schema `main` with tables `a` (columns `id`, `name`) and `b` (column `id`).
fn standard() -> Fake {
    let fake = Fake::default();
    fake.set_list(&main_path(), ObjectKind::Table, vec![tbl("a"), tbl("b")]);
    fake.set_table(columns_table(&tbl("a"), &["id", "name"]));
    fake.set_table(columns_table(&tbl("b"), &["id"]));
    fake
}

#[test]
fn types_are_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<Catalog>();
    check::<CatalogSnapshot>();
    check::<TableState>();
}

#[test]
fn a_snapshot_during_a_gated_refresh_is_immediate_and_old() {
    run(async {
        let r = rig(&standard());
        r.catalog.children(tables_key()).await.unwrap();
        r.catalog.load_tables(vec![tbl("a"), tbl("b")]).await;
        let before = r.catalog.snapshot();

        r.fake.gate();
        r.fake.clear_log();
        let mut h = r.catalog.refresh(RefreshScope::All);
        r.fake.wait_calls(1).await;
        // The worker is blocked inside the driver; the snapshot is a plain synchronous read.
        let during = r.catalog.snapshot();
        assert_eq!(during.generation(), before.generation());
        assert_eq!(during.lookup(&NameQuery::new("a")).len(), 1);

        r.fake.ungate();
        assert!(matches!(
            finish(&mut h).await,
            RefreshOutcome::Applied { failed: 0, .. }
        ));
        let after = r.catalog.snapshot();
        assert!(after.generation() > before.generation());
        // Equal data keeps its Arc, and the old snapshot stays readable.
        let (old, new) = (
            before.listing(&tables_key()).unwrap(),
            after.listing(&tables_key()).unwrap(),
        );
        assert!(Arc::ptr_eq(&old, &new));
        for name in ["a", "b"] {
            let (TableState::Loaded(x), TableState::Loaded(y)) = (
                before.table_state(&tbl(name)),
                after.table_state(&tbl(name)),
            ) else {
                panic!("not loaded");
            };
            assert!(Arc::ptr_eq(&x, &y), "{name}");
        }
        assert_eq!(before.lookup(&NameQuery::new("na")).len(), 1);
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn a_changed_entry_is_replaced_and_old_snapshots_keep_the_old_value() {
    run(async {
        let r = rig(&standard());
        r.catalog.children(tables_key()).await.unwrap();
        r.catalog.load_tables(vec![tbl("a"), tbl("b")]).await;
        let before = r.catalog.snapshot();
        r.fake
            .set_table(columns_table(&tbl("a"), &["id", "renamed"]));
        let mut h = r.catalog.refresh(RefreshScope::All);
        finish(&mut h).await;
        let after = r.catalog.snapshot();

        let TableState::Loaded(old) = before.table_state(&tbl("a")) else {
            panic!("not loaded");
        };
        let TableState::Loaded(new) = after.table_state(&tbl("a")) else {
            panic!("not loaded");
        };
        assert_eq!(old.columns[1].name, "name");
        assert_eq!(new.columns[1].name, "renamed");
        assert_eq!(before.lookup(&NameQuery::new("renamed")).len(), 0);
        assert_eq!(after.lookup(&NameQuery::new("renamed")).len(), 1);
        assert_eq!(after.lookup(&NameQuery::new("name")).len(), 0);
        // The untouched table is shared.
        let (TableState::Loaded(x), TableState::Loaded(y)) =
            (before.table_state(&tbl("b")), after.table_state(&tbl("b")))
        else {
            panic!("not loaded");
        };
        assert!(Arc::ptr_eq(&x, &y));
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

fn big_fake() -> Fake {
    let fake = Fake::default();
    let names = ["users", "user_roles", "Orders", "order_items", "tmp_users"];
    let main_tables: Vec<DbObject> = names.iter().map(|n| tbl(n)).collect();
    fake.set_list(&main_path(), ObjectKind::Table, main_tables);
    fake.set_list(
        &main_path(),
        ObjectKind::View,
        vec![obj(ObjectKind::View, "main", "user_summary")],
    );
    fake.set_list(
        &path("audit"),
        ObjectKind::Table,
        vec![obj(ObjectKind::Table, "audit", "users_log")],
    );
    fake.set_table(columns_table(&tbl("users"), &["id", "user_name", "email"]));
    fake.set_table(columns_table(
        &obj(ObjectKind::Table, "audit", "users_log"),
        &["user_id"],
    ));
    fake
}

async fn load_big(r: &Rig) {
    r.catalog.children(tables_key()).await.unwrap();
    r.catalog
        .children(NodeKey::new(main_path(), ObjectKind::View))
        .await
        .unwrap();
    r.catalog
        .children(NodeKey::new(path("audit"), ObjectKind::Table))
        .await
        .unwrap();
    r.catalog
        .load_tables(vec![
            tbl("users"),
            obj(ObjectKind::Table, "audit", "users_log"),
        ])
        .await;
}

#[test]
fn prefix_lookup_is_scoped_bounded_and_ranked() {
    run(async {
        let r = rig(&big_fake());
        load_big(&r).await;
        let s = r.catalog.snapshot();

        let names =
            |q: &NameQuery| -> Vec<String> { s.lookup(q).into_iter().map(|h| h.name).collect() };
        assert_eq!(
            names(&NameQuery::new("USER")),
            [
                "users",
                "user_id",
                "user_name",
                "users_log",
                "user_roles",
                "user_summary"
            ]
        );
        let exact = s.lookup(&NameQuery::new("users"));
        assert_eq!(exact[0].quality, MatchQuality::Exact);
        assert_eq!(exact[0].parent, main_path());
        // Scoped to a schema.
        let audit = NameQuery::new("user").with_scope(path("audit"));
        assert_eq!(names(&audit), ["user_id", "users_log"]);
        // Scoped to one table: only its columns.
        let cols = NameQuery::new("").with_scope(main_path().in_table("users"));
        assert_eq!(names(&cols), ["id", "email", "user_name"]);
        // Kinds.
        let views = NameQuery::new("user").with_kinds(vec![NameKind::Object(ObjectKind::View)]);
        assert_eq!(names(&views), ["user_summary"]);
        // Bounded, and the bound keeps the best.
        let top = s.lookup(&NameQuery::new("user").with_limit(2));
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].name, "users");
        // Case-insensitive object match keeps the original case.
        assert_eq!(names(&NameQuery::new("ord")), ["Orders", "order_items"]);
        // Paths are not normalised: a scope in a schema nobody listed under matches nothing.
        let none = NameQuery::new("").with_scope(path("nowhere"));
        assert_eq!(s.lookup(&none).len(), 0);
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn fuzzy_lookup_matches_subsequences_after_better_matches() {
    run(async {
        let r = rig(&big_fake());
        load_big(&r).await;
        let s = r.catalog.snapshot();
        let hits = s.lookup(&NameQuery::new("usr").fuzzy());
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"users"), "{names:?}");
        assert!(names.contains(&"user_name"), "{names:?}");
        // Quality never gets better down the list.
        assert!(hits.windows(2).all(|w| w[0].quality <= w[1].quality));
        let oi = s.lookup(&NameQuery::new("oitm").fuzzy());
        assert_eq!(oi[0].name, "order_items");
        assert_eq!(oi[0].quality, MatchQuality::Fuzzy);
        // Prefix mode does not find it.
        assert_eq!(s.lookup(&NameQuery::new("oitm")).len(), 0);
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn not_loaded_loaded_empty_and_failed_are_distinct() {
    run(async {
        let fake = standard();
        fake.set_table(columns_table(&tbl("empty"), &[]));
        fake.fail(CacheKey::Table(tbl("b")));
        let r = rig(&fake);
        for name in ["a", "b", "empty"] {
            assert_eq!(
                r.catalog.snapshot().table_state(&tbl(name)),
                TableState::NotLoaded
            );
        }
        let results = r
            .catalog
            .load_tables(vec![tbl("empty"), tbl("b"), tbl("missing")])
            .await;
        assert!(matches!(results[0].1, Ok(Some(_))));
        assert!(matches!(results[1].1, Err(CatalogError::Driver(_))));
        assert_eq!(results[2].1, Ok(None));
        let s = r.catalog.snapshot();
        match s.table_state(&tbl("empty")) {
            TableState::Loaded(t) => assert_eq!(t.columns.len(), 0),
            other => panic!("{other:?}"),
        }
        assert!(matches!(s.table_state(&tbl("b")), TableState::Failed(_)));
        assert_eq!(s.table_state(&tbl("a")), TableState::NotLoaded);
        assert_eq!(s.table_state(&tbl("missing")), TableState::NotLoaded);
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn load_tables_costs_one_call_per_table() {
    run(async {
        let r = rig(&standard());
        let table_call = r.catalog.table(tbl("a"));
        let batch = r
            .catalog
            .load_tables(vec![tbl("a"), tbl("b"), tbl("a"), tbl("b")]);
        // The batch has already queued its loads; the single call queues behind them.
        let single = table_call.await;
        let batch = batch.await;
        assert!(single.unwrap().is_some());
        assert_eq!(batch.len(), 4);
        let order: Vec<&str> = batch.iter().map(|(o, _)| o.name.as_str()).collect();
        assert_eq!(order, ["a", "b", "a", "b"]);
        assert!(batch.iter().all(|(_, r)| matches!(r, Ok(Some(_)))));
        assert_eq!(r.fake.table_calls(&tbl("a")), 1);
        assert_eq!(r.fake.table_calls(&tbl("b")), 1);
        // Loaded tables are answered at once.
        r.fake.clear_log();
        r.catalog.load_tables(vec![tbl("a"), tbl("b")]).await;
        assert_eq!(r.fake.log().len(), 0);
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn a_dropped_load_tables_future_fetches_nothing_not_yet_started() {
    run(async {
        let fake = standard();
        fake.set_table(columns_table(&tbl("c"), &["id"]));
        let r = rig(&fake);
        r.fake.gate();
        let pending = r.catalog.load_tables(vec![tbl("a"), tbl("b"), tbl("c")]);
        r.fake.wait_calls(1).await;
        drop(pending);
        r.fake.ungate();
        settle().await;
        assert_eq!(r.fake.log().len(), 1, "{:?}", r.fake.log());
        // The catalog still works afterwards.
        assert!(r.catalog.table(tbl("b")).await.unwrap().is_some());
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn a_refresh_after_a_drop_removes_hits_and_table_state() {
    run(async {
        let r = rig(&standard());
        r.catalog.children(tables_key()).await.unwrap();
        r.catalog.load_tables(vec![tbl("a"), tbl("b")]).await;
        let before = r.catalog.snapshot();
        assert_eq!(before.lookup(&NameQuery::new("b")).len(), 1);

        r.fake
            .set_list(&main_path(), ObjectKind::Table, vec![tbl("a")]);
        let mut h = r
            .catalog
            .refresh(RefreshScope::Node(CacheKey::Children(tables_key())));
        finish(&mut h).await;
        let after = r.catalog.snapshot();

        assert_eq!(after.table_state(&tbl("b")), TableState::NotLoaded);
        assert_eq!(after.lookup(&NameQuery::new("b")).len(), 0);
        assert_eq!(after.lookup(&NameQuery::new("a")).len(), 1);
        assert!(matches!(
            after.table_state(&tbl("a")),
            TableState::Loaded(_)
        ));
        // The earlier snapshot is untouched.
        assert!(matches!(
            before.table_state(&tbl("b")),
            TableState::Loaded(_)
        ));
        assert_eq!(before.lookup(&NameQuery::new("b")).len(), 1);
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn foreign_keys_resolve_with_and_without_a_referenced_schema() {
    run(async {
        let fake = Fake::default();
        let orders = tbl("orders");
        let fks = vec![
            ForeignKey::new(vec!["customer_id".into()], "customers", vec!["id".into()])
                .with_name("fk_customer"),
            ForeignKey::new(vec!["acct".into()], "accounts", vec!["id".into()])
                .with_referenced_schema("billing"),
        ];
        fake.set_table(columns_table(&orders, &["customer_id", "acct"]).with_foreign_keys(fks));
        let r = rig(&fake);
        assert_eq!(r.catalog.snapshot().foreign_keys(&orders), None);
        r.catalog.table(orders.clone()).await.unwrap();
        let s = r.catalog.snapshot();
        let got = s.foreign_keys(&orders).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].target, tbl("customers"));
        assert_eq!(got[0].name.as_deref(), Some("fk_customer"));
        assert_eq!(got[0].columns, ["customer_id"]);
        assert_eq!(got[0].referenced_columns, ["id"]);
        assert_eq!(got[1].target, obj(ObjectKind::Table, "billing", "accounts"));
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn find_relations_ignores_case_and_respects_scope() {
    run(async {
        let r = rig(&big_fake());
        load_big(&r).await;
        let s = r.catalog.snapshot();
        let all = s.find_relations(&ObjectPath::root(), "USERS");
        assert_eq!(all, vec![tbl("users")]);
        let view = s.find_relations(&main_path(), "User_Summary");
        assert_eq!(view, vec![obj(ObjectKind::View, "main", "user_summary")]);
        assert_eq!(s.find_relations(&path("audit"), "users").len(), 0);
        assert!(s.index_bytes() > 0);
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}

#[test]
fn names_differing_only_in_case_have_a_stable_total_order() {
    run(async {
        let fake = Fake::default();
        fake.set_list(
            &main_path(),
            ObjectKind::Table,
            vec![tbl("users"), tbl("Users")],
        );
        let r = rig(&fake);
        r.catalog.children(tables_key()).await.unwrap();
        for _ in 0..20 {
            // A fresh snapshot each time: shard iteration order is randomised per index.
            let s = r.catalog.snapshot();
            let one = s.lookup(&NameQuery::new("users").with_limit(1));
            assert_eq!(one.len(), 1);
            assert_eq!(one[0].name, "Users");
            let both: Vec<String> = s
                .lookup(&NameQuery::new("users"))
                .into_iter()
                .map(|h| h.name)
                .collect();
            assert_eq!(both, ["Users", "users"]);
        }
        r.catalog.shutdown();
        r.join.await.unwrap();
    });
}
