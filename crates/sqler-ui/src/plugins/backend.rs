// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What the UI sees of the plugin host: a trait and plain data, so `sqler-ui` never links the WebAssembly runtime
//! (the `Connector` and `AiProviderFactory` precedent, ADR-0016 and ADR-0050). The binary builds a backend when the
//! `plugins` feature is on; without it [`PluginsStartup::unavailable`] says so.

use std::sync::Arc;

use sqler_driver_api::BoxFuture;
use sqler_plugin_api::{
    CapabilitySet, ComponentDigest, Manifest, PluginId, SettingSpec, SettingValue,
};

use super::approval::WriteInbox;
use super::grant::GrantBook;

/// The plugin host as the UI uses it.
///
/// Every method is blocking (file and lock access): call it on the background executor, never on the UI thread.
pub trait PluginsBackend: Send + Sync {
    /// Reloads the grants, discovers the plugins again, registers them and applies the stored preferences.
    fn refresh(&self) -> PluginSnapshot;

    /// Reloads the grants (and refreshes the host's view of the plugins whose grants changed) and returns the rows.
    fn snapshot(&self) -> PluginSnapshot;

    /// Switches a plugin on or off and persists the choice.
    ///
    /// # Errors
    ///
    /// A one-line reason when the choice cannot be kept.
    fn set_enabled(&self, id: &PluginId, on: bool) -> Result<(), String>;

    /// Tells the host the grants of `id` changed (after a decision or a revoke): reload and refresh the plugin.
    fn grants_changed(&self, id: &PluginId);

    /// Stores a setting value of a plugin.
    ///
    /// # Errors
    ///
    /// A one-line reason when the value is refused or cannot be kept.
    fn set_setting(&self, id: &PluginId, key: &str, value: SettingValue) -> Result<(), String>;

    /// Starts a plugin (its `init`). `Err` carries the last error shown in the manager.
    fn start(&self, id: &PluginId) -> BoxFuture<'static, Result<(), String>>;
}

/// What the binary hands over about plugins at startup.
///
/// Only built through [`PluginsStartup::new`] and [`PluginsStartup::unavailable`] (never as a literal), so a field can
/// be added without touching the callers.
#[derive(Clone)]
#[non_exhaustive]
pub struct PluginsStartup {
    /// The host, or `None` when this build has no plugin support.
    pub backend: Option<Arc<dyn PluginsBackend>>,
    /// The book of grants; the dialog decides here and the host reads it.
    pub grants: Arc<GrantBook>,
    /// Where the plugins' write approvals arrive (the app's approver holds the sender). Empty by default.
    pub writes: WriteInbox,
    /// The tools plugins offer to the AI surfaces, or `None` (the default). They ride on the AI audit sink.
    pub ai_tools: Option<Arc<dyn sqler_ai_tools::ExternalTools>>,
}

impl std::fmt::Debug for PluginsStartup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginsStartup")
            .field("backend", &self.backend.is_some())
            .field("writes", &self.writes.is_set())
            .field("ai_tools", &self.ai_tools.is_some())
            .finish_non_exhaustive()
    }
}

impl PluginsStartup {
    /// A startup with a host and the book it reads.
    #[must_use]
    pub fn new(backend: Option<Arc<dyn PluginsBackend>>, grants: Arc<GrantBook>) -> Self {
        Self {
            backend,
            grants,
            writes: WriteInbox::default(),
            ai_tools: None,
        }
    }

    /// Sets the tools plugins offer to the AI surfaces.
    #[must_use]
    pub fn with_ai_tools(mut self, tools: Arc<dyn sqler_ai_tools::ExternalTools>) -> Self {
        self.ai_tools = Some(tools);
        self
    }

    /// Sets the inbox of plugin write approvals.
    #[must_use]
    pub fn with_writes(mut self, writes: WriteInbox) -> Self {
        self.writes = writes;
        self
    }

    /// A build without plugin support: no host and an empty in-memory book.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(None, Arc::new(GrantBook::in_memory()))
    }
}

/// The plugins found and the directories that could not be read.
#[derive(Debug, Clone, Default)]
pub struct PluginSnapshot {
    /// One row per discovered plugin, in a stable order.
    pub rows: Vec<PluginRow>,
    /// Directories that did not load.
    pub problems: Vec<PluginProblem>,
}

/// One plugin as the manager shows it.
#[derive(Debug, Clone)]
pub struct PluginRow {
    /// The validated manifest.
    pub manifest: Arc<Manifest>,
    /// The digest of the component (`None` for a declarative plugin or one that failed to load).
    pub component: Option<ComponentDigest>,
    /// Loaded from a `--plugin-dir` path.
    pub dev: bool,
    /// Where the plugin is in its life.
    pub state: RowState,
    /// Whether the user has it switched on.
    pub enabled: bool,
    /// What the plugin may do now (requested capabilities that are approved).
    pub effective: CapabilitySet,
    /// The last error, in one line.
    pub last_error: Option<String>,
    /// Scopes the host refused (missing or escaping directories).
    pub scope_issues: Vec<String>,
    /// The settings the plugin declares with their current values.
    pub settings: Vec<(SettingSpec, SettingValue)>,
}

/// The life of a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RowState {
    /// Known, not started.
    Idle,
    /// Started and running.
    Live,
    /// Switched off by the user.
    Off,
    /// Stopped after trapping or timing out this many times.
    Faulted {
        /// How many faults.
        count: u32,
    },
    /// The component could not be loaded.
    LoadFailed,
}

/// A directory that did not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProblem {
    /// The directory name.
    pub dir: String,
    /// The plugin id, when the manifest got far enough to have one.
    pub id: Option<String>,
    /// Why, in one line.
    pub message: String,
}

/// A scripted backend for tests and scenes: it counts calls and returns what it was given.
#[cfg(any(test, feature = "screenshots"))]
#[allow(unused_imports)] // the manager scenes and tests use them
pub(crate) use fake::{FakeBackend, row as fake_row};

#[cfg(any(test, feature = "screenshots"))]
#[allow(dead_code)] // the manager scenes and tests use the parts the backend tests do not
mod fake {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        Arc, BoxFuture, CapabilitySet, ComponentDigest, Manifest, PluginId, PluginRow,
        PluginSnapshot, PluginsBackend, RowState, SettingValue,
    };

    /// Counts of each backend call.
    #[derive(Debug, Default)]
    pub(crate) struct Calls {
        pub refresh: AtomicUsize,
        pub snapshot: AtomicUsize,
        pub set_enabled: AtomicUsize,
        pub grants_changed: AtomicUsize,
        pub set_setting: AtomicUsize,
        pub start: AtomicUsize,
    }

    /// The fake: a scripted snapshot, optional failures, and call counters.
    #[derive(Default)]
    pub(crate) struct FakeBackend {
        pub calls: Calls,
        snapshot: Mutex<PluginSnapshot>,
        /// Ids passed to `start`, `grants_changed`, in order.
        pub started: Mutex<Vec<String>>,
        pub changed: Mutex<Vec<String>>,
        /// Result of `start`; `Err` is returned for every start.
        pub start_error: Mutex<Option<String>>,
        /// Result of `set_enabled`/`set_setting` when set.
        pub write_error: Mutex<Option<String>>,
    }

    impl FakeBackend {
        pub fn new(snapshot: PluginSnapshot) -> Arc<Self> {
            Arc::new(Self {
                snapshot: Mutex::new(snapshot),
                ..Self::default()
            })
        }

        /// Replaces what the next `refresh`/`snapshot` returns.
        #[allow(clippy::unwrap_used)] // test double
        pub fn script(&self, snapshot: PluginSnapshot) {
            *self.snapshot.lock().unwrap() = snapshot;
        }

        pub fn count(counter: &AtomicUsize) -> usize {
            counter.load(Ordering::SeqCst)
        }

        #[allow(clippy::unwrap_used)] // test double
        fn rows(&self) -> PluginSnapshot {
            self.snapshot.lock().unwrap().clone()
        }
    }

    #[allow(clippy::unwrap_used)] // test double
    impl PluginsBackend for FakeBackend {
        fn refresh(&self) -> PluginSnapshot {
            self.calls.refresh.fetch_add(1, Ordering::SeqCst);
            self.rows()
        }

        fn snapshot(&self) -> PluginSnapshot {
            self.calls.snapshot.fetch_add(1, Ordering::SeqCst);
            self.rows()
        }

        fn set_enabled(&self, id: &PluginId, on: bool) -> Result<(), String> {
            self.calls.set_enabled.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = self.write_error.lock().unwrap().clone() {
                return Err(e);
            }
            for row in &mut self.snapshot.lock().unwrap().rows {
                if row.manifest.id() == id {
                    row.enabled = on;
                    row.state = if on { RowState::Idle } else { RowState::Off };
                }
            }
            Ok(())
        }

        fn grants_changed(&self, id: &PluginId) {
            self.calls.grants_changed.fetch_add(1, Ordering::SeqCst);
            self.changed.lock().unwrap().push(id.to_string());
        }

        fn set_setting(&self, id: &PluginId, key: &str, value: SettingValue) -> Result<(), String> {
            self.calls.set_setting.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = self.write_error.lock().unwrap().clone() {
                return Err(e);
            }
            for row in &mut self.snapshot.lock().unwrap().rows {
                if row.manifest.id() == id {
                    for (spec, current) in &mut row.settings {
                        if spec.key.as_str() == key {
                            *current = value.clone();
                        }
                    }
                }
            }
            Ok(())
        }

        fn start(&self, id: &PluginId) -> BoxFuture<'static, Result<(), String>> {
            self.calls.start.fetch_add(1, Ordering::SeqCst);
            self.started.lock().unwrap().push(id.to_string());
            let result = match self.start_error.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(()),
            };
            Box::pin(async move { result })
        }
    }

    /// A row for tests and scenes: idle, enabled, nothing effective.
    pub(crate) fn row(manifest: Manifest, component: Option<ComponentDigest>) -> PluginRow {
        PluginRow {
            manifest: Arc::new(manifest),
            component,
            dev: false,
            state: RowState::Idle,
            enabled: true,
            effective: CapabilitySet::new(),
            last_error: None,
            scope_issues: Vec::new(),
            settings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::grant::fixture_manifest;

    #[test]
    fn unavailable_has_no_backend_and_an_empty_book() {
        let s = PluginsStartup::unavailable();
        assert!(s.backend.is_none());
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let key = m.grant_key(None).unwrap();
        assert_eq!(
            s.grants.status(&key),
            crate::plugins::grant::GrantStatus::NeedsPrompt { previous: None }
        );
        assert!(format!("{s:?}").contains("backend: false"));
    }

    #[test]
    fn the_fake_counts_calls_and_follows_its_script() {
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let id = m.id().clone();
        let fake = FakeBackend::new(PluginSnapshot {
            rows: vec![fake_row(m, None)],
            problems: Vec::new(),
        });
        let startup = PluginsStartup::new(Some(fake.clone()), Arc::new(GrantBook::in_memory()));
        let backend = startup.backend.unwrap();
        assert_eq!(FakeBackend::count(&fake.calls.refresh), 0);
        assert_eq!(backend.refresh().rows.len(), 1);
        assert_eq!(backend.snapshot().rows.len(), 1);
        backend.set_enabled(&id, false).unwrap();
        assert_eq!(backend.snapshot().rows[0].state, RowState::Off);
        backend.grants_changed(&id);
        assert_eq!(FakeBackend::count(&fake.calls.refresh), 1);
        assert_eq!(FakeBackend::count(&fake.calls.snapshot), 2);
        assert_eq!(FakeBackend::count(&fake.calls.set_enabled), 1);
        assert_eq!(FakeBackend::count(&fake.calls.grants_changed), 1);
        assert_eq!(fake.changed.lock().unwrap().as_slice(), ["acme-sync"]);
    }
}
