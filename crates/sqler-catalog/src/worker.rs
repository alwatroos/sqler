// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The worker: owns the session and runs every introspection call, one at a time.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqler_driver_api::sqler_core::{DbObject, Listing, ObjectPath, Table};
use sqler_driver_api::{CancelFlag, Introspect, Session};
use tokio::sync::{mpsc, oneshot};

use crate::catalog::{RefreshEvent, RefreshOutcome};
use crate::error::CatalogError;
use crate::key::{CacheKey, NodeKey, RefreshScope};
use crate::store::{self, ListEntry, Shared, Staged, Store, TableEntry};

/// What the worker introspects: a session or a bare `Introspect`.
///
/// [`CatalogWorker::run`] gives it back, so the caller can close a session.
pub enum IntrospectSource {
    /// A session; its [`Session::introspection`] is used.
    Session(Box<dyn Session>),
    /// An introspection object.
    Introspect(Box<dyn Introspect>),
}

impl From<Box<dyn Session>> for IntrospectSource {
    fn from(session: Box<dyn Session>) -> Self {
        Self::Session(session)
    }
}

impl From<Box<dyn Introspect>> for IntrospectSource {
    fn from(introspect: Box<dyn Introspect>) -> Self {
        Self::Introspect(introspect)
    }
}

impl std::fmt::Debug for IntrospectSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Session(_) => "IntrospectSource::Session",
            Self::Introspect(_) => "IntrospectSource::Introspect",
        })
    }
}

impl IntrospectSource {
    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        match self {
            Self::Session(session) => session.introspection(),
            Self::Introspect(introspect) => Some(&mut **introspect),
        }
    }
}

/// A loaded value, as handed back to a waiting caller.
#[derive(Debug, Clone)]
pub(crate) enum Loaded {
    List(Arc<Listing>),
    Table(Option<Arc<Table>>),
}

pub(crate) type Reply = oneshot::Sender<Result<Loaded, CatalogError>>;

/// A refresh as sent to the worker.
pub(crate) struct Job {
    pub(crate) scope: RefreshScope,
    pub(crate) flag: CancelFlag,
    pub(crate) events: mpsc::UnboundedSender<RefreshEvent>,
    finished: AtomicBool,
}

impl Job {
    pub(crate) fn new(
        scope: RefreshScope,
        flag: CancelFlag,
        events: mpsc::UnboundedSender<RefreshEvent>,
    ) -> Self {
        Self {
            scope,
            flag,
            events,
            finished: AtomicBool::new(false),
        }
    }

    pub(crate) fn finish(&self, outcome: RefreshOutcome) {
        // Exactly one `Finished` per job, whichever path gets there first.
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        // A dropped handle means nobody listens; the refresh ran regardless.
        let _ = self.events.send(RefreshEvent::Finished(outcome));
    }

    fn emit(&self, event: RefreshEvent) {
        let _ = self.events.send(event);
    }
}

impl Drop for Job {
    /// A job that is dropped unfinished (the worker future was dropped or aborted) is closed.
    fn drop(&mut self) {
        self.finish(RefreshOutcome::Closed);
    }
}

pub(crate) enum Command {
    Load(CacheKey, Reply),
    Refresh(Job),
    Shutdown,
}

/// The progress of a refresh that has started.
struct Run {
    keys: Vec<CacheKey>,
    next: usize,
    staged: Staged,
    failed: usize,
}

struct Active {
    job: Job,
    run: Option<Run>,
}

enum Fetched {
    List(NodeKey, Listing),
    Table(DbObject, Option<Table>),
}

async fn fetch(source: &mut IntrospectSource, key: &CacheKey) -> Result<Fetched, CatalogError> {
    let intro = source
        .introspection()
        .ok_or(CatalogError::NoIntrospection)?;
    match key {
        CacheKey::Children(node) => {
            let listing = intro.list(&node.parent, node.kind).await?;
            Ok(Fetched::List(node.clone(), listing))
        }
        CacheKey::Table(obj) => {
            let table = intro.table(obj).await?;
            Ok(Fetched::Table(obj.clone(), table))
        }
    }
}

/// Replaces the published store with the result of `f` applied to a copy.
///
/// Only the worker writes, so nothing changes the store between the clone and the swap. `f` and
/// the clone run outside the lock; the write lock is held for a pointer swap only.
fn publish(shared: &Shared, f: impl FnOnce(&mut Store)) {
    let mut next = Store::clone(&store::current(shared));
    f(&mut next);
    *store::write(shared) = Arc::new(next);
}

/// Stages a fetched value. An equal value keeps the cached entry (and its index); a new one gets
/// its entry built here, outside any lock.
fn stage(staged: &mut Staged, current: &Store, fetched: Fetched) {
    match fetched {
        Fetched::List(key, listing) => match current.lists.get(&key) {
            Some(old) if *old.listing == listing => {
                staged.unchanged.push(CacheKey::Children(key));
            }
            _ => staged
                .lists
                .push((key, Arc::new(ListEntry::new(Arc::new(listing))))),
        },
        Fetched::Table(obj, Some(table)) => match current.tables.get(&obj) {
            Some(old) if *old.table == table => staged.unchanged.push(CacheKey::Table(obj)),
            _ => staged
                .tables
                .push((obj, Arc::new(TableEntry::new(Arc::new(table))))),
        },
        Fetched::Table(obj, None) => staged.gone.push(obj),
    }
}

/// The background half of the catalog. Run it on an executor; it spawns nothing itself.
///
/// It owns the [`IntrospectSource`] and serves loads and refreshes in sequence. Loads come
/// before the next refresh step.
#[must_use = "the worker does nothing until `run` is awaited"]
pub struct CatalogWorker {
    source: IntrospectSource,
    rx: mpsc::UnboundedReceiver<Command>,
    store: Shared,
    jobs: VecDeque<Active>,
    /// Commands taken off the channel while answering duplicate loads, still to be handled.
    deferred: VecDeque<Command>,
}

impl CatalogWorker {
    pub(crate) fn new(
        source: IntrospectSource,
        rx: mpsc::UnboundedReceiver<Command>,
        store: Shared,
    ) -> Self {
        Self {
            source,
            rx,
            store,
            jobs: VecDeque::new(),
            deferred: VecDeque::new(),
        }
    }

    /// Serves requests until [`crate::Catalog::shutdown`] or until every handle is dropped.
    ///
    /// Pending refreshes end with [`RefreshOutcome::Closed`] and waiting loads see
    /// [`CatalogError::Closed`]. Returns the source so the caller can close the session.
    ///
    /// If the future is dropped instead, open refreshes still end with `Closed` and waiting
    /// loads see [`CatalogError::Closed`], because the pending requests are dropped with it.
    pub async fn run(mut self) -> IntrospectSource {
        loop {
            let mut open = true;
            loop {
                let next = match self.deferred.pop_front() {
                    Some(cmd) => Ok(cmd),
                    None => self.rx.try_recv(),
                };
                match next {
                    Ok(cmd) => {
                        if !self.handle(cmd).await {
                            open = false;
                            break;
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        open = false;
                        break;
                    }
                }
            }
            if !open {
                break;
            }
            if self.jobs.is_empty() {
                match self.rx.recv().await {
                    Some(cmd) => {
                        if !self.handle(cmd).await {
                            break;
                        }
                    }
                    None => break,
                }
            } else {
                self.step().await;
            }
        }
        self.close();
        self.source
    }

    /// Returns `false` on shutdown.
    async fn handle(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Load(key, reply) => self.load(key, reply).await,
            Command::Refresh(job) => self.jobs.push_back(Active { job, run: None }),
            Command::Shutdown => return false,
        }
        true
    }

    /// Ends every refresh with `Closed` and refuses further requests.
    fn close(&mut self) {
        self.rx.close();
        for active in self.jobs.drain(..) {
            active.job.finish(RefreshOutcome::Closed);
        }
        for cmd in self.deferred.drain(..) {
            if let Command::Refresh(job) = cmd {
                job.finish(RefreshOutcome::Closed);
            }
        }
        while let Ok(cmd) = self.rx.try_recv() {
            if let Command::Refresh(job) = cmd {
                job.finish(RefreshOutcome::Closed);
            }
        }
    }

    fn lookup(&self, key: &CacheKey) -> Option<Loaded> {
        let store = store::current(&self.store);
        match key {
            CacheKey::Children(k) => store
                .lists
                .get(k)
                .map(|e| Loaded::List(Arc::clone(&e.listing))),
            CacheKey::Table(o) => store
                .tables
                .get(o)
                .map(|e| Loaded::Table(Some(Arc::clone(&e.table)))),
        }
    }

    async fn load(&mut self, key: CacheKey, reply: Reply) {
        // The caller dropped its future: nobody wants the result, so do not fetch it.
        if reply.is_closed() {
            return;
        }
        // Another caller may have loaded it while this request waited.
        if let Some(hit) = self.lookup(&key) {
            let _ = reply.send(Ok(hit));
            return;
        }
        let result = match fetch(&mut self.source, &key).await {
            Ok(Fetched::List(node, listing)) => {
                let listing = Arc::new(listing);
                let mut staged = Staged::default();
                let entry = Arc::new(ListEntry::new(Arc::clone(&listing)));
                staged.lists.push((node, entry));
                publish(&self.store, |s| s.apply(staged));
                Ok(Loaded::List(listing))
            }
            Ok(Fetched::Table(obj, Some(table))) => {
                let table = Arc::new(table);
                let mut staged = Staged::default();
                let entry = Arc::new(TableEntry::new(Arc::clone(&table)));
                staged.tables.push((obj, entry));
                publish(&self.store, |s| s.apply(staged));
                Ok(Loaded::Table(Some(table)))
            }
            Ok(Fetched::Table(_, None)) => {
                publish(&self.store, |s| s.clear_error(&key));
                Ok(Loaded::Table(None))
            }
            Err(error) => {
                let mut staged = Staged::default();
                staged.failures.push((key.clone(), error.clone()));
                publish(&self.store, |s| s.apply(staged));
                Err(error)
            }
        };
        // Waiters queued for the same key share this one result, success or failure.
        while let Ok(cmd) = self.rx.try_recv() {
            self.deferred.push_back(cmd);
        }
        let mut rest = VecDeque::with_capacity(self.deferred.len());
        for cmd in self.deferred.drain(..) {
            match cmd {
                Command::Load(k, waiter) if k == key => {
                    let _ = waiter.send(result.clone());
                }
                other => rest.push_back(other),
            }
        }
        self.deferred = rest;
        let _ = reply.send(result);
    }

    /// Runs one step of the oldest refresh: start, one introspection call, or the final apply.
    async fn step(&mut self) {
        let Some(mut active) = self.jobs.pop_front() else {
            return;
        };
        if active.job.flag.is_cancelled() {
            active.job.finish(RefreshOutcome::Cancelled);
            return;
        }
        let mut run = if let Some(run) = active.run.take() {
            run
        } else {
            let keys = self.scope_keys(&active.job.scope);
            active.job.emit(RefreshEvent::Started { total: keys.len() });
            Run {
                keys,
                next: 0,
                staged: Staged::default(),
                failed: 0,
            }
        };
        if run.next >= run.keys.len() {
            let total = run.keys.len();
            publish(&self.store, |s| s.apply(run.staged));
            active.job.finish(RefreshOutcome::Applied {
                refreshed: total - run.failed,
                failed: run.failed,
            });
            return;
        }
        let key = run.keys[run.next].clone();
        match fetch(&mut self.source, &key).await {
            Ok(fetched) => stage(&mut run.staged, &store::current(&self.store), fetched),
            Err(error) => {
                run.failed += 1;
                run.staged.failures.push((key.clone(), error.clone()));
                active.job.emit(RefreshEvent::NodeFailed { key, error });
            }
        }
        run.next += 1;
        active.job.emit(RefreshEvent::Progress {
            done: run.next,
            total: run.keys.len(),
        });
        active.run = Some(run);
        self.jobs.push_front(active);
    }

    fn scope_keys(&self, scope: &RefreshScope) -> Vec<CacheKey> {
        match scope {
            RefreshScope::Node(key) => vec![key.clone()],
            RefreshScope::Subtree(path) => store::current(&self.store).keys_within(path),
            RefreshScope::All => store::current(&self.store).keys_within(&ObjectPath::root()),
        }
    }
}
