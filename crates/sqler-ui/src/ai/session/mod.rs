// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI session tab (ADR-0053): the state of one chat with a model about one connection, and the
//! texts and labels the view shows. Plain Rust, no `GPUI`; the tab body lives behind the `gui`
//! feature.
//!
//! The idle `ExplorationSession` sits in the tab. A turn lends it to a job on tokio and it comes
//! back through the host channel ([`HostMsg`]); the workspace applies each message in order.

use std::collections::HashMap;

use sqler_ai_tools::session::{
    ExplorationSession, LimitKind, NotRunReason, Notice, SessionCancel, SessionState, SqlEntry,
    ToolCallStatus, TranscriptEntry,
};
use sqler_ai_tools::{ConnectionInfo, Truncation};
use sqler_config::ConnectionId;
use tokio::sync::mpsc::UnboundedSender;

mod host;
mod job;
mod source;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use host::{ChannelEvents, ChannelUiHost, HostMsg, UiCall, UiRequest};
pub use job::{AiSessionStart, AiTurnJob, SessionReady, TurnDone, build_session};
pub use source::TabSource;

use super::approval::ApprovalQueue;
use super::indicator::AiStatus;
use crate::tabs::TabId;
use crate::workspace::RequestId;

/// AI is off for the connection.
pub const SESSION_AI_OFF: &str =
    "AI is off for this connection. Turn it on in the AI view to start a session.";
/// There is no connection to open a session on.
pub const SESSION_NO_CONNECTION: &str = "Open or select a connection first.";
/// `ai.toml` could not be read.
pub const SESSION_CONFIG_BROKEN: &str = "ai.toml could not be read; fix it to use AI.";
/// The provider kind is not compiled into this build.
pub const SESSION_NOT_BUILT: &str = "This AI provider is not available in this build.";
/// A message was sent before the session was built.
pub const SESSION_STARTING: &str = "The session is still starting.";
/// A message was sent while the model works.
pub const SESSION_BUSY: &str = "The model is still working; wait for it or cancel.";
/// An empty message.
pub const SESSION_EMPTY: &str = "Type a message first.";
/// The model cannot call tools.
pub const NO_TOOLS: &str = "This model cannot call tools, so it cannot explore a database. Pick another Exploration model in Settings > AI.";
/// The answer of a UI tool whose tab is gone.
pub const TAB_CLOSED: &str = "the AI session tab was closed";
/// A UI tool or a re-run named another connection than the tab's.
pub const WRONG_CONNECTION: &str = "this session can only use its own connection";

/// An approve or deny named a statement that no longer waits; nothing was answered.
pub const STALE_ANSWER: &str =
    "That statement is not the one waiting any more, so nothing was approved or denied.";
/// A statement of the transcript is no longer kept.
pub const ENTRY_GONE: &str = "That statement is no longer in the transcript.";
/// How many transcript entries a tab keeps. The older ones are dropped and the view says so.
pub const MAX_ENTRIES: usize = 500;
/// The line that stands for the dropped entries.
pub const DROPPED_MARKER: &str = "Older entries were dropped to keep this session light.";

/// Where a session tab is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPhase {
    /// The session is being built.
    Starting,
    /// The session is idle: a message can be sent.
    Ready,
    /// A turn runs; the session is on loan to it.
    Running {
        /// The turn.
        turn: u64,
    },
    /// The session could not be built; the text says why.
    Failed(String),
}

/// The state of one AI session tab.
pub struct AiSessionTab {
    pub(crate) conn: ConnectionId,
    pub(crate) info: ConnectionInfo,
    pub(crate) status: Option<AiStatus>,
    pub(crate) phase: SessionPhase,
    pub(crate) entries: Vec<TranscriptEntry>,
    /// How many entries before `entries[0]` were dropped. `dropped + i` is the stable number of
    /// `entries[i]`: the position in the engine transcript, which only grows.
    pub(crate) dropped: usize,
    pub(crate) streaming: String,
    pub(crate) state: SessionState,
    pub(crate) draft: String,
    pub(crate) approvals: ApprovalQueue,
    pub(crate) session: Option<Box<ExplorationSession>>,
    pub(crate) cancel: Option<SessionCancel>,
    pub(crate) source: Option<std::sync::Arc<TabSource>>,
    pub(crate) tx: UnboundedSender<HostMsg>,
    pub(crate) next_turn: u64,
}

// Never the transcript or the draft.
impl std::fmt::Debug for AiSessionTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiSessionTab")
            .field("conn", &self.conn)
            .field("phase", &self.label())
            .field("entries", &self.entries.len())
            .field("waiting", &self.approvals.len())
            .finish_non_exhaustive()
    }
}

impl AiSessionTab {
    /// A tab whose session is being built.
    #[must_use]
    pub(crate) fn new(
        conn: ConnectionId,
        info: ConnectionInfo,
        status: Option<AiStatus>,
        tx: UnboundedSender<HostMsg>,
    ) -> Self {
        Self {
            conn,
            info,
            status,
            phase: SessionPhase::Starting,
            entries: Vec::new(),
            dropped: 0,
            streaming: String::new(),
            state: SessionState::Idle,
            draft: String::new(),
            approvals: ApprovalQueue::default(),
            session: None,
            cancel: None,
            source: None,
            tx,
            next_turn: 0,
        }
    }

    /// The saved connection the session explores.
    #[must_use]
    pub fn conn(&self) -> &ConnectionId {
        &self.conn
    }

    /// The connection as the tools see it.
    #[must_use]
    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// What AI does for the connection (the provider chip).
    #[must_use]
    pub fn status(&self) -> Option<&AiStatus> {
        self.status.as_ref()
    }

    /// Where the tab is.
    #[must_use]
    pub fn phase(&self) -> &SessionPhase {
        &self.phase
    }

    /// The transcript so far, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// How many older entries the tab no longer keeps.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// The entry with the stable number `no`, if it is still kept.
    #[must_use]
    pub fn entry(&self, no: usize) -> Option<&TranscriptEntry> {
        self.entries.get(no.checked_sub(self.dropped)?)
    }

    /// Adds one entry; the oldest go when there are more than [`MAX_ENTRIES`].
    pub(crate) fn push_entry(&mut self, entry: TranscriptEntry) {
        self.entries.push(entry);
        self.trim();
    }

    /// Replaces the entries by the whole engine transcript.
    pub(crate) fn set_entries(&mut self, all: &[TranscriptEntry]) {
        self.entries = all.to_vec();
        self.dropped = 0;
        self.trim();
    }

    fn trim(&mut self) {
        let extra = self.entries.len().saturating_sub(MAX_ENTRIES);
        if extra > 0 {
            self.entries.drain(..extra);
            self.dropped += extra;
        }
    }

    /// The answer the model is streaming now.
    #[must_use]
    pub fn streaming(&self) -> &str {
        &self.streaming
    }

    /// The engine state.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
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

    /// Whether a turn runs.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.phase, SessionPhase::Running { .. })
    }

    /// The short state for the hub: `Starting…`, `Ready`, what the model does, or `Failed`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match &self.phase {
            SessionPhase::Starting => "Starting\u{2026}",
            SessionPhase::Ready => "Ready",
            SessionPhase::Failed(_) => "Failed",
            SessionPhase::Running { .. } => match state_label(self.state) {
                "" => state_label(SessionState::Streaming),
                text => text,
            },
        }
    }

    /// The handle that cancels the statement in flight, if there is one.
    #[must_use]
    pub(crate) fn in_flight(&self) -> Option<sqler_driver_api::CancelHandle> {
        self.source.as_ref().and_then(|s| s.cancel_in_flight())
    }
}

/// The AI session tabs of a workspace.
#[derive(Default)]
pub struct AiSessions {
    pub(crate) tabs: HashMap<TabId, AiSessionTab>,
    /// Statements to run once the editor tab that a re-run opens is connected.
    pub(crate) reruns: HashMap<RequestId, String>,
}

impl std::fmt::Debug for AiSessions {
    /// Counts only: the re-runs hold the exact SQL of a statement.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiSessions")
            .field("tabs", &self.tabs.len())
            .field("reruns", &self.reruns.len())
            .finish()
    }
}

impl AiSessions {
    /// The session tab, if `tab` is one.
    #[must_use]
    pub fn get(&self, tab: TabId) -> Option<&AiSessionTab> {
        self.tabs.get(&tab)
    }

    /// Whether any AI session tab is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// The tabs, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (TabId, &AiSessionTab)> {
        self.tabs.iter().map(|(id, tab)| (*id, tab))
    }
}

/// What the state line says while the model works; empty when it does not.
#[must_use]
pub fn state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Streaming => "Thinking\u{2026}",
        SessionState::ToolCallPending => "Running a tool\u{2026}",
        SessionState::AwaitingApproval => "Waiting for your approval",
        _ => "",
    }
}

/// How a tool call ended, for people.
#[must_use]
pub fn tool_status_text(status: &ToolCallStatus) -> String {
    match status {
        ToolCallStatus::Ok => "done".to_owned(),
        ToolCallStatus::Error { code, .. } => code.clone(),
        ToolCallStatus::NotRun(reason) => {
            let why = match reason {
                NotRunReason::Limit(_) => "limit reached",
                NotRunReason::Repeated => "repeated call",
                _ => "cancelled",
            };
            format!("not run ({why})")
        }
        ToolCallStatus::Cancelled => "cancelled, may have run".to_owned(),
        _ => "unknown".to_owned(),
    }
}

/// The line under a statement: `3 rows · row limit · approved`, or `failed`.
#[must_use]
pub fn sql_footer(entry: &SqlEntry) -> String {
    if entry.failed {
        return "failed".to_owned();
    }
    let mut parts = vec![if entry.rows == 1 {
        "1 row".to_owned()
    } else {
        format!("{} rows", entry.rows)
    }];
    match entry.truncated {
        Some(Truncation::RowLimit) => parts.push("row limit".to_owned()),
        Some(Truncation::TimeLimit) => parts.push("time limit".to_owned()),
        _ => {}
    }
    if entry.approved {
        parts.push("approved".to_owned());
    }
    parts.join(" \u{b7} ")
}

/// A line the engine added, for people.
#[must_use]
pub fn notice_text(notice: &Notice) -> String {
    match notice {
        Notice::Retrying { attempt, delay } => {
            format!("Retrying (attempt {attempt}) in {} s", delay.as_secs())
        }
        Notice::LimitReached(kind) => {
            let what = match kind {
                LimitKind::ModelSteps => "model steps",
                LimitKind::ToolCalls => "tool calls",
                LimitKind::Tokens => "tokens",
                _ => "time",
            };
            format!("Stopped: the limit of {what} for this turn was reached.")
        }
        Notice::LoopDetected { tool } => {
            format!("Stopped: the model kept repeating {tool}.")
        }
        Notice::Cancelled => "Cancelled.".to_owned(),
        Notice::ProviderError { message } => format!("The AI provider failed: {message}"),
        Notice::OutputTruncated => "The answer was cut at the output limit.".to_owned(),
        _ => "Notice.".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_labels() {
        assert_eq!(state_label(SessionState::Streaming), "Thinking\u{2026}");
        assert_eq!(
            state_label(SessionState::ToolCallPending),
            "Running a tool\u{2026}"
        );
        assert_eq!(
            state_label(SessionState::AwaitingApproval),
            "Waiting for your approval"
        );
        assert_eq!(state_label(SessionState::Idle), "");
        assert_eq!(state_label(SessionState::Done), "");
    }

    #[test]
    fn tool_status_texts() {
        assert_eq!(tool_status_text(&ToolCallStatus::Ok), "done");
        assert_eq!(
            tool_status_text(&ToolCallStatus::Error {
                code: "denied".into(),
                message: "m".into()
            }),
            "denied"
        );
        assert_eq!(
            tool_status_text(&ToolCallStatus::NotRun(NotRunReason::Repeated)),
            "not run (repeated call)"
        );
        assert_eq!(
            tool_status_text(&ToolCallStatus::NotRun(NotRunReason::Limit(
                LimitKind::ToolCalls
            ))),
            "not run (limit reached)"
        );
        assert_eq!(
            tool_status_text(&ToolCallStatus::NotRun(NotRunReason::Cancelled)),
            "not run (cancelled)"
        );
        assert_eq!(
            tool_status_text(&ToolCallStatus::Cancelled),
            "cancelled, may have run"
        );
    }

    #[test]
    fn notice_texts() {
        assert_eq!(notice_text(&Notice::Cancelled), "Cancelled.");
        assert!(notice_text(&Notice::LimitReached(LimitKind::Tokens)).contains("tokens"));
        assert!(
            notice_text(&Notice::LoopDetected {
                tool: "run_query".into()
            })
            .contains("run_query")
        );
        assert!(
            notice_text(&Notice::Retrying {
                attempt: 2,
                delay: std::time::Duration::from_secs(3)
            })
            .contains("attempt 2")
        );
        assert!(
            notice_text(&Notice::ProviderError {
                message: "boom".into()
            })
            .contains("boom")
        );
        assert_ne!(notice_text(&Notice::OutputTruncated), "");
    }
}
