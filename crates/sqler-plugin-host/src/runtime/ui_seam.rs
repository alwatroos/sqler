// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A narrow, hidden way to call the `ui-handler` export, so the end-to-end guest tests (`tests/guests.rs`) can drive
//! a real component before P8 owns this interface. P8 replaces both methods with its own typed calls.

use sqler_plugin_api::PluginId;

use super::bindings::exports::sqler::plugin::ui_handler::{CommandContext, GuestIndices};
use super::call::{CallFn, PendingCall};
use super::host::PluginHost;
use super::instance::Live;
use crate::error::{CallError, GuestError};
use crate::status::CallKind;

/// `ui-handler` is not part of every component: report it the way the other per-interface lookups do.
fn missing() -> wasmtime::Error {
    wasmtime::Error::new(CallError::MissingExport {
        interface: "ui-handler",
    })
}

impl PluginHost {
    /// Runs `ui-handler.run-command` with no command context. A guest error is [`CallError::Guest`].
    #[doc(hidden)]
    pub fn call_command(&self, id: &PluginId, command: &str) -> PendingCall<()> {
        let command = command.to_owned();
        let f: CallFn<()> = Box::new(move |live: &mut Live| {
            Box::pin(async move {
                let indices = GuestIndices::new(&live.pre).map_err(|_| missing())?;
                let guest = indices.load(&mut live.store, &live.instance)?;
                guest
                    .call_run_command(&mut live.store, &command, &CommandContext::None)
                    .await?
                    .map_err(|e| wasmtime::Error::new(CallError::Guest(GuestError::from(e))))
            })
        });
        self.run(id, CallKind::Command, f)
    }

    /// Runs `ui-handler.status` for a declared status item.
    #[doc(hidden)]
    pub fn call_status(&self, id: &PluginId, item: &str) -> PendingCall<Option<String>> {
        let item = item.to_owned();
        let f: CallFn<Option<String>> = Box::new(move |live: &mut Live| {
            Box::pin(async move {
                let indices = GuestIndices::new(&live.pre).map_err(|_| missing())?;
                let guest = indices.load(&mut live.store, &live.instance)?;
                guest.call_status(&mut live.store, &item).await
            })
        });
        self.run(id, CallKind::Status, f)
    }
}
