// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The typed `completion.complete` call (P11). It refuses a plugin that does not declare `[completion]` before any
//! guest code runs, and cleans and caps what the guest returns. The call kind is [`CallKind::Completion`], so the
//! deadline is the host's completion deadline and a timeout or a trap counts toward the fault rule.

mod convert;

use sqler_plugin_api::{CompletionItem, CompletionRequest, PluginId};

use self::convert::{items_from_wit, request_to_wit};
use crate::error::CallError;
use crate::runtime::wit_exports::completion::GuestIndices;
use crate::runtime::{CallFn, Live, PendingCall, PluginHost};
use crate::status::{CallKind, PluginState};

impl PluginHost {
    /// The plugins that can complete for `driver`, in load order: registered, not disabled, with a component, a
    /// `[completion]` block, and a driver list that is empty or names `driver`. Without a driver only plugins with
    /// an empty list match.
    #[must_use]
    pub fn completion_providers(&self, driver: Option<&str>) -> Vec<PluginId> {
        self.plugins()
            .into_iter()
            .filter(|s| !matches!(s.state, PluginState::Disabled { .. }))
            .filter(|s| {
                let Some((plugin, _)) = self.plugin_view(&s.id) else {
                    return false;
                };
                if plugin.component.is_none() {
                    return false;
                }
                let Some(c) = &plugin.manifest.contributes().completion else {
                    return false;
                };
                c.drivers.is_empty()
                    || driver.is_some_and(|d| c.drivers.iter().any(|e| e.as_str() == d))
            })
            .map(|s| s.id)
            .collect()
    }

    /// Runs `completion.complete`. The request is checked (statement size, cursor on a char boundary) before guest
    /// code runs; the items come back cleaned and capped.
    pub fn complete(
        &self,
        id: &PluginId,
        request: CompletionRequest,
    ) -> PendingCall<Vec<CompletionItem>> {
        let declared = match self.plugin_view(id) {
            None => return PendingCall::failed(CallError::NotFound),
            Some((plugin, _)) => plugin.manifest.contributes().completion.is_some(),
        };
        if !declared {
            return PendingCall::failed(CallError::Undeclared { what: "completion" });
        }
        let request = match request_to_wit(request) {
            Ok(r) => r,
            Err(e) => return PendingCall::failed(e),
        };
        let f: CallFn<Vec<CompletionItem>> = Box::new(move |live: &mut Live| {
            Box::pin(async move {
                let indices = GuestIndices::new(&live.pre).map_err(|_| {
                    wasmtime::Error::new(CallError::MissingExport {
                        interface: "completion",
                    })
                })?;
                let guest = indices.load(&mut live.store, &live.instance)?;
                let items = guest.call_complete(&mut live.store, &request).await?;
                Ok(items_from_wit(items))
            })
        });
        self.run(id, CallKind::Completion, f)
    }
}
