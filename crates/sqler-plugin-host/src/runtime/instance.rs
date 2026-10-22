// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Creating a live instance: compile (cached), link, instantiate, run `lifecycle.init`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqler_plugin_api::{DriverKind, PluginId};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use wasmtime::component::{Component, Instance, InstancePre};
use wasmtime::{Store, Trap};
use wasmtime_wasi::I32Exit;

use super::bindings::exports::sqler::plugin::lifecycle;
use super::engine::EngineHost;
use super::host::{EngineError, Inner, PluginEntry, shut_down_error};
use super::http::{Budgets, EgressHooks};
use super::limiter::PluginLimiter;
use super::state::HostState;
use super::wasi;
use crate::error::{CallError, GuestError, LoadError};
use crate::grant::effective;
use crate::scope::resolve_preopens;
use crate::status::CallKind;

/// A plugin that is instantiated.
pub(crate) struct Live {
    pub store: Store<HostState>,
    // Read by the items that resolve exports from `GuestIndices` (P8-P12).
    #[allow(dead_code)]
    pub instance: Instance,
    #[allow(dead_code)]
    pub pre: InstancePre<HostState>,
    /// The grant generation it was built under; a refresh makes it stale.
    pub generation: u64,
}

/// What a call produced before the host looked at the store.
pub(super) enum Bounded<T> {
    Done(wasmtime::Result<T>),
    Timeout,
    Cancelled,
}

/// Runs `fut` until it finishes, the wall-clock `deadline` passes, or `cancel` fires. The guest yields to the
/// executor at every epoch tick, so the timer and the token are polled even while it spins or waits in a host call.
pub(super) async fn bounded<T>(
    deadline: Duration,
    cancel: &CancellationToken,
    fut: impl Future<Output = wasmtime::Result<T>>,
) -> Bounded<T> {
    tokio::select! {
        biased;
        r = fut => Bounded::Done(r),
        () = cancel.cancelled() => Bounded::Cancelled,
        () = sleep(deadline) => Bounded::Timeout,
    }
}

const MESSAGE_CHARS: usize = 512;

fn short(message: String) -> String {
    if message.chars().count() <= MESSAGE_CHARS {
        message
    } else {
        message
            .chars()
            .take(MESSAGE_CHARS)
            .chain("...".chars())
            .collect()
    }
}

/// Maps the outcome of a bounded call to the error taxonomy, using what the store knows (the memory limiter).
pub(super) fn classify<T>(
    state: &HostState,
    deadline: Duration,
    b: Bounded<T>,
) -> Result<T, CallError> {
    match b {
        Bounded::Done(Ok(v)) => Ok(v),
        Bounded::Timeout => Err(CallError::Timeout { after: deadline }),
        Bounded::Cancelled => Err(CallError::Cancelled),
        Bounded::Done(Err(e)) => Err(if state.limiter.hit() {
            CallError::MemoryLimit {
                limit: state.limiter.cap(),
            }
        } else if let Some(c) = e.downcast_ref::<CallError>() {
            c.clone()
        } else if e.downcast_ref::<Trap>().is_some() {
            CallError::Trap {
                message: short(format!("{e:#}")),
            }
        } else if let Some(exit) = e.downcast_ref::<I32Exit>() {
            // `wasi:cli/exit` is linked, but a plugin has no process to leave: an exit is a fault like any trap, so a
            // guest that exits on every call is disabled by the three-faults rule.
            CallError::Trap {
                message: format!("guest exited (status {})", exit.0),
            }
        } else {
            CallError::Engine {
                message: short(format!("{e:#}")),
            }
        }),
    }
}

/// Why an instantiation failed, and whether it counts toward the three-faults rule.
pub(super) struct Failure {
    pub error: CallError,
    pub fault: bool,
}

impl Failure {
    fn load(e: LoadError) -> Self {
        Self {
            error: CallError::Load(e),
            fault: false,
        }
    }
}

struct Prepared {
    host: Arc<EngineHost>,
    component: Component,
    state: HostState,
}

/// The blocking half: engine (first time), compile, WASI context with its preopens, host state.
fn prepare(
    inner: &Inner,
    entry: &PluginEntry,
    approved: Option<&sqler_plugin_api::CapabilitySet>,
) -> Result<Prepared, Failure> {
    let host = inner.engine().map_err(|e| match e {
        EngineError::ShutDown => Failure {
            error: shut_down_error(),
            fault: false,
        },
        EngineError::Load(e) => Failure::load(e),
    })?;
    let plugin = Arc::clone(&entry.plugin);
    let (component, compiled) = host.component(&plugin).map_err(Failure::load)?;
    if let Some(took) = compiled {
        inner.record(|m| &mut m.compile, took);
    }
    let effective = effective(&plugin.manifest.capabilities, approved);
    let (preopens, issues) = resolve_preopens(&effective, &plugin.roots);
    entry
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .scope_issues
        .clone_from(&issues);
    let id: PluginId = plugin.manifest.id.clone();
    let log = Arc::clone(&entry.log);
    let wasi = wasi::build(&id, &preopens, &issues, &log);
    let egress = inner.opts.egress.for_plugin(&id, &effective);
    let guest_driver = plugin
        .manifest
        .contributes
        .drivers
        .iter()
        .any(|d| matches!(d.kind, DriverKind::Guest));
    let limits = &inner.opts.limits;
    let cap = if guest_driver {
        limits.driver_memory_bytes
    } else {
        limits.memory_bytes
    };
    // The host's own limits win over anything the embedder put under the same type.
    let mut services = inner.opts.services.clone();
    services.insert(Arc::new(limits.query));
    let state = HostState::new(
        plugin,
        effective,
        services,
        Arc::clone(&inner.opts.settings),
        PluginLimiter::new(cap),
        log,
        wasi,
        EgressHooks::new(
            egress,
            Budgets {
                plugin: Arc::clone(&entry.egress),
                global: Arc::clone(&inner.egress_global),
            },
        ),
    );
    Ok(Prepared {
        host,
        component,
        state,
    })
}

/// Builds a [`Live`] with fresh grants and runs `init` under the init deadline.
pub(super) async fn instantiate(
    inner: &Arc<Inner>,
    entry: &Arc<PluginEntry>,
    cancel: &CancellationToken,
) -> Result<Live, Failure> {
    let approved = inner.opts.grants.approved(&entry.plugin.key);
    let generation = entry.generation();
    let blocking = {
        let (inner, entry) = (Arc::clone(inner), Arc::clone(entry));
        tokio::task::spawn_blocking(move || prepare(&inner, &entry, approved.as_ref()))
    };
    let prepared = tokio::select! {
        biased;
        () = cancel.cancelled() => {
            return Err(Failure { error: CallError::Cancelled, fault: false });
        }
        r = blocking => match r {
            Ok(Ok(p)) => p,
            Ok(Err(f)) => return Err(f),
            Err(_) => return Err(Failure { error: CallError::HostPanic, fault: true }),
        },
    };
    let Prepared {
        host,
        component,
        state,
    } = prepared;
    // The epoch thread must outlive this instantiation even if the host is shut down meanwhile.
    let Ok(_flight) = host.in_flight() else {
        return Err(Failure {
            error: shut_down_error(),
            fault: false,
        });
    };

    let mut store = Store::new(&host.engine, state);
    store.limiter(|s| &mut s.limiter);
    let pre = host.linker.instantiate_pre(&component).map_err(|e| {
        Failure::load(LoadError::Link {
            message: short(format!("{e:#}")),
        })
    })?;
    let indices = lifecycle::GuestIndices::new(&pre).map_err(|_| Failure {
        error: CallError::MissingExport {
            interface: "lifecycle",
        },
        fault: false,
    })?;

    let deadline = CallKind::Init.deadline(&inner.opts.limits.deadlines);
    store.epoch_deadline_async_yield_and_update(1);
    let mut times = (Duration::ZERO, Duration::ZERO);
    let ran = bounded(deadline, cancel, async {
        let t = Instant::now();
        let instance = pre.instantiate_async(&mut store).await?;
        times.0 = t.elapsed();
        let guest = indices.load(&mut store, &instance)?;
        let t = Instant::now();
        let init = guest.call_init(&mut store).await?;
        times.1 = t.elapsed();
        Ok((instance, init))
    })
    .await;
    if matches!(ran, Bounded::Done(Ok(_))) {
        inner.record(|m| &mut m.instantiate, times.0);
        inner.record(|m| &mut m.init, times.1);
    }
    match classify(store.data(), deadline, ran) {
        Ok((instance, Ok(()))) => Ok(Live {
            store,
            instance,
            pre,
            generation,
        }),
        // An `init` that reports an error counts as a fault: the plugin did not start.
        Ok((_, Err(e))) => Err(Failure {
            error: CallError::Guest(GuestError::from(e)),
            fault: true,
        }),
        Err(error) => {
            let fault = error.is_fault();
            Err(Failure { error, fault })
        }
    }
}
