// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ui`: void calls the host drops until the UI sink exists (P8 replaces this file).

use crate::runtime::bindings::sqler::plugin::ui as wit;
use crate::runtime::state::HostState;

impl wit::Host for HostState {
    async fn refresh_panel(&mut self, panel: String) {
        tracing::debug!(plugin = %self.plugin.manifest.id, %panel, "ui.refresh-panel dropped: no UI sink");
    }

    async fn set_status(&mut self, item: String, _text: String) {
        tracing::debug!(plugin = %self.plugin.manifest.id, %item, "ui.set-status dropped: no UI sink");
    }

    async fn notify(&mut self, _message: String) {
        tracing::debug!(plugin = %self.plugin.manifest.id, "ui.notify dropped: no UI sink");
    }
}
