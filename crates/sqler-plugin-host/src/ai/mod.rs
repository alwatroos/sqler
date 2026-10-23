// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `ai-tools` export: which plugins contribute tools, and one typed `invoke` call.
//!
//! The host does not know the AI tool set (`sqler-ai-tools` is an L3 peer). The caller lends the data service and the
//! approver of the surface it serves in a [`PluginData`] for that one call: the call swaps it into the instance's
//! services, so `query.run` and `catalog.*` of this call go through the surface's own gate, and puts the previous
//! services back when the guest returns. A call that is dropped or times out drops the instance, so the swap cannot
//! outlive the call.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use sqler_plugin_api::{AiTool, CapabilityKind, PluginId, ToolName};

use super::wit_exports::ai_tools::GuestIndices;
use super::{CallFn, Live, PendingCall, PluginHost};
use crate::error::{CallError, ErrorCode, GuestError};
use crate::grant::effective;
use crate::query::PluginData;
use crate::status::{CallKind, PluginState};

#[cfg(test)]
mod tests;

/// Largest `arguments-json` handed to a guest.
pub const MAX_AI_ARGUMENT_BYTES: usize = 1 << 20;
/// Largest JSON text a guest may return.
pub const MAX_AI_RESULT_BYTES: usize = 1 << 20;
/// Longest model-visible tool name.
const MAX_MODEL_NAME_BYTES: usize = 64;

/// A tool a plugin declares, together with the plugin it belongs to.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PluginAiTool {
    /// The plugin that contributes it.
    pub plugin: PluginId,
    /// The plugin's display name.
    pub plugin_name: String,
    /// The manifest entry.
    pub tool: AiTool,
}

impl PluginAiTool {
    /// The name the model sees: `<plugin-id>__<tool-name>`.
    #[must_use]
    pub fn model_name(&self) -> String {
        format!("{}__{}", self.plugin, self.tool.name)
    }
}

fn refused(code: ErrorCode, message: &str) -> CallError {
    CallError::Guest(GuestError {
        code,
        message: message.to_owned(),
    })
}

/// A pending call that is cancelled when the future is dropped before it finishes.
struct CancelOnDrop<R> {
    call: PendingCall<R>,
    done: bool,
}

impl<R> Future for CancelOnDrop<R> {
    type Output = Result<R, CallError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let poll = Pin::new(&mut self.call).poll(cx);
        if poll.is_ready() {
            self.done = true;
        }
        poll
    }
}

impl<R> Drop for CancelOnDrop<R> {
    fn drop(&mut self) {
        if !self.done {
            self.call.cancel();
        }
    }
}

impl PluginHost {
    /// The tools of the registered plugins whose `ai.tool` capability is effective now, in load order. Never compiles
    /// or instantiates anything. A tool whose model name would exceed 64 bytes is left out.
    #[must_use]
    pub fn ai_tools(&self) -> Vec<PluginAiTool> {
        let mut out = Vec::new();
        for status in self.plugins() {
            if matches!(status.state, PluginState::Disabled { .. })
                || !status.effective.allows(CapabilityKind::AiTool)
            {
                continue;
            }
            let Some(entry) = self.inner.entry(&status.id) else {
                continue;
            };
            let manifest = &entry.plugin.manifest;
            for tool in &manifest.contributes().ai_tools {
                let item = PluginAiTool {
                    plugin: status.id.clone(),
                    plugin_name: manifest.name().to_owned(),
                    tool: tool.clone(),
                };
                if item.model_name().len() <= MAX_MODEL_NAME_BYTES {
                    out.push(item);
                }
            }
        }
        out
    }

    /// Runs `ai-tools.invoke` of `plugin` with `arguments_json` and returns the JSON text it produced.
    ///
    /// `data` is the data service and approver for this call only. Dropping the returned future cancels the call.
    ///
    /// # Errors
    /// [`CallError::Guest`] with `InvalidArgument` (arguments over [`MAX_AI_ARGUMENT_BYTES`]), `CapabilityDenied`
    /// (`ai.tool` is not effective; nothing is instantiated), `NotFound` (the manifest declares no such tool) or
    /// `LimitExceeded` (a result over [`MAX_AI_RESULT_BYTES`]); [`CallError::NotFound`] for an unknown plugin;
    /// [`CallError::MissingExport`] when the component has no `ai-tools`; plus every error of a call.
    pub async fn invoke_ai_tool(
        &self,
        plugin: &PluginId,
        tool: &ToolName,
        arguments_json: String,
        data: PluginData,
    ) -> Result<String, CallError> {
        if arguments_json.len() > MAX_AI_ARGUMENT_BYTES {
            return Err(refused(
                ErrorCode::InvalidArgument,
                "the arguments are too large",
            ));
        }
        let entry = self.inner.entry(plugin).ok_or(CallError::NotFound)?;
        let approved = self.inner.opts.grants.approved(&entry.plugin.key);
        let allowed = effective(entry.plugin.manifest.capabilities(), approved.as_ref())
            .allows(CapabilityKind::AiTool);
        if !allowed {
            return Err(refused(
                ErrorCode::CapabilityDenied,
                "the plugin may not provide AI tools",
            ));
        }
        let declared = entry
            .plugin
            .manifest
            .contributes()
            .ai_tools
            .iter()
            .any(|t| &t.name == tool);
        if !declared {
            return Err(refused(ErrorCode::NotFound, "the plugin has no such tool"));
        }
        let name = tool.as_str().to_owned();
        let data = Arc::new(data);
        let f: CallFn<String> = Box::new(move |live: &mut Live| {
            Box::pin(async move {
                let indices = GuestIndices::new(&live.pre).map_err(|_| {
                    wasmtime::Error::new(CallError::MissingExport {
                        interface: "ai-tools",
                    })
                })?;
                let guest = indices.load(&mut live.store, &live.instance)?;
                let saved = live.store.data().services.clone();
                live.store.data_mut().services.insert(data);
                let ran = guest
                    .call_invoke(&mut live.store, &name, &arguments_json)
                    .await;
                live.store.data_mut().services = saved;
                ran?.map_err(|e| wasmtime::Error::new(CallError::Guest(GuestError::from(e))))
            })
        });
        let call = CancelOnDrop {
            call: self.run(plugin, CallKind::AiTool, f),
            done: false,
        };
        let text = call.await?;
        if text.len() > MAX_AI_RESULT_BYTES {
            return Err(refused(
                ErrorCode::LimitExceeded,
                "the tool result is too large",
            ));
        }
        Ok(text)
    }
}
