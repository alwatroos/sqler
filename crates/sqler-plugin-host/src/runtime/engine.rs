// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The wasmtime engine, its epoch thread, the linker and the component cache. Created on the first compile and never
//! before (the first frame must not pay for it).

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sqler_plugin_api::ComponentDigest;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, WasmFeatures};

use super::link::add_imports;
use super::state::HostState;
use crate::discovery::{DiscoveredPlugin, MAX_COMPONENT_BYTES};
use crate::error::LoadError;

/// The engine and everything that lives as long as it does.
pub(crate) struct EngineHost {
    pub engine: Engine,
    pub linker: Linker<HostState>,
    cache: Mutex<HashMap<ComponentDigest, Component>>,
    epoch: Epoch,
}

impl EngineHost {
    /// Builds the engine, the linker and the epoch thread.
    ///
    /// # Errors
    /// `NoCompiler` without feature `cl`; `Compile` when the engine cannot be configured.
    pub(crate) fn create(tick: Duration) -> Result<Self, LoadError> {
        // Without the Cranelift feature nothing can be compiled, and `.cwasm` loading needs `unsafe` (P7's call).
        if cfg!(not(feature = "cl")) {
            return Err(LoadError::NoCompiler);
        }
        let engine = Engine::new(&config()).map_err(|e| LoadError::Compile {
            message: format!("engine: {e:#}"),
        })?;
        let mut linker = Linker::new(&engine);
        add_imports(&mut linker).map_err(|e| LoadError::Compile {
            message: format!("linker: {e:#}"),
        })?;
        let epoch = Epoch::start(engine.clone(), tick)?;
        Ok(Self {
            engine,
            linker,
            cache: Mutex::new(HashMap::new()),
            epoch,
        })
    }

    /// The compiled component of `plugin`, and how long compiling took (`None` for a cache hit). Blocking: reads,
    /// hashes and compiles; call it on a blocking thread.
    ///
    /// # Errors
    /// `EntryMissing` for a plugin without a component, `Io`, `ComponentTooLarge`, `ComponentChanged` when the
    /// bytes no longer match the discovery digest, `Compile`.
    pub(crate) fn component(
        &self,
        plugin: &DiscoveredPlugin,
    ) -> Result<(Component, Option<Duration>), LoadError> {
        let file = plugin
            .component
            .as_ref()
            .ok_or_else(|| LoadError::EntryMissing {
                path: plugin.dir.clone(),
            })?;
        if let Some(c) = self.cache().get(&file.digest) {
            return Ok((c.clone(), None));
        }
        let io = |e: std::io::Error| LoadError::Io {
            path: file.path.clone(),
            message: e.to_string(),
        };
        let mut bytes = Vec::new();
        File::open(&file.path)
            .map_err(io)?
            .take(MAX_COMPONENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if len > MAX_COMPONENT_BYTES {
            return Err(LoadError::ComponentTooLarge {
                len,
                max: MAX_COMPONENT_BYTES,
            });
        }
        if ComponentDigest::of(&bytes) != file.digest {
            return Err(LoadError::ComponentChanged);
        }
        let started = Instant::now();
        let component = compile(&self.engine, &bytes)?;
        let took = started.elapsed();
        self.cache().insert(file.digest, component.clone());
        Ok((component, Some(took)))
    }

    /// Marks a call as running: the epoch thread outlives `stop` until the last guard is dropped, so a guest that is
    /// being cancelled still gets its yield points.
    ///
    /// # Errors
    /// [`ShutDown`] once `stop` has been called. The check and the count share the mutex `stop` uses, so a guard is
    /// either handed out before `stop` (and the thread waits for it) or refused; guest code never runs without a
    /// ticking epoch.
    pub(crate) fn in_flight(&self) -> Result<InFlight, ShutDown> {
        let stopped = self
            .epoch
            .shared
            .stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *stopped {
            return Err(ShutDown);
        }
        self.epoch.shared.inflight.fetch_add(1, Ordering::SeqCst);
        Ok(InFlight(Arc::clone(&self.epoch.shared)))
    }

    /// Asks the epoch thread to stop and joins it when no call is running. With calls still running it exits by
    /// itself after the last one.
    pub(crate) fn stop(&self) {
        self.epoch.stop();
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<ComponentDigest, Component>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for EngineHost {
    fn drop(&mut self) {
        self.epoch.stop();
    }
}

#[cfg(feature = "cl")]
fn compile(engine: &Engine, bytes: &[u8]) -> Result<Component, LoadError> {
    Component::new(engine, bytes).map_err(|e| LoadError::Compile {
        message: format!("{e:#}"),
    })
}

// Without Cranelift there is no compiler and `.cwasm` loading would need `unsafe`; `create` already refused, so this
// is only reached through a future change.
#[cfg(not(feature = "cl"))]
fn compile(_engine: &Engine, _bytes: &[u8]) -> Result<Component, LoadError> {
    Err(LoadError::NoCompiler)
}

/// ADR-0054 and ADR-0057: component model, epoch interruption, no fuel, no on-disk cache, no backtrace details;
/// Cranelift at `Speed`.
///
/// Every WebAssembly proposal is pinned to a value this function chose, so a wasmtime update that changes a default
/// cannot widen what a guest may use. The matrix is in ADR-0057. The ones that matter for the memory cap:
/// `threads` (shared memories, whose growth does not go through the `ResourceLimiter`) and `memory64` (4 GiB-plus
/// declarations) are off. Multi-memory stays on because every memory is counted by the limiter, together with the
/// others of the same store.
fn config() -> Config {
    let mut c = Config::new();
    c.wasm_component_model(true)
        .epoch_interruption(true)
        .consume_fuel(false)
        .wasm_backtrace_details(wasmtime::WasmBacktraceDetails::Disable);
    for (feature, on) in FEATURES {
        c.wasm_features(*feature, *on);
    }
    // Belt and braces for the one that matters most: wasmtime would also refuse to create a shared memory by itself.
    c.shared_memory(false);
    #[cfg(feature = "cl")]
    c.cranelift_opt_level(wasmtime::OptLevel::Speed);
    c
}

/// The proposal matrix. `true` is what Rust's `wasm32-wasip2` output and the component model need, plus the
/// harmless standardised ones; `false` is everything else this wasmtime knows.
const FEATURES: &[(WasmFeatures, bool)] = &[
    // On: standardised, used by the toolchains, no effect on the memory accounting.
    (WasmFeatures::BULK_MEMORY, true),
    (WasmFeatures::MULTI_VALUE, true),
    (WasmFeatures::REFERENCE_TYPES, true),
    (WasmFeatures::SIMD, true),
    (WasmFeatures::MUTABLE_GLOBAL, true),
    (WasmFeatures::SATURATING_FLOAT_TO_INT, true),
    (WasmFeatures::SIGN_EXTENSION, true),
    (WasmFeatures::EXTENDED_CONST, true),
    // Every memory is counted by the limiter, together with the others of the store.
    (WasmFeatures::MULTI_MEMORY, true),
    // Off: not needed by the ABI, or outside the limiter.
    (WasmFeatures::THREADS, false),
    (WasmFeatures::SHARED_EVERYTHING_THREADS, false),
    (WasmFeatures::MEMORY64, false),
    (WasmFeatures::GC, false),
    (WasmFeatures::FUNCTION_REFERENCES, false),
    (WasmFeatures::RELAXED_SIMD, false),
    (WasmFeatures::TAIL_CALL, false),
    (WasmFeatures::EXCEPTIONS, false),
    (WasmFeatures::LEGACY_EXCEPTIONS, false),
    (WasmFeatures::STACK_SWITCHING, false),
    (WasmFeatures::WIDE_ARITHMETIC, false),
    (WasmFeatures::CUSTOM_PAGE_SIZES, false),
    (WasmFeatures::CM_ASYNC, false),
    (WasmFeatures::CM_ASYNC_STACKFUL, false),
    (WasmFeatures::CM_MORE_ASYNC_BUILTINS, false),
    (WasmFeatures::CM_THREADING, false),
    (WasmFeatures::CM_ERROR_CONTEXT, false),
    (WasmFeatures::CM_GC, false),
    (WasmFeatures::CM64, false),
    (WasmFeatures::CM_MAP, false),
];

struct EpochShared {
    stop: Mutex<bool>,
    wake: Condvar,
    inflight: AtomicUsize,
}

/// The epoch thread has been told to stop: no guest code may start.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ShutDown;

/// Keeps the epoch thread alive while a call runs.
pub(crate) struct InFlight(Arc<EpochShared>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
        // Wake a stopping thread so it can notice it is the last.
        let _guard = self
            .0
            .stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0.wake.notify_all();
    }
}

struct Epoch {
    shared: Arc<EpochShared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Epoch {
    fn start(engine: Engine, tick: Duration) -> Result<Self, LoadError> {
        let shared = Arc::new(EpochShared {
            stop: Mutex::new(false),
            wake: Condvar::new(),
            inflight: AtomicUsize::new(0),
        });
        let s = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("sqler-plugin-epoch".to_owned())
            .spawn(move || epoch_loop(&engine, tick, &s))
            .map_err(|e| LoadError::Compile {
                message: format!("epoch thread: {e}"),
            })?;
        Ok(Self {
            shared,
            thread: Mutex::new(Some(handle)),
        })
    }

    fn stop(&self) {
        let idle = {
            let mut stop = self
                .shared
                .stop
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *stop = true;
            self.shared.wake.notify_all();
            // Read under the lock `in_flight` takes: no guard can appear after this.
            self.shared.inflight.load(Ordering::SeqCst) == 0
        };
        if idle {
            let handle = self
                .thread
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(h) = handle {
                // A panic in the tick loop is not worth propagating from a shutdown.
                let _ = h.join();
            }
        }
    }
}

fn epoch_loop(engine: &Engine, tick: Duration, shared: &EpochShared) {
    let mut stop = shared
        .stop
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if *stop && shared.inflight.load(Ordering::SeqCst) == 0 {
            return;
        }
        let (guard, _) = shared
            .wake
            .wait_timeout(stop, tick)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        stop = guard;
        engine.increment_epoch();
    }
}
