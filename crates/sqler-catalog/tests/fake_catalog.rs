// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The catalog against a scripted fake `Introspect`: laziness, snapshots, dedup, errors.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};

use sqler_catalog::{
    CacheKey, Catalog, CatalogError, IntrospectSource, NodeKey, RefreshEvent, RefreshHandle,
    RefreshOutcome, RefreshScope,
};
use sqler_driver_api::sqler_core::{
    ColumnInfo, DataType, DbObject, Listing, ObjectKind, ObjectPath, Query, Table,
};
use sqler_driver_api::{BoxFuture, CancelHandle, DriverError, Introspect, QueryStream, Session};
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

fn main_path() -> ObjectPath {
    ObjectPath::root().in_schema("main")
}

fn schema(name: &str) -> DbObject {
    DbObject::new(ObjectKind::Schema, ObjectPath::root(), name)
}

fn tbl(name: &str) -> DbObject {
    DbObject::new(ObjectKind::Table, main_path(), name)
}

fn schemas_key() -> NodeKey {
    NodeKey::new(ObjectPath::root(), ObjectKind::Schema)
}

fn tables_key() -> NodeKey {
    NodeKey::new(main_path(), ObjectKind::Table)
}

fn index_key(table: &str) -> NodeKey {
    NodeKey::new(main_path().in_table(table), ObjectKind::Index)
}

fn objects(listing: &Listing) -> Vec<String> {
    match listing {
        Listing::Objects(o) => o.iter().map(|o| o.name.clone()).collect(),
        Listing::Unsupported => panic!("unsupported"),
    }
}

#[derive(Default)]
struct Inner {
    lists: HashMap<(ObjectPath, ObjectKind), Listing>,
    tables: HashMap<DbObject, Table>,
    failing: HashSet<CacheKey>,
    log: Vec<CacheKey>,
    gate: Option<Arc<Notify>>,
}

/// A scripted `Introspect`. The test keeps a clone to change data and read the call log.
#[derive(Clone, Default)]
struct Fake {
    inner: Arc<Mutex<Inner>>,
}

impl Fake {
    /// Schemas `main`; tables `a`, `b` with one column each; an index on `a`.
    fn standard() -> Self {
        let fake = Self::default();
        fake.set_list(
            &ObjectPath::root(),
            ObjectKind::Schema,
            vec![schema("main")],
        );
        fake.set_list(&main_path(), ObjectKind::Table, vec![tbl("a"), tbl("b")]);
        for name in ["a", "b"] {
            fake.set_table(&tbl(name), "id");
        }
        fake.set_list(
            &main_path().in_table("a"),
            ObjectKind::Index,
            vec![DbObject::new(
                ObjectKind::Index,
                main_path().in_table("a"),
                "idx_a",
            )],
        );
        fake
    }

    fn set_list(&self, parent: &ObjectPath, kind: ObjectKind, objs: Vec<DbObject>) {
        self.inner
            .lock()
            .unwrap()
            .lists
            .insert((parent.clone(), kind), Listing::Objects(objs));
    }

    fn set_table(&self, obj: &DbObject, column: &str) {
        let table =
            Table::new(obj.clone()).with_columns(vec![ColumnInfo::new(column, DataType::Integer)]);
        self.inner.lock().unwrap().tables.insert(obj.clone(), table);
    }

    fn fail(&self, key: CacheKey) {
        self.inner.lock().unwrap().failing.insert(key);
    }

    fn unfail(&self, key: &CacheKey) {
        self.inner.lock().unwrap().failing.remove(key);
    }

    fn log(&self) -> Vec<CacheKey> {
        self.inner.lock().unwrap().log.clone()
    }

    fn clear_log(&self) {
        self.inner.lock().unwrap().log.clear();
    }

    /// Makes every following call wait until released.
    fn gate(&self) {
        self.inner.lock().unwrap().gate = Some(Arc::new(Notify::new()));
    }

    /// Lets one waiting (or the next) call through.
    fn release_one(&self) {
        let gate = self.inner.lock().unwrap().gate.clone();
        gate.unwrap().notify_one();
    }

    /// Removes the gate and lets the waiting call through.
    fn ungate(&self) {
        let gate = self.inner.lock().unwrap().gate.take();
        if let Some(gate) = gate {
            gate.notify_one();
        }
    }

    /// Waits until the call log has at least `n` entries.
    async fn wait_calls(&self, n: usize) {
        while self.log().len() < n {
            yield_now().await;
        }
    }

    /// Logs the call, waits at the gate, then returns the scripted outcome.
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
    fn supports(&self, kind: ObjectKind) -> bool {
        !matches!(kind, ObjectKind::Role)
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        Box::pin(async move {
            self.enter(CacheKey::Children(NodeKey::new(parent.clone(), kind)))
                .await?;
            if !self.supports(kind) {
                return Ok(Listing::Unsupported);
            }
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

async fn events(handle: &mut RefreshHandle) -> Vec<RefreshEvent> {
    let mut out = Vec::new();
    while let Some(event) = handle.next_event().await {
        let last = matches!(event, RefreshEvent::Finished(_));
        out.push(event);
        if last {
            break;
        }
    }
    out
}

/// Started, then Progress with rising `done`, then Applied.
fn assert_applied(events: &[RefreshEvent], total: usize, failed: usize) {
    assert_eq!(events.first(), Some(&RefreshEvent::Started { total }));
    assert_eq!(
        events.last(),
        Some(&RefreshEvent::Finished(RefreshOutcome::Applied {
            refreshed: total - failed,
            failed
        }))
    );
    let progress: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            RefreshEvent::Progress { done, total: t } => {
                assert_eq!(*t, total);
                Some(*done)
            }
            _ => None,
        })
        .collect();
    assert_eq!(progress, (1..=total).collect::<Vec<_>>());
    for e in &events[1..events.len() - 1] {
        assert!(
            matches!(
                e,
                RefreshEvent::Progress { .. } | RefreshEvent::NodeFailed { .. }
            ),
            "{e:?}"
        );
    }
}

/// Loads the schema list, the table list, both tables and the index list of `a`.
async fn load_all(c: &Catalog) {
    c.children(schemas_key()).await.unwrap();
    c.children(tables_key()).await.unwrap();
    c.table(tbl("a")).await.unwrap();
    c.table(tbl("b")).await.unwrap();
    c.children(index_key("a")).await.unwrap();
}

#[test]
fn a_load_fetches_only_its_node_and_caches_it() {
    run(async {
        let r = rig(&Fake::standard());
        assert!(r.catalog.cached(&tables_key()).is_none());
        let listing = r.catalog.children(tables_key()).await.unwrap();
        assert_eq!(objects(&listing), ["a", "b"]);
        assert_eq!(r.fake.log(), [CacheKey::Children(tables_key())]);
        assert_eq!(r.catalog.cached_len(), 1);

        let again = r.catalog.children(tables_key()).await.unwrap();
        assert!(Arc::ptr_eq(&listing, &again));
        assert!(Arc::ptr_eq(
            &listing,
            &r.catalog.cached(&tables_key()).unwrap()
        ));
        assert_eq!(r.fake.log().len(), 1, "a cached read made a call");

        let table = r.catalog.table(tbl("a")).await.unwrap().unwrap();
        assert_eq!(table.columns[0].name, "id");
        assert!(r.catalog.cached_table(&tbl("a")).is_some());
        assert!(r.catalog.cached_table(&tbl("b")).is_none());
        r.catalog.table(tbl("a")).await.unwrap();
        assert_eq!(r.fake.log().len(), 2);
        assert_eq!(r.catalog.cached_len(), 2);
    });
}

#[test]
fn a_missing_table_is_not_cached() {
    run(async {
        let r = rig(&Fake::standard());
        assert!(r.catalog.table(tbl("nope")).await.unwrap().is_none());
        assert!(r.catalog.table(tbl("nope")).await.unwrap().is_none());
        assert_eq!(r.fake.log().len(), 2);
        assert_eq!(r.catalog.cached_len(), 0);
    });
}

#[test]
fn concurrent_misses_cost_one_call() {
    run(async {
        let r = rig(&Fake::standard());
        r.fake.gate();
        let first = tokio::spawn(r.catalog.children(tables_key()));
        let second = tokio::spawn(r.catalog.clone().children(tables_key()));
        let third = tokio::spawn(r.catalog.table(tbl("a")));
        r.fake.wait_calls(1).await;
        settle().await;
        assert_eq!(r.fake.log(), [CacheKey::Children(tables_key())]);
        r.fake.release_one();
        let (first, second) = (
            first.await.unwrap().unwrap(),
            second.await.unwrap().unwrap(),
        );
        assert!(Arc::ptr_eq(&first, &second));
        r.fake.ungate();
        assert!(third.await.unwrap().unwrap().is_some());
        assert_eq!(
            r.fake.log(),
            [CacheKey::Children(tables_key()), CacheKey::Table(tbl("a"))]
        );
    });
}

#[test]
fn errors_are_per_node_and_a_retry_clears_them() {
    run(async {
        let r = rig(&Fake::standard());
        r.catalog.children(tables_key()).await.unwrap();
        let bad = CacheKey::Table(tbl("a"));
        r.fake.fail(bad.clone());
        let err = r.catalog.table(tbl("a")).await.unwrap_err();
        assert_eq!(
            err,
            CatalogError::Driver("query failed: scripted failure".into())
        );
        assert_eq!(r.catalog.node_error(&bad), Some(err));
        // The other nodes are intact and still load.
        assert!(r.catalog.cached(&tables_key()).is_some());
        assert!(r.catalog.table(tbl("b")).await.unwrap().is_some());
        assert_eq!(r.catalog.node_error(&CacheKey::Table(tbl("b"))), None);
        assert_eq!(r.catalog.cached_len(), 2);
        // A failed load is not cached: the next request retries and succeeds.
        r.fake.unfail(&bad);
        assert!(r.catalog.table(tbl("a")).await.unwrap().is_some());
        assert_eq!(r.catalog.node_error(&bad), None);
    });
}

#[test]
fn a_source_without_introspection_reports_it() {
    struct Bare;
    impl Session for Bare {
        fn cancel_handle(&self) -> CancelHandle {
            sqler_driver_api::CancelFlag::new().handle()
        }
        fn execute<'s>(
            &'s mut self,
            _query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            Box::pin(std::future::ready(Err(DriverError::Unsupported(
                "no queries".into(),
            ))))
        }
        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }
    run(async {
        let session: Box<dyn Session> = Box::new(Bare);
        let (catalog, worker) = Catalog::new(session);
        let join = tokio::spawn(worker.run());
        let err = catalog.children(schemas_key()).await.unwrap_err();
        assert_eq!(err, CatalogError::NoIntrospection);
        assert_eq!(
            catalog.node_error(&CacheKey::Children(schemas_key())),
            Some(CatalogError::NoIntrospection)
        );
        catalog.shutdown();
        assert!(matches!(join.await.unwrap(), IntrospectSource::Session(_)));
    });
}

#[test]
fn refresh_scopes_refetch_only_cached_keys_in_scope() {
    run(async {
        let r = rig(&Fake::standard());
        load_all(&r.catalog).await;
        assert_eq!(r.catalog.cached_len(), 5);

        // Node: exactly that key, in scope even when it is not cached.
        r.fake.clear_log();
        let uncached = CacheKey::Children(index_key("b"));
        let mut h = r.catalog.refresh(RefreshScope::Node(uncached.clone()));
        assert_applied(&events(&mut h).await, 1, 0);
        assert_eq!(r.fake.log(), [uncached]);
        assert_eq!(r.catalog.cached_len(), 6);

        r.fake.clear_log();
        let mut h = r
            .catalog
            .refresh(RefreshScope::Node(CacheKey::Children(tables_key())));
        assert_applied(&events(&mut h).await, 1, 0);
        assert_eq!(r.fake.log(), [CacheKey::Children(tables_key())]);

        // Subtree: the cached keys within the path, parents first, not the schema list.
        r.fake.clear_log();
        let mut h = r.catalog.refresh(RefreshScope::Subtree(main_path()));
        assert_applied(&events(&mut h).await, 5, 0);
        let log = r.fake.log();
        assert_eq!(log.len(), 5);
        assert_eq!(log[0], CacheKey::Children(tables_key()));
        assert!(!log.contains(&CacheKey::Children(schemas_key())));
        assert!(log.windows(2).all(|w| w[0].depth() <= w[1].depth()));
        r.fake.clear_log();
        let mut h = r
            .catalog
            .refresh(RefreshScope::Subtree(main_path().in_table("a")));
        assert_applied(&events(&mut h).await, 2, 0);
        assert_eq!(
            r.fake.log().into_iter().collect::<HashSet<_>>(),
            HashSet::from([
                CacheKey::Table(tbl("a")),
                CacheKey::Children(index_key("a"))
            ])
        );

        // All, and the root subtree, are the same.
        r.fake.clear_log();
        let mut h = r.catalog.refresh(RefreshScope::All);
        assert_applied(&events(&mut h).await, 6, 0);
        assert_eq!(r.fake.log().len(), 6);
        r.fake.clear_log();
        let mut h = r.catalog.refresh(RefreshScope::Subtree(ObjectPath::root()));
        assert_applied(&events(&mut h).await, 6, 0);

        // Nothing cached in scope: no calls.
        r.fake.clear_log();
        let other = ObjectPath::root().in_schema("other");
        let mut h = r.catalog.refresh(RefreshScope::Subtree(other));
        assert_applied(&events(&mut h).await, 0, 0);
        assert_eq!(r.fake.log().len(), 0);
    });
}

#[test]
fn readers_keep_the_old_snapshot_until_the_refresh_is_applied() {
    run(async {
        let r = rig(&Fake::standard());
        load_all(&r.catalog).await;
        let old = r.catalog.cached(&tables_key()).unwrap();
        r.fake.set_list(
            &main_path(),
            ObjectKind::Table,
            vec![tbl("a"), tbl("b"), tbl("c")],
        );
        r.fake.set_table(&tbl("a"), "renamed");
        r.fake.gate();
        r.fake.clear_log();
        let mut h = r.catalog.refresh(RefreshScope::Subtree(main_path()));
        r.fake.wait_calls(1).await;
        assert_eq!(
            h.next_event().await,
            Some(RefreshEvent::Started { total: 4 })
        );

        // All but the last call finish; nothing is visible before the end.
        for _ in 0..3 {
            r.fake.release_one();
            assert!(matches!(
                h.next_event().await,
                Some(RefreshEvent::Progress { .. })
            ));
            assert!(Arc::ptr_eq(&old, &r.catalog.cached(&tables_key()).unwrap()));
            assert_eq!(
                r.catalog.cached_table(&tbl("a")).unwrap().columns[0].name,
                "id"
            );
        }
        r.fake.ungate();
        let rest = events(&mut h).await;
        assert!(matches!(
            rest.last(),
            Some(RefreshEvent::Finished(RefreshOutcome::Applied { .. }))
        ));
        assert_eq!(
            objects(&r.catalog.cached(&tables_key()).unwrap()),
            ["a", "b", "c"]
        );
        assert_eq!(
            r.catalog.cached_table(&tbl("a")).unwrap().columns[0].name,
            "renamed"
        );
    });
}

#[test]
fn cancel_while_gated_leaves_the_store_unchanged() {
    run(async {
        let r = rig(&Fake::standard());
        load_all(&r.catalog).await;
        let old = r.catalog.cached(&tables_key()).unwrap();
        r.fake
            .set_list(&main_path(), ObjectKind::Table, vec![tbl("z")]);
        r.fake.gate();
        let mut h = r.catalog.refresh(RefreshScope::All);
        r.fake.wait_calls(6).await;
        h.cancel();
        assert!(h.cancel_flag().is_cancelled());
        r.fake.ungate();
        let all = events(&mut h).await;
        assert_eq!(
            all.last(),
            Some(&RefreshEvent::Finished(RefreshOutcome::Cancelled))
        );
        assert!(Arc::ptr_eq(&old, &r.catalog.cached(&tables_key()).unwrap()));
        assert_eq!(r.catalog.cached_len(), 5);
        assert_eq!(h.next_event().await, None);
        // The worker still serves loads after a cancel.
        assert!(r.catalog.children(index_key("b")).await.is_ok());
    });
}

#[test]
fn a_node_failing_during_a_refresh_keeps_its_old_data() {
    run(async {
        let r = rig(&Fake::standard());
        load_all(&r.catalog).await;
        let old_b = r.catalog.cached_table(&tbl("b")).unwrap();
        r.fake.set_list(
            &main_path(),
            ObjectKind::Table,
            vec![tbl("a"), tbl("b"), tbl("c")],
        );
        r.fake.set_table(&tbl("a"), "renamed");
        let bad = CacheKey::Table(tbl("b"));
        r.fake.fail(bad.clone());
        let mut h = r.catalog.refresh(RefreshScope::Subtree(main_path()));
        let all = events(&mut h).await;
        assert!(all.contains(&RefreshEvent::NodeFailed {
            key: bad.clone(),
            error: CatalogError::Driver("query failed: scripted failure".into()),
        }));
        assert_applied(&all, 4, 1);
        assert_eq!(
            objects(&r.catalog.cached(&tables_key()).unwrap()),
            ["a", "b", "c"]
        );
        assert_eq!(
            r.catalog.cached_table(&tbl("a")).unwrap().columns[0].name,
            "renamed"
        );
        assert!(Arc::ptr_eq(
            &old_b,
            &r.catalog.cached_table(&tbl("b")).unwrap()
        ));
        assert!(r.catalog.node_error(&bad).is_some());
        // The next successful refresh of that node clears the error.
        r.fake.unfail(&bad);
        let mut h = r.catalog.refresh(RefreshScope::Node(bad.clone()));
        assert_applied(&events(&mut h).await, 1, 0);
        assert_eq!(r.catalog.node_error(&bad), None);
    });
}

#[test]
fn pruning_a_dropped_table_evicts_its_details_and_index_lists() {
    run(async {
        let r = rig(&Fake::standard());
        load_all(&r.catalog).await;
        r.catalog.children(index_key("b")).await.unwrap();
        assert_eq!(r.catalog.cached_len(), 6);
        r.fake
            .set_list(&main_path(), ObjectKind::Table, vec![tbl("a")]);
        let mut h = r
            .catalog
            .refresh(RefreshScope::Node(CacheKey::Children(tables_key())));
        assert_applied(&events(&mut h).await, 1, 0);
        assert!(r.catalog.cached_table(&tbl("b")).is_none());
        assert!(r.catalog.cached(&index_key("b")).is_none());
        assert!(r.catalog.cached_table(&tbl("a")).is_some());
        assert!(r.catalog.cached(&index_key("a")).is_some());
        assert!(r.catalog.cached(&schemas_key()).is_some());
        assert_eq!(r.catalog.cached_len(), 4);
    });
}

#[test]
fn a_load_during_a_gated_refresh_is_served_before_it_finishes() {
    run(async {
        let r = rig(&Fake::standard());
        load_all(&r.catalog).await;
        r.fake.clear_log();
        r.fake.gate();
        let mut h = r.catalog.refresh(RefreshScope::Subtree(main_path()));
        r.fake.wait_calls(1).await;
        let load = tokio::spawn(r.catalog.children(index_key("b")));
        settle().await;
        // Finish the first refresh call; the queued load comes before the second one.
        r.fake.release_one();
        r.fake.wait_calls(2).await;
        assert_eq!(r.fake.log()[1], CacheKey::Children(index_key("b")));
        r.fake.release_one();
        assert!(load.await.unwrap().is_ok());
        let seen: Vec<_> = std::iter::from_fn(|| h.try_next_event()).collect();
        assert!(
            !seen.iter().any(|e| matches!(e, RefreshEvent::Finished(_))),
            "{seen:?}"
        );
        r.fake.ungate();
        assert_applied(&[seen, events(&mut h).await].concat(), 4, 0);
    });
}

#[test]
fn shutdown_returns_the_source_and_closes_pending_refreshes() {
    run(async {
        let r = rig(&Fake::standard());
        load_all(&r.catalog).await;
        r.fake.gate();
        let mut running = r.catalog.refresh(RefreshScope::All);
        let mut queued = r.catalog.refresh(RefreshScope::All);
        r.fake.wait_calls(6).await;
        r.catalog.shutdown();
        r.fake.ungate();
        let source = r.join.await.unwrap();
        assert!(matches!(source, IntrospectSource::Introspect(_)));
        for h in [&mut running, &mut queued] {
            let all = events(h).await;
            assert_eq!(
                all.last(),
                Some(&RefreshEvent::Finished(RefreshOutcome::Closed))
            );
        }
        // Reads of the cache still work; loads and refreshes report Closed.
        assert!(r.catalog.cached(&tables_key()).is_some());
        assert_eq!(
            r.catalog.children(index_key("b")).await.unwrap_err(),
            CatalogError::Closed
        );
        let mut late = r.catalog.refresh(RefreshScope::All);
        assert_eq!(
            events(&mut late).await,
            [RefreshEvent::Finished(RefreshOutcome::Closed)]
        );
    });
}

#[test]
fn dropping_every_handle_stops_the_worker() {
    run(async {
        let Rig { catalog, join, .. } = rig(&Fake::standard());
        let clone = catalog.clone();
        drop(catalog);
        settle().await;
        assert!(!join.is_finished(), "a clone is still alive");
        drop(clone);
        let source = join.await.unwrap();
        assert!(matches!(source, IntrospectSource::Introspect(_)));
    });
}

#[test]
fn dropping_a_refresh_handle_detaches_the_refresh() {
    run(async {
        let r = rig(&Fake::standard());
        load_all(&r.catalog).await;
        r.fake
            .set_list(&main_path(), ObjectKind::Table, vec![tbl("a")]);
        r.fake.gate();
        let h = r
            .catalog
            .refresh(RefreshScope::Node(CacheKey::Children(tables_key())));
        r.fake.wait_calls(6).await;
        drop(h);
        r.fake.ungate();
        // A load queued behind the refresh is served after it finished.
        r.catalog.children(index_key("b")).await.unwrap();
        assert_eq!(objects(&r.catalog.cached(&tables_key()).unwrap()), ["a"]);
    });
}
