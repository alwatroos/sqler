// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SQLer`'s own view of `ACP` data: sessions, updates, permission requests and exit information.
//!
//! These types wrap the protocol crate's types, so a protocol bump changes only `connection.rs`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// Identifier of an agent session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub(crate) String);

impl SessionId {
    /// Wrap an id received from an agent (for example one stored by the host).
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Status of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
    /// Not started.
    Pending,
    /// Running.
    InProgress,
    /// Finished.
    Completed,
    /// Failed.
    Failed,
}

/// A tool call (or a change to one) as the agent reports it. Absent fields were not part of the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallInfo {
    /// Id of the call, stable across its updates.
    pub id: String,
    /// Human readable title.
    pub title: Option<String>,
    /// Tool kind (`read`, `edit`, `execute`, ...).
    pub kind: Option<String>,
    /// Current status.
    pub status: Option<ToolCallStatus>,
    /// Text content attached to the call.
    pub text: Vec<String>,
}

/// One entry of an agent plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    /// What the step does.
    pub content: String,
    /// `high`, `medium` or `low`.
    pub priority: String,
    /// `pending`, `in_progress` or `completed`.
    pub status: String,
}

/// A streamed update of a session. Only text content is carried.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionUpdate {
    /// A chunk of the agent's answer.
    AgentMessage(String),
    /// A chunk of the agent's reasoning.
    AgentThought(String),
    /// A chunk of the user message (replayed by the agent).
    UserMessage(String),
    /// A tool call started or changed.
    ToolCall(ToolCallInfo),
    /// The agent's plan.
    Plan(Vec<PlanEntry>),
    /// An update `SQLer` does not render (the name says which kind).
    Other(&'static str),
}

/// Why a prompt turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The agent finished its answer.
    EndTurn,
    /// The token limit was reached.
    MaxTokens,
    /// The agent hit its turn limit.
    MaxTurnRequests,
    /// The agent refused to continue.
    Refusal,
    /// The prompt was cancelled.
    Cancelled,
}

/// What the agent told `SQLer` about itself during `initialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    /// Negotiated `ACP` protocol version.
    pub protocol_version: u16,
    /// Agent name, when it sent one.
    pub name: Option<String>,
    /// Agent version, when it sent one.
    pub version: Option<String>,
    /// The agent accepts `MCP` servers over HTTP.
    pub mcp_http: bool,
    /// The agent accepts `MCP` servers over SSE.
    pub mcp_sse: bool,
}

/// How an agent process ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitInfo {
    /// Exit code, when the process exited normally.
    pub code: Option<i32>,
    /// Terminating signal (Unix), when it was killed by one.
    pub signal: Option<i32>,
    /// The last lines the agent wrote to stderr (at most 64).
    pub stderr_tail: Vec<String>,
}

impl fmt::Display for ExitInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.code, self.signal) {
            (Some(code), _) => write!(f, "exit code {code}"),
            (None, Some(signal)) => write!(f, "signal {signal}"),
            (None, None) => f.write_str("unknown status"),
        }
    }
}

/// Kind of a permission option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOptionKind {
    /// Allow this once.
    AllowOnce,
    /// Allow now and in the future.
    AllowAlways,
    /// Reject this once.
    RejectOnce,
    /// Reject now and in the future.
    RejectAlways,
}

/// One choice offered by a permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    /// Id to answer with.
    pub id: String,
    /// Label for the user.
    pub name: String,
    /// What the option does.
    pub kind: PermissionOptionKind,
}

/// The agent asks the user to approve a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    /// Session the call belongs to.
    pub session_id: SessionId,
    /// The call to approve.
    pub tool_call: ToolCallInfo,
    /// The choices.
    pub options: Vec<PermissionOption>,
}

/// The host's answer to a [`PermissionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// The option with this id. An id that was not offered is sent as `Cancelled`.
    Selected(String),
    /// No choice was made.
    Cancelled,
}

/// The party that answers permission requests (the Agent tab in `SQLer`).
pub trait AcpHost: Send + Sync + 'static {
    /// Decide on a permission request. The future may take as long as the user needs; a `cancel` of the
    /// session drops it and answers the agent with `Cancelled`.
    fn request_permission(
        &self,
        req: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send>>;
}

/// An event from a running agent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentEvent {
    /// A streamed session update.
    Update {
        /// The session.
        session_id: SessionId,
        /// The update.
        update: SessionUpdate,
    },
    /// A line the agent wrote to stderr.
    Stderr(String),
    /// The process ended (sent once, last).
    Exited(ExitInfo),
}
