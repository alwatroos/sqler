// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `PluginDrivers`: the [`DriverSource`] that offers the drivers of plugins to the registry.
//!
//! Availability is worked out from the host's state on every lookup, so enable, disable, revoke and reload need no
//! hook in the plugin lifecycle: the next `ids` or `lookup` sees the new state.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, Weak};

use sqler_driver_api::sqler_core::DriverId;
use sqler_driver_api::{Driver, DriverError, DriverSource, SourceLookup};
use sqler_plugin_api::{
    Capability, CapabilityKind, DriverContribution, DriverKind, EngineId, PluginId,
};

use super::error;
use super::guest::GuestDriver;
use super::host_engine::HostEngineDriver;
use crate::discovery::DiscoveredPlugin;
use crate::runtime::PluginHost;
use crate::status::{PluginState, PluginStatus};

/// The native engines this build links, by the id a plugin names in `driver.host:<engine>`.
///
/// The binaries fill it from the engine crates behind their own features. A native engine must not assume that
/// `config.driver` equals its own id: the connection carries the id of the plugin driver.
#[derive(Default)]
pub struct HostEngines {
    engines: BTreeMap<EngineId, Arc<dyn Driver>>,
}

impl HostEngines {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an engine.
    ///
    /// # Errors
    /// [`DriverError::DuplicateDriver`] when the engine is already in the table.
    pub fn register(
        &mut self,
        engine: EngineId,
        driver: Arc<dyn Driver>,
    ) -> Result<(), DriverError> {
        if self.engines.contains_key(&engine) {
            return Err(DriverError::DuplicateDriver(engine.to_string()));
        }
        self.engines.insert(engine, driver);
        Ok(())
    }

    pub(crate) fn get(&self, engine: &EngineId) -> Option<Arc<dyn Driver>> {
        self.engines.get(engine).cloned()
    }
}

impl std::fmt::Debug for HostEngines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.engines.keys()).finish()
    }
}

/// One driver a plugin's manifest declares, with the plugin's state at the time of the lookup.
struct Claim {
    plugin: Arc<DiscoveredPlugin>,
    status: PluginStatus,
    driver: DriverContribution,
}

impl Claim {
    fn plugin_id(&self) -> &PluginId {
        self.plugin.manifest.id()
    }
}

/// The drivers of plugins as a [`DriverSource`].
///
/// Create it with the engines the binary links, add it to the registry, and [`attach`](Self::attach) the plugin host
/// once it exists. Until then it offers nothing.
pub struct PluginDrivers {
    host: OnceLock<PluginHost>,
    engines: HostEngines,
    this: Weak<Self>,
}

impl PluginDrivers {
    /// A source with no host yet.
    #[must_use]
    pub fn new(engines: HostEngines) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            host: OnceLock::new(),
            engines,
            this: this.clone(),
        })
    }

    /// Connects the source to the plugin host. The first call wins; later ones are ignored.
    pub fn attach(&self, host: &PluginHost) {
        let _ = self.host.set(host.clone());
    }

    /// Every driver of every registered plugin, in load order.
    fn claims(&self) -> Vec<Claim> {
        let Some(host) = self.host.get() else {
            return Vec::new();
        };
        let mut claims = Vec::new();
        for (plugin, status) in host.driver_plugins() {
            for driver in &plugin.manifest.contributes().drivers {
                claims.push(Claim {
                    plugin: Arc::clone(&plugin),
                    status: status.clone(),
                    driver: driver.clone(),
                });
            }
        }
        claims
    }

    /// Whether the driver of claim `at` can be used now: the four rules in their fixed order.
    fn check(&self, claims: &[Claim], at: usize) -> Result<(), String> {
        let claim = &claims[at];
        let driver = claim.driver.id.as_str();
        let plugin = claim.plugin_id().as_str();
        if matches!(claim.status.state, PluginState::Disabled { .. }) {
            return Err(error::disabled(driver, plugin));
        }
        match &claim.driver.kind {
            DriverKind::Host { engine } => {
                let cap = Capability::DriverHost(engine.clone());
                if !claim.status.effective.contains(&cap) {
                    return Err(error::not_approved(driver, plugin, &cap.to_string()));
                }
                if self.engines.get(engine).is_none() {
                    return Err(error::no_engine(driver, engine.as_str()));
                }
            }
            _ => {
                if !claim.status.effective.allows(CapabilityKind::DriverGuest) {
                    return Err(error::not_approved(driver, plugin, "driver.guest"));
                }
            }
        }
        if let Some(first) = claims[..at]
            .iter()
            .find(|c| c.driver.id == claim.driver.id && c.plugin_id() != claim.plugin_id())
        {
            return Err(error::collision(driver, first.plugin_id().as_str()));
        }
        Ok(())
    }

    /// Whether `plugin`'s driver `driver` can be used now, or the fixed text that says why not. Reads the plugin
    /// host's state and the grants: do not call it on the UI thread.
    ///
    /// # Errors
    /// The fixed text of the first rule that fails.
    pub fn availability(&self, plugin: &PluginId, driver: &str) -> Result<(), String> {
        let claims = self.claims();
        match claims
            .iter()
            .position(|c| c.plugin_id() == plugin && c.driver.id.as_str() == driver)
        {
            Some(at) => self.check(&claims, at),
            None => Err(error::NOT_AVAILABLE.to_owned()),
        }
    }

    fn adapter(&self, claim: &Claim, host: &PluginHost) -> Option<Arc<dyn Driver>> {
        let gate = Gate {
            source: self.this.clone(),
            plugin: claim.plugin_id().clone(),
            driver: claim.driver.id.as_str().to_owned(),
        };
        Some(match &claim.driver.kind {
            DriverKind::Host { engine } => {
                Arc::new(HostEngineDriver::new(gate, self.engines.get(engine)?))
            }
            _ => Arc::new(GuestDriver::new(gate, host.clone())),
        })
    }
}

impl std::fmt::Debug for PluginDrivers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginDrivers")
            .field("attached", &self.host.get().is_some())
            .field("engines", &self.engines)
            .finish_non_exhaustive()
    }
}

impl DriverSource for PluginDrivers {
    fn ids(&self) -> Vec<DriverId> {
        let claims = self.claims();
        let mut ids: Vec<DriverId> = (0..claims.len())
            .filter(|&at| self.check(&claims, at).is_ok())
            .filter_map(|at| DriverId::new(claims[at].driver.id.as_str()).ok())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    fn lookup(&self, id: &DriverId) -> SourceLookup {
        let Some(host) = self.host.get() else {
            return SourceLookup::Unknown;
        };
        let claims = self.claims();
        let Some(at) = claims
            .iter()
            .position(|c| c.driver.id.as_str() == id.as_str())
        else {
            return SourceLookup::Unknown;
        };
        match self.check(&claims, at) {
            Err(text) => SourceLookup::Unavailable(text),
            Ok(()) => self.adapter(&claims[at], host).map_or_else(
                || SourceLookup::Unavailable(error::NOT_AVAILABLE.to_owned()),
                SourceLookup::Available,
            ),
        }
    }
}

/// What an adapter holds to ask the source whether its driver is still usable.
#[derive(Clone)]
pub(super) struct Gate {
    source: Weak<PluginDrivers>,
    pub(super) plugin: PluginId,
    pub(super) driver: String,
}

impl Gate {
    /// Re-checks the four rules. The text of the first failing rule is the error.
    pub(super) fn check(&self) -> Result<(), DriverError> {
        let Some(source) = self.source.upgrade() else {
            return Err(DriverError::Unavailable(error::NOT_AVAILABLE.to_owned()));
        };
        source
            .availability(&self.plugin, &self.driver)
            .map_err(DriverError::Unavailable)
    }
}
