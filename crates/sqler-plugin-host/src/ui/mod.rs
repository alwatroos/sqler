// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

// The generated host trait is async; the implementation has nothing to await.
#![allow(clippy::unused_async_trait_impl)]

//! The `ui` import and the typed `ui-handler` calls (P8).
//!
//! The application implements [`UiSink`] and inserts one [`PluginUi`] into [`Services`](crate::Services). A guest's
//! `ui.refresh-panel`, `ui.set-status` and `ui.notify` then pass four checks, in order: a sink exists; the panel or
//! item is declared in the manifest; `refresh-panel` has `ui.panel` in the effective set; the plugin is under its
//! rate. A call that fails any of them is dropped with one `tracing::debug!` line and the guest sees nothing. Text is
//! cut to one line without control characters before it reaches the sink, so a plugin cannot forge a second status
//! item or a terminal escape. The typed calls into the guest are in [`calls`].

mod calls;
mod convert;
mod limits;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use sqler_plugin_api::{CapabilityKind, CapabilitySet, ContributionId, PluginId};

use self::limits::{NOTIFY_MAX_BYTES, STATUS_MAX_BYTES, UiLimits, one_line};
use crate::discovery::DiscoveredPlugin;
use crate::runtime::HostState;
use crate::runtime::wit::ui as wit;

/// Where a plugin's UI requests go. Implemented by the application; called from a plugin's call task, so it must
/// not block.
pub trait UiSink: Send + Sync {
    /// The plugin asked for `panel` to be rendered again.
    fn refresh_panel(&self, plugin: &PluginId, panel: &ContributionId);
    /// The plugin set the text of `item` (one line, possibly empty, at most 256 bytes).
    fn set_status(&self, plugin: &PluginId, item: &ContributionId, text: String);
    /// The plugin shows a notification (one line, at most 512 bytes, never empty).
    fn notify(&self, plugin: &PluginId, message: String);
}

/// The `ui` import service: the sink and the per-plugin rate limits. Limits belong to the plugin, not to an
/// instance, so a restart does not refill them.
pub struct PluginUi {
    sink: Arc<dyn UiSink>,
    limits: Mutex<HashMap<PluginId, UiLimits>>,
}

impl PluginUi {
    /// A service forwarding to `sink`.
    #[must_use]
    pub fn new(sink: Arc<dyn UiSink>) -> Self {
        Self {
            sink,
            limits: Mutex::new(HashMap::new()),
        }
    }

    /// Takes a token from the bucket `pick` selects; `false` means over the rate.
    fn admit(
        &self,
        plugin: &PluginId,
        now: Instant,
        pick: impl FnOnce(&mut UiLimits) -> &mut limits::Bucket,
    ) -> bool {
        let mut map = self
            .limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pick(map.entry(plugin.clone()).or_insert_with(UiLimits::new)).take(now)
    }

    /// `ui.refresh-panel` from `plugin`.
    pub(crate) fn refresh_panel(
        &self,
        plugin: &DiscoveredPlugin,
        effective: &CapabilitySet,
        panel: &str,
        now: Instant,
    ) {
        let id = plugin.manifest.id();
        let Some(declared) = plugin
            .manifest
            .contributes()
            .panels
            .iter()
            .find(|p| p.id.as_str() == panel)
        else {
            tracing::debug!(plugin = %id, "ui.refresh-panel dropped: panel is not declared");
            return;
        };
        if !effective.allows(CapabilityKind::UiPanel) {
            tracing::debug!(plugin = %id, "ui.refresh-panel dropped: `ui.panel` is not granted");
            return;
        }
        if !self.admit(id, now, |l| &mut l.refresh) {
            tracing::debug!(plugin = %id, "ui.refresh-panel dropped: rate limit");
            return;
        }
        self.sink.refresh_panel(id, &declared.id);
    }

    /// `ui.set-status` from `plugin`.
    pub(crate) fn set_status(
        &self,
        plugin: &DiscoveredPlugin,
        item: &str,
        text: &str,
        now: Instant,
    ) {
        let id = plugin.manifest.id();
        let Some(declared) = plugin
            .manifest
            .contributes()
            .status_items
            .iter()
            .find(|s| s.id.as_str() == item)
        else {
            tracing::debug!(plugin = %id, "ui.set-status dropped: item is not declared");
            return;
        };
        if !self.admit(id, now, |l| &mut l.status) {
            tracing::debug!(plugin = %id, "ui.set-status dropped: rate limit");
            return;
        }
        self.sink
            .set_status(id, &declared.id, one_line(text, STATUS_MAX_BYTES));
    }

    /// `ui.notify` from `plugin`.
    pub(crate) fn notify(&self, plugin: &DiscoveredPlugin, message: &str, now: Instant) {
        let id = plugin.manifest.id();
        let message = one_line(message, NOTIFY_MAX_BYTES);
        if message.is_empty() {
            tracing::debug!(plugin = %id, "ui.notify dropped: empty message");
            return;
        }
        if !self.admit(id, now, |l| &mut l.notify) {
            tracing::debug!(plugin = %id, "ui.notify dropped: rate limit");
            return;
        }
        self.sink.notify(id, message);
    }
}

impl std::fmt::Debug for PluginUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginUi").finish_non_exhaustive()
    }
}

impl HostState {
    fn plugin_ui(&self) -> Option<Arc<PluginUi>> {
        let ui = self.services.get::<PluginUi>();
        if ui.is_none() {
            tracing::debug!(plugin = %self.plugin.manifest.id(), "ui call dropped: no UI sink");
        }
        ui
    }
}

impl wit::Host for HostState {
    async fn refresh_panel(&mut self, panel: String) {
        if let Some(ui) = self.plugin_ui() {
            ui.refresh_panel(&self.plugin, &self.effective, &panel, Instant::now());
        }
    }

    async fn set_status(&mut self, item: String, text: String) {
        if let Some(ui) = self.plugin_ui() {
            ui.set_status(&self.plugin, &item, &text, Instant::now());
        }
    }

    async fn notify(&mut self, message: String) {
        if let Some(ui) = self.plugin_ui() {
            ui.notify(&self.plugin, &message, Instant::now());
        }
    }
}

#[cfg(test)]
mod tests;
