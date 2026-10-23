// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The data in every plugin's `Store`.

use std::sync::Arc;

use sqler_plugin_api::CapabilitySet;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use super::http::EgressHooks;
use super::limiter::PluginLimiter;
use super::log_host::SharedLimiter;
use super::services::Services;
use crate::discovery::DiscoveredPlugin;
use crate::settings::SettingsStore;

/// Entries (streams, http fields, bodies) the WASI resource table of one plugin may hold. The wasmtime default is a
/// million; the plugins here need a few dozen at a time.
pub(crate) const MAX_RESOURCES: usize = 10_000;

/// Everything a host function needs about the plugin it serves. Later items read their host traits from `services`.
pub(crate) struct HostState {
    pub plugin: Arc<DiscoveredPlugin>,
    /// Declared and approved at the time of instantiation.
    pub effective: CapabilitySet,
    pub services: Services,
    pub settings: Arc<dyn SettingsStore>,
    pub limiter: PluginLimiter,
    pub log: SharedLimiter,
    wasi: WasiCtx,
    table: ResourceTable,
    http: WasiHttpCtx,
    hooks: EgressHooks,
}

impl HostState {
    // One argument per piece of per-plugin state; a builder would only rename them.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        plugin: Arc<DiscoveredPlugin>,
        effective: CapabilitySet,
        services: Services,
        settings: Arc<dyn SettingsStore>,
        limiter: PluginLimiter,
        log: SharedLimiter,
        wasi: WasiCtx,
        hooks: EgressHooks,
    ) -> Self {
        let mut table = ResourceTable::new();
        table.set_max_capacity(MAX_RESOURCES);
        Self {
            plugin,
            effective,
            services,
            settings,
            limiter,
            log,
            wasi,
            table,
            http: WasiHttpCtx::new(),
            hooks,
        }
    }
}

#[cfg(all(test, feature = "cl"))]
impl HostState {
    /// The WASI resource table, for the tests of its cap.
    pub(crate) fn resources(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.hooks,
        }
    }
}
