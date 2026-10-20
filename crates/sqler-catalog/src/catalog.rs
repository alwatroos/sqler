// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The cloneable catalog handle, refresh handles and refresh events.

use std::future::Future;
use std::sync::{Arc, RwLock};

use sqler_driver_api::CancelFlag;
use sqler_driver_api::sqler_core::{DbObject, Listing, Table};
use tokio::sync::{mpsc, oneshot};

use crate::error::CatalogError;
use crate::key::{CacheKey, NodeKey, RefreshScope};
use crate::store::{self, Shared, Store};
use crate::worker::{CatalogWorker, Command, IntrospectSource, Job, Loaded};

/// How a refresh ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefreshOutcome {
    /// The results were applied to the cache in one step.
    Applied {
        /// Entries fetched and replaced.
        refreshed: usize,
        /// Entries that failed; their old data was kept.
        failed: usize,
    },
    /// The refresh was cancelled; the cache is unchanged.
    Cancelled,
    /// The worker stopped before the refresh finished; the cache is unchanged.
    Closed,
}

/// A progress report of a refresh. `Finished` is always the last one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefreshEvent {
    /// The refresh started and will fetch `total` entries.
    Started {
        /// Number of entries to fetch.
        total: usize,
    },
    /// One more entry was fetched (or failed).
    Progress {
        /// Entries processed so far.
        done: usize,
        /// Entries to process in total.
        total: usize,
    },
    /// One entry failed; the refresh goes on.
    NodeFailed {
        /// The entry.
        key: CacheKey,
        /// Why it failed.
        error: CatalogError,
    },
    /// The refresh ended.
    Finished(RefreshOutcome),
}

/// Follows and cancels one refresh. Dropping it lets the refresh finish (it does not cancel).
#[derive(Debug)]
pub struct RefreshHandle {
    flag: CancelFlag,
    events: mpsc::UnboundedReceiver<RefreshEvent>,
}

impl RefreshHandle {
    /// Asks the refresh to stop. A call already running finishes and its result is discarded.
    pub fn cancel(&self) {
        self.flag.cancel();
    }

    /// The flag that [`RefreshHandle::cancel`] sets.
    #[must_use]
    pub fn cancel_flag(&self) -> CancelFlag {
        self.flag.clone()
    }

    /// The next event, or `None` after `Finished` has been taken.
    pub async fn next_event(&mut self) -> Option<RefreshEvent> {
        self.events.recv().await
    }

    /// The next event if one is ready.
    pub fn try_next_event(&mut self) -> Option<RefreshEvent> {
        self.events.try_recv().ok()
    }
}

/// A lazy, cached view of a database's catalog.
///
/// Cheap to clone and shareable between threads. Reads of cached nodes are synchronous and
/// never touch the worker or the database. Everything else goes through the
/// [`CatalogWorker`], which runs the introspection calls one at a time.
#[derive(Debug, Clone)]
pub struct Catalog {
    store: Shared,
    tx: mpsc::UnboundedSender<Command>,
}

impl Catalog {
    /// Creates a catalog and its worker. Run [`CatalogWorker::run`] on an executor.
    pub fn new(source: impl Into<IntrospectSource>) -> (Catalog, CatalogWorker) {
        let store: Shared = Arc::new(RwLock::new(Store::default()));
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = CatalogWorker::new(source.into(), rx, Arc::clone(&store));
        (Catalog { store, tx }, worker)
    }

    /// The cached children of `key`, if loaded. Never touches the database.
    #[must_use]
    pub fn cached(&self, key: &NodeKey) -> Option<Arc<Listing>> {
        store::read(&self.store).lists.get(key).cloned()
    }

    /// The children of `key`: from the cache, or loaded by the worker (one call per node).
    ///
    /// # Errors
    /// [`CatalogError::Driver`] or [`CatalogError::NoIntrospection`] if the load fails (the error
    /// is also kept for [`Catalog::node_error`]); [`CatalogError::Closed`] if the worker stopped.
    pub fn children(
        &self,
        key: NodeKey,
    ) -> impl Future<Output = Result<Arc<Listing>, CatalogError>> + Send + 'static {
        let hit = self.cached(&key);
        let tx = self.tx.clone();
        async move {
            if let Some(listing) = hit {
                return Ok(listing);
            }
            match request(&tx, CacheKey::Children(key)).await? {
                Loaded::List(listing) => Ok(listing),
                Loaded::Table(_) => Err(CatalogError::Closed),
            }
        }
    }

    /// The cached details of `obj`, if loaded. Never touches the database.
    #[must_use]
    pub fn cached_table(&self, obj: &DbObject) -> Option<Arc<Table>> {
        store::read(&self.store).tables.get(obj).cloned()
    }

    /// The details of `obj`: from the cache, or loaded by the worker.
    ///
    /// `Ok(None)` means the driver found no such table; that is not cached.
    ///
    /// # Errors
    /// Same as [`Catalog::children`].
    pub fn table(
        &self,
        obj: DbObject,
    ) -> impl Future<Output = Result<Option<Arc<Table>>, CatalogError>> + Send + 'static {
        let hit = self.cached_table(&obj);
        let tx = self.tx.clone();
        async move {
            if let Some(table) = hit {
                return Ok(Some(table));
            }
            match request(&tx, CacheKey::Table(obj)).await? {
                Loaded::Table(table) => Ok(table),
                Loaded::List(_) => Err(CatalogError::Closed),
            }
        }
    }

    /// The last error of `key`, until a later load or refresh of it succeeds.
    #[must_use]
    pub fn node_error(&self, key: &CacheKey) -> Option<CatalogError> {
        store::read(&self.store).errors.get(key).cloned()
    }

    /// Number of cached lists plus tables.
    #[must_use]
    pub fn cached_len(&self) -> usize {
        store::read(&self.store).len()
    }

    /// Starts a refresh in the background and returns its handle.
    ///
    /// `Subtree` and `All` re-fetch only what is cached when the refresh starts. Readers see
    /// the old data until the whole refresh is applied at once.
    #[allow(clippy::must_use_candidate)] // dropping the handle detaches the refresh
    pub fn refresh(&self, scope: RefreshScope) -> RefreshHandle {
        let flag = CancelFlag::new();
        let (events, rx) = mpsc::unbounded_channel();
        let job = Job {
            scope,
            flag: flag.clone(),
            events,
        };
        if let Err(mpsc::error::SendError(Command::Refresh(job))) =
            self.tx.send(Command::Refresh(job))
        {
            job.finish(RefreshOutcome::Closed);
        }
        RefreshHandle { flag, events: rx }
    }

    /// Stops the worker: refreshes end `Closed` and [`CatalogWorker::run`] returns the source.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

async fn request(
    tx: &mpsc::UnboundedSender<Command>,
    key: CacheKey,
) -> Result<Loaded, CatalogError> {
    let (reply, rx) = oneshot::channel();
    tx.send(Command::Load(key, reply))
        .map_err(|_| CatalogError::Closed)?;
    rx.await.map_err(|_| CatalogError::Closed)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<F: Future + Send + 'static>(_: F) {}
    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn handles_and_worker_future_are_send() {
        assert_send_sync::<Catalog>();
        assert_send::<CatalogWorker>();
        assert_send::<RefreshHandle>();
        let _ = |worker: CatalogWorker| check(worker.run());
    }

    #[test]
    fn closed_catalog_reports_closed() {
        struct Nothing;
        impl sqler_driver_api::Introspect for Nothing {
            fn supports(&self, _: sqler_driver_api::sqler_core::ObjectKind) -> bool {
                false
            }
            fn list<'a>(
                &'a mut self,
                _: &'a sqler_driver_api::sqler_core::ObjectPath,
                _: sqler_driver_api::sqler_core::ObjectKind,
            ) -> sqler_driver_api::BoxFuture<'a, Result<Listing, sqler_driver_api::DriverError>>
            {
                Box::pin(std::future::ready(Ok(Listing::Unsupported)))
            }
            fn table<'a>(
                &'a mut self,
                _: &'a DbObject,
            ) -> sqler_driver_api::BoxFuture<'a, Result<Option<Table>, sqler_driver_api::DriverError>>
            {
                Box::pin(std::future::ready(Ok(None)))
            }
            fn definition<'a>(
                &'a mut self,
                _: &'a DbObject,
            ) -> sqler_driver_api::BoxFuture<
                'a,
                Result<Option<String>, sqler_driver_api::DriverError>,
            > {
                Box::pin(std::future::ready(Ok(None)))
            }
        }
        let boxed: Box<dyn sqler_driver_api::Introspect> = Box::new(Nothing);
        let (catalog, worker) = Catalog::new(boxed);
        drop(worker);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let key = NodeKey::new(
            sqler_driver_api::sqler_core::ObjectPath::root(),
            sqler_driver_api::sqler_core::ObjectKind::Schema,
        );
        assert_eq!(
            rt.block_on(catalog.children(key)),
            Err(CatalogError::Closed)
        );
        let mut handle = catalog.refresh(RefreshScope::All);
        assert_eq!(
            handle.try_next_event(),
            Some(RefreshEvent::Finished(RefreshOutcome::Closed))
        );
        assert_eq!(handle.try_next_event(), None);
    }
}
