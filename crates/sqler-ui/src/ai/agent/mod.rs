// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The agent tab (ADR-0051): the state of one ACP agent (Claude Code, Codex, ...) that works on one
//! connection through `SQLer`'s own tools. Plain Rust, no `GPUI`; the tab body lives behind the
//! `gui` feature.
//!
//! The workspace holds the state; a runner task on tokio owns the process, the tool endpoint and
//! the connection source (`runner`). Everything the agent does reaches the tab through one
//! ordered channel of [`AgentMsg`]. Writes need the user: SQL statements go through the approval
//! queue of ADR-0053, the agent's own permission requests through the [`PermissionQueue`].

use std::collections::HashMap;
use std::sync::Arc;

use sqler_acp::{AgentInfo, AgentProfile};
use sqler_ai_tools::ConnectionInfo;
use sqler_config::ConnectionId;

mod endpoint;
mod host;
mod permission;
mod profiles;
mod runner;
mod transcript;

pub use endpoint::{ENDPOINT_STOP_LIMIT, Endpoint, bind_endpoint};
pub use host::{AgentMsg, PermissionAsk, TabAcpHost};
pub use permission::{PendingPermission, PermissionQueue};
pub use profiles::{AGENTS_FILE, AgentProfiles, load_agent_profiles};
pub use runner::{
    AGENT_SHUTDOWN_GRACE, AgentAbort, AgentAbortHandle, AgentControl, AgentFailure, AgentReady,
    AgentStart, AgentStarted, app_launch_options, start_agent,
};
pub use transcript::{
    AgentEntry, Chunked, DROPPED_MARKER, MAX_ENTRIES, MAX_ENTRY_BYTES, STDERR_LINES, ToolEntry,
    Transcript,
};

use super::approval::ApprovalQueue;
use super::indicator::AiStatus;
use super::session::TabSource;
use crate::tabs::TabId;

/// AI is off for the connection.
pub const AGENT_AI_OFF: &str =
    "AI is off for this connection. Turn it on before an agent can use it.";
/// A prompt was sent while the agent works.
pub const AGENT_BUSY: &str = "The agent is still working; wait for it or cancel.";
/// A prompt was sent before the agent was ready.
pub const AGENT_NOT_READY: &str = "The agent is not ready yet.";
/// The agent stopped because AI was turned off for its connection.
pub const AGENT_STOPPED_AI_OFF: &str =
    "The agent was stopped because AI was turned off for this connection.";
/// The agent was stopped because the app quits.
pub const AGENT_STOPPED_QUIT: &str = "The agent was stopped because the app is closing.";
/// Nothing in the profile list has the id of a pick.
pub const AGENT_UNKNOWN: &str = "That agent is not in the list.";

/// Where an agent tab is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPhase {
    /// The user has not picked an agent yet.
    Picking,
    /// The endpoint and the agent are being started.
    Starting,
    /// The agent is idle: a prompt can be sent.
    Ready,
    /// A prompt runs.
    Running {
        /// The turn.
        turn: u64,
    },
    /// The agent ended (it exited, crashed or was stopped); the tab can restart it.
    Stopped(AgentFailure),
    /// The agent could not be started.
    Failed(AgentFailure),
}

/// The state of one agent tab.
pub struct AgentTab {
    pub(crate) conn: ConnectionId,
    pub(crate) info: ConnectionInfo,
    pub(crate) status: Option<AiStatus>,
    pub(crate) phase: AgentPhase,
    pub(crate) profile: Option<AgentProfile>,
    pub(crate) transcript: Transcript,
    pub(crate) draft: String,
    pub(crate) approvals: ApprovalQueue,
    pub(crate) permissions: PermissionQueue,
    pub(crate) control: Option<AgentControl>,
    pub(crate) abort: Option<AgentAbortHandle>,
    pub(crate) source: Option<Arc<TabSource>>,
    pub(crate) agent: Option<AgentInfo>,
    pub(crate) endpoint_url: Option<String>,
    pub(crate) next_turn: u64,
}

// Ids only: never the transcript, the draft or the profile's `env`.
impl std::fmt::Debug for AgentTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTab")
            .field("conn", &self.conn)
            .field("phase", &self.label())
            .field("profile", &self.profile.as_ref().map(|p| p.id.as_str()))
            .field("entries", &self.transcript.entries().len())
            .field("waiting", &(self.approvals.len() + self.permissions.len()))
            .finish_non_exhaustive()
    }
}

impl AgentTab {
    /// A tab that waits for the user to pick an agent.
    #[must_use]
    pub(crate) fn new(conn: ConnectionId, info: ConnectionInfo, status: Option<AiStatus>) -> Self {
        Self {
            conn,
            info,
            status,
            phase: AgentPhase::Picking,
            profile: None,
            transcript: Transcript::default(),
            draft: String::new(),
            approvals: ApprovalQueue::default(),
            permissions: PermissionQueue::default(),
            control: None,
            abort: None,
            source: None,
            agent: None,
            endpoint_url: None,
            next_turn: 0,
        }
    }

    /// The connection the tab works on.
    #[must_use]
    pub fn conn(&self) -> &ConnectionId {
        &self.conn
    }

    /// The connection as the tools see it.
    #[must_use]
    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// What AI does for the connection.
    #[must_use]
    pub fn status(&self) -> Option<&AiStatus> {
        self.status.as_ref()
    }

    /// Where the tab is.
    #[must_use]
    pub fn phase(&self) -> &AgentPhase {
        &self.phase
    }

    /// The agent that was picked.
    #[must_use]
    pub fn profile(&self) -> Option<&AgentProfile> {
        self.profile.as_ref()
    }

    /// The name of the agent: what it called itself, else the name of its profile.
    #[must_use]
    pub fn agent_name(&self) -> Option<&str> {
        self.agent
            .as_ref()
            .and_then(|a| a.name.as_deref())
            .or_else(|| self.profile.as_ref().map(|p| p.name.as_str()))
    }

    /// The conversation.
    #[must_use]
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// The text in the composer.
    #[must_use]
    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// The statements that wait for the user.
    #[must_use]
    pub fn approvals(&self) -> &ApprovalQueue {
        &self.approvals
    }

    /// The permission requests of the agent that wait for the user.
    #[must_use]
    pub fn permissions(&self) -> &PermissionQueue {
        &self.permissions
    }

    /// The URL of the tool endpoint while it runs. It holds no token.
    #[must_use]
    pub fn endpoint_url(&self) -> Option<&str> {
        self.endpoint_url.as_deref()
    }

    /// The process id of the agent while it runs.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.control.as_ref().and_then(AgentControl::pid)
    }

    /// Whether a prompt runs.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.phase, AgentPhase::Running { .. })
    }

    /// Whether the agent process and its endpoint are alive or on their way.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.control.is_some()
    }

    /// Whether the agent is starting or running (not picked yet, stopped or failed).
    #[must_use]
    pub fn is_live_or_starting(&self) -> bool {
        matches!(
            self.phase,
            AgentPhase::Starting | AgentPhase::Ready | AgentPhase::Running { .. }
        )
    }

    /// `Choose an agent`, `Starting…`, `Ready`, what the agent does, `Stopped` or `Failed`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match &self.phase {
            AgentPhase::Picking => "Choose an agent",
            AgentPhase::Starting => "Starting\u{2026}",
            AgentPhase::Ready => "Ready",
            AgentPhase::Running { .. } => {
                if self.approvals.is_waiting() || self.permissions.is_waiting() {
                    "Waiting for your approval"
                } else {
                    "Working\u{2026}"
                }
            }
            AgentPhase::Stopped(_) => "Stopped",
            AgentPhase::Failed(_) => "Failed",
        }
    }
}

/// The agent tabs of a workspace and the profiles they pick from.
#[derive(Default)]
pub struct AgentTabs {
    pub(crate) tabs: HashMap<TabId, AgentTab>,
    pub(crate) profiles: Option<AgentProfiles>,
    pub(crate) loading: bool,
}

impl std::fmt::Debug for AgentTabs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTabs")
            .field("tabs", &self.tabs.len())
            .field("profiles", &self.profiles.as_ref().map(|p| p.list.len()))
            .finish_non_exhaustive()
    }
}

impl AgentTabs {
    /// The agent tab `tab`, if it is one.
    #[must_use]
    pub fn get(&self, tab: TabId) -> Option<&AgentTab> {
        self.tabs.get(&tab)
    }

    /// Whether no agent tab is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Every agent tab.
    pub fn iter(&self) -> impl Iterator<Item = (TabId, &AgentTab)> {
        self.tabs.iter().map(|(id, tab)| (*id, tab))
    }
}
