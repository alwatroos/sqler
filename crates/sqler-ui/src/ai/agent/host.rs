// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What one agent run tells its tab: one ordered channel of [`AgentMsg`].
//!
//! The runner forwards the agent's events, the tool set asks for the approval of a statement, the
//! agent asks for permission, and a prompt ends. The workspace applies each message in order.
//! `Debug` shows the kind of a message and ids only: never text, SQL or a tool title.

use std::future::Future;
use std::pin::Pin;

use sqler_acp::{AcpHost, AgentEvent, PermissionDecision, PermissionRequest, StopReason};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::ai::ApprovalAsk;

/// One message of an agent's host channel.
pub enum AgentMsg {
    /// An event of the agent: an update, a line of stderr, the end of the process.
    Event(AgentEvent),
    /// The tool set asks to run a statement that is not a plain read.
    Ask(ApprovalAsk),
    /// The agent asks the user to allow one of its own tool calls.
    Permission(PermissionAsk),
    /// A prompt ended.
    PromptDone {
        /// The turn the prompt belongs to.
        turn: u64,
        /// How it ended, or why it failed (a user-facing text).
        result: Result<StopReason, String>,
    },
}

impl std::fmt::Debug for AgentMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Event(_) => f.write_str("AgentMsg::Event(..)"),
            Self::Ask(ask) => f.debug_tuple("AgentMsg::Ask").field(ask).finish(),
            Self::Permission(ask) => f.debug_tuple("AgentMsg::Permission").field(ask).finish(),
            Self::PromptDone { turn, result } => f
                .debug_struct("AgentMsg::PromptDone")
                .field("turn", turn)
                .field("ok", &result.is_ok())
                .finish(),
        }
    }
}

/// A permission request of the agent and the way to answer it.
pub struct PermissionAsk {
    /// What the agent wants to do and the choices it offers.
    pub request: PermissionRequest,
    /// Where the answer goes. Dropping it cancels the request.
    pub reply: oneshot::Sender<PermissionDecision>,
}

// Never the tool call: its title and text come from the agent.
impl std::fmt::Debug for PermissionAsk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionAsk")
            .field("options", &self.request.options.len())
            .finish_non_exhaustive()
    }
}

/// The [`AcpHost`] of an agent tab: every permission request becomes a message for the tab. A
/// closed channel or a dropped reply answers `Cancelled`.
pub struct TabAcpHost(pub UnboundedSender<AgentMsg>);

impl std::fmt::Debug for TabAcpHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TabAcpHost")
    }
}

impl AcpHost for TabAcpHost {
    fn request_permission(
        &self,
        req: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send>> {
        let tx = self.0.clone();
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            let ask = PermissionAsk {
                request: req,
                reply,
            };
            if tx.send(AgentMsg::Permission(ask)).is_err() {
                return PermissionDecision::Cancelled;
            }
            answer.await.unwrap_or(PermissionDecision::Cancelled)
        })
    }
}
