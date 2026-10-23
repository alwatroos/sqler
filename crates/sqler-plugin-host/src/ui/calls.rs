// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The typed calls into the `ui-handler` export. Each refuses a contribution the manifest does not declare before
//! any guest code runs; panels also need `ui.panel` in the effective set.

use sqler_plugin_api::{
    CapabilityKind, CapabilitySet, CommandContext, ContributionId, HostInterface, PluginId,
    WidgetError, WidgetEvent, WidgetTree,
};

use super::convert::{context_to_wit, event_to_wit, tree_from_wit};
use super::limits::{STATUS_MAX_BYTES, one_line};
use crate::error::{CallError, ErrorCode, GuestError};
use crate::runtime::ui_handler::GuestIndices;
use crate::runtime::{CallFn, Live, PendingCall, PluginHost};
use crate::status::CallKind;

/// `ui-handler` is not part of every component: report it the way the other per-interface lookups do.
fn missing() -> wasmtime::Error {
    wasmtime::Error::new(CallError::MissingExport {
        interface: "ui-handler",
    })
}

fn guest_error(e: crate::runtime::wit::types::PluginError) -> wasmtime::Error {
    wasmtime::Error::new(CallError::Guest(GuestError::from(e)))
}

/// What the manifest of the plugin declares, looked up by id.
#[derive(Clone, Copy)]
enum Declared {
    Command,
    Panel,
    StatusItem,
}

impl Declared {
    fn what(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Panel => "panel",
            Self::StatusItem => "status item",
        }
    }
}

/// The error for a panel call without `ui.panel`, worded like the API gate's.
fn panel_denied() -> CallError {
    CallError::Guest(GuestError {
        code: ErrorCode::CapabilityDenied,
        message: format!(
            "`{:?}` is not granted to this plugin",
            HostInterface::UiPanel
        ),
    })
}

impl PluginHost {
    /// Checks the declaration (and, for a panel, the grant). `Err` is the call to return.
    fn check_ui(
        &self,
        id: &PluginId,
        what: Declared,
        name: &ContributionId,
    ) -> Result<(), CallError> {
        let Some((plugin, effective)) = self.plugin_view(id) else {
            return Err(CallError::NotFound);
        };
        let c = plugin.manifest.contributes();
        let declared = match what {
            Declared::Command => c.commands.iter().any(|x| x.id == *name),
            Declared::Panel => c.panels.iter().any(|x| x.id == *name),
            Declared::StatusItem => c.status_items.iter().any(|x| x.id == *name),
        };
        if !declared {
            return Err(CallError::Undeclared { what: what.what() });
        }
        if matches!(what, Declared::Panel) && !panel_allowed(&effective) {
            return Err(panel_denied());
        }
        Ok(())
    }

    /// Runs `ui-handler.run-command` for a declared command. A guest error is [`CallError::Guest`].
    pub fn run_command(
        &self,
        id: &PluginId,
        command: &ContributionId,
        context: CommandContext,
    ) -> PendingCall<()> {
        if let Err(e) = self.check_ui(id, Declared::Command, command) {
            return PendingCall::failed(e);
        }
        let Some(context) = context_to_wit(context) else {
            return PendingCall::failed(CallError::Guest(GuestError {
                code: ErrorCode::InvalidArgument,
                message: "the command context cannot be shown to plugins".to_owned(),
            }));
        };
        let command = command.as_str().to_owned();
        let f: CallFn<()> = Box::new(move |live: &mut Live| {
            Box::pin(async move {
                let indices = GuestIndices::new(&live.pre).map_err(|_| missing())?;
                let guest = indices.load(&mut live.store, &live.instance)?;
                guest
                    .call_run_command(&mut live.store, &command, &context)
                    .await?
                    .map_err(guest_error)
            })
        });
        self.run(id, CallKind::Command, f)
    }

    /// Runs `ui-handler.render-panel` for a declared panel. A tree that fails validation is an `Err(WidgetError)`
    /// value, not a fault.
    pub fn render_panel(
        &self,
        id: &PluginId,
        panel: &ContributionId,
    ) -> PendingCall<Result<WidgetTree, WidgetError>> {
        if let Err(e) = self.check_ui(id, Declared::Panel, panel) {
            return PendingCall::failed(e);
        }
        let panel = panel.as_str().to_owned();
        let f: CallFn<Result<WidgetTree, WidgetError>> = Box::new(move |live: &mut Live| {
            Box::pin(async move {
                let indices = GuestIndices::new(&live.pre).map_err(|_| missing())?;
                let guest = indices.load(&mut live.store, &live.instance)?;
                let tree = guest.call_render_panel(&mut live.store, &panel).await?;
                Ok(tree_from_wit(tree))
            })
        });
        self.run(id, CallKind::Render, f)
    }

    /// Runs `ui-handler.panel-event`. `Some` is the tree that replaces the panel (or its validation error), `None`
    /// means the guest left the panel as it is.
    pub fn panel_event(
        &self,
        id: &PluginId,
        panel: &ContributionId,
        event: WidgetEvent,
    ) -> PendingCall<Option<Result<WidgetTree, WidgetError>>> {
        if let Err(e) = self.check_ui(id, Declared::Panel, panel) {
            return PendingCall::failed(e);
        }
        let panel = panel.as_str().to_owned();
        let event = event_to_wit(event);
        let f: CallFn<Option<Result<WidgetTree, WidgetError>>> =
            Box::new(move |live: &mut Live| {
                Box::pin(async move {
                    let indices = GuestIndices::new(&live.pre).map_err(|_| missing())?;
                    let guest = indices.load(&mut live.store, &live.instance)?;
                    let tree = guest
                        .call_panel_event(&mut live.store, &panel, &event)
                        .await?;
                    Ok(tree.map(tree_from_wit))
                })
            });
        self.run(id, CallKind::Command, f)
    }

    /// Runs `ui-handler.status` for a declared status item.
    pub fn status(&self, id: &PluginId, item: &ContributionId) -> PendingCall<Option<String>> {
        if let Err(e) = self.check_ui(id, Declared::StatusItem, item) {
            return PendingCall::failed(e);
        }
        let item = item.as_str().to_owned();
        let f: CallFn<Option<String>> = Box::new(move |live: &mut Live| {
            Box::pin(async move {
                let indices = GuestIndices::new(&live.pre).map_err(|_| missing())?;
                let guest = indices.load(&mut live.store, &live.instance)?;
                let text = guest.call_status(&mut live.store, &item).await?;
                // Plugin text is one plain line, like the text it pushes with `set-status`.
                Ok(text
                    .map(|t| one_line(&t, STATUS_MAX_BYTES))
                    .filter(|t| !t.is_empty()))
            })
        });
        self.run(id, CallKind::Status, f)
    }
}

fn panel_allowed(effective: &CapabilitySet) -> bool {
    effective.allows(CapabilityKind::UiPanel)
}
