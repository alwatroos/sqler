// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `PluginHost`: the registry of plugins, their state, and the lifecycle verbs the app calls.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use sqler_plugin_api::{CapabilitySet, Limits, PluginId, SettingValue};
use tokio_util::sync::CancellationToken;

use super::call::PendingCall;
use super::engine::EngineHost;
use super::http::Budget;
use super::instance::Live;
use super::log_host::SharedLimiter;
use super::services::Services;
use crate::discovery::{DiscoveredPlugin, Discovery, LoadProblem};
use crate::egress::EgressFactory;
use crate::error::{CallError, LoadError, SettingsError};
use crate::grant::{GrantSource, effective};
use crate::log::LogLimiter;
use crate::scope::ScopeIssue;
use crate::settings::{SettingsStore, write_setting};
use crate::status::{DisableReason, PluginState, PluginStatus};

/// Consecutive faults that disable a plugin.
pub(crate) const MAX_FAULTS: u32 = 3;

/// Timings of the host's own phases, for `examples/probe.rs` and size/cost reports. Each list keeps the first
/// [`METRIC_SAMPLES`] samples.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    /// Creating the engine, linker and epoch thread (once).
    pub engine_create: Option<Duration>,
    /// Compiling a component (cache misses only).
    pub compile: Vec<Duration>,
    /// `instantiate_async` of a linked component.
    pub instantiate: Vec<Duration>,
    /// `lifecycle.init` of a fresh instance.
    pub init: Vec<Duration>,
}

/// Samples kept per metric.
pub(crate) const METRIC_SAMPLES: usize = 4096;

/// Everything the host needs from the app.
pub struct HostOptions {
    /// The runtime calls run on. The host never creates one.
    pub runtime: tokio::runtime::Handle,
    /// What the user approved.
    pub grants: Arc<dyn GrantSource>,
    /// Where setting values live.
    pub settings: Arc<dyn SettingsStore>,
    /// Outgoing HTTP, per plugin.
    pub egress: Arc<dyn EgressFactory>,
    /// Memory caps, deadlines and the epoch tick.
    pub limits: Limits,
    /// Host traits for later items.
    pub services: Services,
}

/// What can change about a plugin while it is registered.
pub(super) struct EntryState {
    pub disabled: Option<DisableReason>,
    pub faults: u32,
    pub live: bool,
    /// `fs.read` scopes the last instantiation could not preopen (and why), for the plugin list.
    pub scope_issues: Vec<ScopeIssue>,
}

/// The error a call gets once the host is shut down.
pub(super) fn shut_down_error() -> CallError {
    CallError::Engine {
        message: "the plugin host is shut down".to_owned(),
    }
}

/// Why the engine is not available.
pub(super) enum EngineError {
    ShutDown,
    Load(LoadError),
}

/// One registered plugin.
pub(super) struct PluginEntry {
    pub plugin: Arc<DiscoveredPlugin>,
    /// The log rate limit. It belongs to the plugin, not to an instance: re-instantiating after a fault or an exit
    /// must not refill the bucket.
    pub log: SharedLimiter,
    /// The egress budget of the plugin; like the log limit it outlives instances, so a restart does not refill it.
    pub egress: Arc<Budget>,
    /// The instance. Held for the whole of a call, which serialises the calls of one plugin.
    pub live: tokio::sync::Mutex<Option<Live>>,
    pub state: Mutex<EntryState>,
    /// Cancels every call of this plugin (disable, unload, shutdown); replaced after use.
    kill: Mutex<CancellationToken>,
    generation: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl PluginEntry {
    fn new(plugin: Arc<DiscoveredPlugin>) -> Self {
        Self {
            plugin,
            log: Arc::new(Mutex::new(LogLimiter::new())),
            egress: Budget::per_plugin(),
            live: tokio::sync::Mutex::new(None),
            state: Mutex::new(EntryState {
                disabled: None,
                faults: 0,
                live: false,
                scope_issues: Vec::new(),
            }),
            kill: Mutex::new(CancellationToken::new()),
            generation: AtomicU64::new(0),
        }
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub(super) fn call_token(&self) -> CancellationToken {
        lock(&self.kill).child_token()
    }

    pub(super) fn set_live(&self, live: bool) {
        lock(&self.state).live = live;
    }

    /// The error a call gets while the plugin is disabled.
    pub(super) fn disabled_error(&self) -> Option<CallError> {
        lock(&self.state).disabled.as_ref().map(|r| match r {
            DisableReason::Load(e) => CallError::Load(e.clone()),
            _ => CallError::Disabled,
        })
    }

    /// Cancels the calls in flight; later calls get a fresh token.
    fn kill_calls(&self) {
        let old = std::mem::replace(&mut *lock(&self.kill), CancellationToken::new());
        old.cancel();
    }
}

/// Shared by every clone of the host.
pub(super) struct Inner {
    pub opts: HostOptions,
    engine: OnceLock<Result<Arc<EngineHost>, LoadError>>,
    /// Serialises creating the engine against `shutdown` stopping it, so an engine is never created after the stop.
    engine_gate: Mutex<()>,
    touched: AtomicBool,
    pub shutdown: AtomicBool,
    /// The egress budget shared by every plugin of this host.
    pub egress_global: Arc<Budget>,
    registry: Mutex<Registry>,
    metrics: Mutex<Metrics>,
}

#[derive(Default)]
struct Registry {
    order: Vec<PluginId>,
    map: HashMap<PluginId, Arc<PluginEntry>>,
}

impl Inner {
    /// The engine, created on first use and never after `shutdown`. Blocking (it starts a thread): call it on a
    /// blocking thread.
    pub(super) fn engine(&self) -> Result<Arc<EngineHost>, EngineError> {
        let _gate = lock(&self.engine_gate);
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(EngineError::ShutDown);
        }
        self.touched.store(true, Ordering::SeqCst);
        self.engine
            .get_or_init(|| {
                let t = Instant::now();
                let host = EngineHost::create(self.opts.limits.epoch_tick).map(Arc::new);
                lock(&self.metrics).engine_create = Some(t.elapsed());
                host
            })
            .clone()
            .map_err(EngineError::Load)
    }

    /// Adds a sample to one of the metric lists, up to the cap.
    pub(super) fn record(
        &self,
        pick: impl FnOnce(&mut Metrics) -> &mut Vec<Duration>,
        d: Duration,
    ) {
        let mut m = lock(&self.metrics);
        let list = pick(&mut m);
        if list.len() < METRIC_SAMPLES {
            list.push(d);
        }
    }

    pub(super) fn entry(&self, id: &PluginId) -> Option<Arc<PluginEntry>> {
        lock(&self.registry).map.get(id).cloned()
    }
}

/// The plugin host. Cheap to clone; every clone is the same host.
///
/// `new` does nothing: no I/O, no engine, no thread. Discovery, compile and the epoch thread start only on explicit
/// calls, so a host in the app's first frame costs nothing (`is_idle` proves it).
#[derive(Clone)]
pub struct PluginHost {
    pub(super) inner: Arc<Inner>,
}

impl PluginHost {
    /// Creates the host. Does no work.
    #[must_use]
    pub fn new(opts: HostOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                opts,
                engine: OnceLock::new(),
                engine_gate: Mutex::new(()),
                touched: AtomicBool::new(false),
                shutdown: AtomicBool::new(false),
                egress_global: Budget::global(),
                registry: Mutex::new(Registry::default()),
                metrics: Mutex::new(Metrics::default()),
            }),
        }
    }

    /// True until the first compile: no engine, no epoch thread, no work has been done.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        !self.inner.touched.load(Ordering::SeqCst)
    }

    /// Registers the discovered plugins and replaces the previous set. Nothing is compiled. A plugin whose directory
    /// and grant key are unchanged keeps its instance and fault count; every other previous plugin is unloaded.
    /// Returns the discovery problems unchanged, for the app to show.
    #[must_use]
    pub fn load(&self, d: Discovery) -> Vec<LoadProblem> {
        let mut next = Registry::default();
        let mut gone = Vec::new();
        {
            let mut reg = lock(&self.inner.registry);
            for p in d.plugins {
                let id = p.manifest.id().clone();
                if next.map.contains_key(&id) {
                    continue;
                }
                let entry = match reg.map.remove(&id) {
                    Some(old) if old.plugin.key == p.key && old.plugin.dir == p.dir => old,
                    Some(old) => {
                        gone.push(old);
                        Arc::new(PluginEntry::new(Arc::new(p)))
                    }
                    None => Arc::new(PluginEntry::new(Arc::new(p))),
                };
                next.order.push(id.clone());
                next.map.insert(id, entry);
            }
            gone.extend(reg.map.drain().map(|(_, e)| e));
            *reg = next;
        }
        for e in gone {
            self.retire(&e);
        }
        d.problems
    }

    /// The phase timings recorded so far.
    #[doc(hidden)]
    #[must_use]
    pub fn metrics(&self) -> Metrics {
        lock(&self.inner.metrics).clone()
    }

    /// The registered plugins in load order.
    #[must_use]
    pub fn plugins(&self) -> Vec<PluginStatus> {
        let entries: Vec<_> = {
            let reg = lock(&self.inner.registry);
            reg.order
                .iter()
                .filter_map(|id| reg.map.get(id).cloned())
                .collect()
        };
        entries
            .iter()
            .map(|e| {
                let m = &e.plugin.manifest;
                let (state, scope_issues) = {
                    let st = lock(&e.state);
                    let state = match &st.disabled {
                        Some(reason) => PluginState::Disabled {
                            reason: reason.clone(),
                        },
                        None if st.live => PluginState::Live,
                        None => PluginState::Idle,
                    };
                    (state, st.scope_issues.clone())
                };
                let approved = self.inner.opts.grants.approved(&e.plugin.key);
                PluginStatus {
                    id: m.id().clone(),
                    state,
                    effective: effective(m.capabilities(), approved.as_ref()),
                    key: e.plugin.key.clone(),
                    scope_issues,
                }
            })
            .collect()
    }

    /// The discovered plugin and its effective capabilities now, for the typed calls that check a declaration before
    /// they reach the guest.
    pub(crate) fn plugin_view(
        &self,
        id: &PluginId,
    ) -> Option<(Arc<DiscoveredPlugin>, CapabilitySet)> {
        let entry = self.inner.entry(id)?;
        let approved = self.inner.opts.grants.approved(&entry.plugin.key);
        let caps = effective(entry.plugin.manifest.capabilities(), approved.as_ref());
        Some((Arc::clone(&entry.plugin), caps))
    }

    /// Instantiates the plugin if needed (which runs `lifecycle.init`). A no-op for a live plugin.
    pub fn init(&self, id: &PluginId) -> PendingCall<()> {
        self.run(
            id,
            crate::status::CallKind::Init,
            Box::new(|_| Box::pin(async { Ok(()) })),
        )
    }

    /// Cancels the calls in flight (they end with `Cancelled`, so a revoked capability cannot keep working until the
    /// call's deadline) and drops the instance; the next call relinks with the grants as they are then.
    pub fn refresh_grants(&self, id: &PluginId) {
        if let Some(e) = self.inner.entry(id) {
            e.generation.fetch_add(1, Ordering::SeqCst);
            e.set_live(false);
            self.retire_calls(&e);
        }
    }

    /// Lifts a disable (user or faults) and resets the fault count.
    pub fn enable(&self, id: &PluginId) {
        if let Some(e) = self.inner.entry(id) {
            let mut st = lock(&e.state);
            st.disabled = None;
            st.faults = 0;
        }
    }

    /// Disables the plugin: calls in flight are cancelled and the instance is dropped.
    pub fn disable(&self, id: &PluginId) {
        if let Some(e) = self.inner.entry(id) {
            {
                let mut st = lock(&e.state);
                st.disabled = Some(DisableReason::User);
                st.live = false;
            }
            self.retire_calls(&e);
        }
    }

    /// Removes the plugin. There is no shutdown export in the ABI, so unloading drops the store.
    pub fn unload(&self, id: &PluginId) {
        let removed = {
            let mut reg = lock(&self.inner.registry);
            reg.order.retain(|i| i != id);
            reg.map.remove(id)
        };
        if let Some(e) = removed {
            self.retire(&e);
        }
    }

    /// Writes a setting of a registered plugin (the app's side; guests only read).
    ///
    /// # Errors
    /// [`SettingsError::Undeclared`] for an unknown plugin or key, plus those of `write_setting`.
    pub fn set_setting(
        &self,
        id: &PluginId,
        key: &str,
        v: SettingValue,
    ) -> Result<(), SettingsError> {
        let entry = self
            .inner
            .entry(id)
            .ok_or_else(|| SettingsError::Undeclared {
                key: key.chars().take(64).collect(),
            })?;
        write_setting(&entry.plugin.manifest, &*self.inner.opts.settings, key, v)
    }

    /// Drops every store and stops the epoch thread (joined when no call is running, else it exits after the last).
    /// Calls made afterwards fail.
    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        let entries: Vec<_> = {
            let mut reg = lock(&self.inner.registry);
            reg.order.clear();
            reg.map.drain().map(|(_, e)| e).collect()
        };
        for e in &entries {
            self.retire(e);
        }
        // Stores that were free are already gone; busy ones are dropped by their cancelled calls. Wait for neither.
        // Under the gate: an engine being created now is stopped here, and none is created after.
        let _gate = lock(&self.inner.engine_gate);
        if let Some(Ok(engine)) = self.inner.engine.get() {
            engine.stop();
        }
    }

    fn retire(&self, e: &Arc<PluginEntry>) {
        lock(&e.state).live = false;
        self.retire_calls(e);
    }

    /// Cancels the calls in flight and drops the instance.
    fn retire_calls(&self, e: &Arc<PluginEntry>) {
        e.kill_calls();
        self.drop_live(e);
    }

    /// Drops the instance now when no call holds it, else when the call is done.
    fn drop_live(&self, e: &Arc<PluginEntry>) {
        if let Ok(mut g) = e.live.try_lock() {
            *g = None;
            return;
        }
        let e = Arc::clone(e);
        self.inner.opts.runtime.spawn(async move {
            *e.live.lock().await = None;
        });
    }
}
