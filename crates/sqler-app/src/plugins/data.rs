// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The databases the app lends to plugins (ADR-0058): the saved connections, opened on the app's connect pipeline.
//!
//! Every [`acquire`](DataService::acquire) opens a **fresh session** of its own; it never shares one with a tab of the
//! user, so no session on which the user holds a transaction is ever lent. [`release`](DataService::release) always
//! closes the session: an approved `BEGIN`, `SET ROLE`, `SET search_path`, `USE`, a `SQLite` `ATTACH` or a temporary
//! table changes the state of a session, and a session that plugin code touched never goes back into a pool.

use std::sync::Arc;

use sqler_catalog::Catalog;
use sqler_config::{ConfigDir, ConnectionProfile};
use sqler_driver_api::{BoxFuture, Session};
use sqler_plugin_host::{DataError, DataService, PluginConnection};
use sqler_ui::connections::{ConnectRequest, Connector};

use super::drivers;

/// The saved connections of the user, read from `connections.toml` when asked (a connection added while the app runs
/// is seen by the next call).
pub(super) struct AppData {
    config: Option<ConfigDir>,
    connector: Arc<dyn Connector>,
}

impl AppData {
    pub(super) fn new(config: Option<ConfigDir>, connector: Arc<dyn Connector>) -> Self {
        Self { config, connector }
    }

    /// The saved profiles; empty without a configuration folder or when the file does not load. Blocking file I/O.
    fn read_profiles(config: Option<&ConfigDir>) -> Vec<ConnectionProfile> {
        config
            .and_then(|dir| dir.load_connection_store().ok())
            .map(|store| store.connections().to_vec())
            .unwrap_or_default()
    }

    /// The profiles, read on a blocking task so the runtime worker is not held by the file.
    async fn profiles(&self) -> Vec<ConnectionProfile> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || Self::read_profiles(config.as_ref()))
            .await
            .unwrap_or_default()
    }

    /// The same for the synchronous trait method: on a multi-threaded runtime the worker is handed over while the
    /// file is read; elsewhere there is no worker to hold.
    fn profiles_sync(&self) -> Vec<ConnectionProfile> {
        let multi = tokio::runtime::Handle::try_current()
            .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
        if multi {
            tokio::task::block_in_place(|| Self::read_profiles(self.config.as_ref()))
        } else {
            Self::read_profiles(self.config.as_ref())
        }
    }
}

impl DataService for AppData {
    fn connections(&self) -> Vec<PluginConnection> {
        self.profiles_sync()
            .iter()
            .map(|p| {
                let spec = p.spec();
                drivers::declare_dialect(PluginConnection::new(
                    p.id().as_str(),
                    spec.name.clone(),
                    spec.driver.clone(),
                    spec.environment,
                ))
            })
            .collect()
    }

    /// The app keeps no catalog outside the browser of a window; plugins get none for now.
    fn catalog(&self, _connection: &str) -> Option<Catalog> {
        None
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DataError>> {
        Box::pin(async move {
            let profile = self
                .profiles()
                .await
                .into_iter()
                .find(|p| p.id().as_str() == connection)
                .ok_or_else(|| DataError::UnknownConnection(connection.to_owned()))?;
            // The text reaches the plugin: no host, user, database or secret in it.
            let request = ConnectRequest::from_profile(&profile).map_err(|_| {
                DataError::NotAvailable("this connection cannot be opened".to_owned())
            })?;
            match self.connector.connect(request).await {
                Ok(session) => Ok(session),
                Err(failure) => {
                    // The plugin sees a fixed text; the reason stays on our side.
                    eprintln!("sqler: a plugin could not open a connection: {failure}");
                    Err(DataError::Driver(failure.to_string()))
                }
            }
        })
    }

    fn release(&self, _connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let _ = session.close().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_config::{ConnectionSpec, ConnectionStore};
    use sqler_driver_api::sqler_core::{DriverId, Environment, Query};
    use sqler_driver_api::{CancelFlag, CancelHandle, DriverError, QueryStream};
    use sqler_ui::connections::{ConnectFailure, HostKeyPrompt};

    use super::*;
    use crate::plugins::scratch::Scratch;

    struct Fake(Arc<AtomicUsize>);

    impl Session for Fake {
        fn cancel_handle(&self) -> CancelHandle {
            CancelFlag::new().handle()
        }

        fn execute<'s>(
            &'s mut self,
            _query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            Box::pin(async { Err(DriverError::Unsupported("test".into())) })
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    struct Counting {
        connects: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
        fail: bool,
    }

    impl Connector for Counting {
        fn driver_ids(&self) -> Vec<DriverId> {
            Vec::new()
        }

        fn connect(
            &self,
            _request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            let closed = Arc::clone(&self.closed);
            let fail = self.fail;
            Box::pin(async move {
                if fail {
                    return Err(ConnectFailure::Aborted);
                }
                let session: Box<dyn Session> = Box::new(Fake(closed));
                Ok(session)
            })
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    struct Rig {
        data: AppData,
        id: String,
        connects: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
        _dir: Scratch,
    }

    fn rig(fail: bool) -> Rig {
        let dir = Scratch::new("data");
        let config = ConfigDir::new(dir.path());
        let driver = DriverId::new("sqlite").unwrap();
        let mut spec = ConnectionSpec::new("Orders", driver, "orders.db");
        spec.environment = Some(Environment::Prod);
        let mut store = ConnectionStore::new();
        let id = store.add_connection(spec, None).unwrap();
        config.save_connection_store(&store).unwrap();
        let connects = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let connector = Arc::new(Counting {
            connects: Arc::clone(&connects),
            closed: Arc::clone(&closed),
            fail,
        });
        Rig {
            data: AppData::new(Some(config), connector),
            id: id.as_str().to_owned(),
            connects,
            closed,
            _dir: dir,
        }
    }

    #[test]
    fn it_lists_the_saved_connections_with_their_environment() {
        let r = rig(false);
        let list = r.data.connections();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, r.id);
        assert_eq!(list[0].name, "Orders");
        assert_eq!(list[0].environment, Some(Environment::Prod));
        assert!(r.data.catalog(&r.id).is_none());
        // No folder, no connections.
        let none = AppData::new(
            None,
            Arc::new(Counting {
                connects: Arc::default(),
                closed: Arc::default(),
                fail: false,
            }),
        );
        assert_eq!(none.connections(), Vec::new());
    }

    #[tokio::test]
    async fn every_acquire_opens_its_own_session_and_release_closes_it() {
        let r = rig(false);
        let a = r.data.acquire(&r.id).await.unwrap();
        let b = r.data.acquire(&r.id).await.unwrap();
        assert_eq!(r.connects.load(Ordering::SeqCst), 2);
        r.data.release(&r.id, a).await;
        assert_eq!(r.closed.load(Ordering::SeqCst), 1);
        r.data.release(&r.id, b).await;
        assert_eq!(r.closed.load(Ordering::SeqCst), 2);
        // Nothing was kept: the next acquire connects again.
        let c = r.data.acquire(&r.id).await.unwrap();
        assert_eq!(r.connects.load(Ordering::SeqCst), 3);
        r.data.release(&r.id, c).await;
    }

    #[tokio::test]
    async fn an_unknown_id_and_a_failed_connect_are_refused_without_detail() {
        let r = rig(true);
        assert!(matches!(
            r.data.acquire("nope").await.err(),
            Some(DataError::UnknownConnection(_))
        ));
        assert_eq!(r.connects.load(Ordering::SeqCst), 0);
        assert!(matches!(
            r.data.acquire(&r.id).await.err(),
            Some(DataError::Driver(_))
        ));
    }
}
