// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The exploration session engine (ADR-0039): one chat with a model about one connection.
//!
//! An [`ExplorationSession`] is bound to a connection, a provider and a
//! [`ToolSet`](crate::ToolSet). [`ExplorationSession::send`] runs one user turn as a loop: a model step
//! streams through the provider, each tool call the model made runs through
//! [`ToolSet::call_detailed`](crate::ToolSet::call_detailed) in order, and the results go back to
//! the model. The loop ends when the model ends its turn, or when a limit, loop detection,
//! cancellation or a provider error stops it.
//!
//! # Guards
//!
//! Redaction, the untrusted-data envelope, the statement guard, approvals and the audit log
//! stay in the tool set (ADR-0036, ADR-0037). The session adds a wrapper around the host's
//! [`Approver`](crate::Approver) that only reports waits to the UI; it never decides. With no
//! approver, every statement that is not a plain read is refused. The session never builds a
//! tool result: what the model sees is exactly the result the tool set returned. A call the
//! session itself refuses (a limit, a repeat, a cancel) is shown in the transcript as "not
//! run" and left out of the history sent to the model.
//!
//! # Limits
//!
//! [`SessionLimits`]: model steps and tool calls per user turn, a token budget for the whole
//! session, a wall-clock limit per turn (approval waits do not count), a repeat limit for
//! identical calls, and a [`RetryPolicy`] for `RateLimited`, `Overloaded` and `Network` errors that arrive
//! before the model produced anything.
//!
//! # Cancellation
//!
//! [`SessionCancel`] (from [`ExplorationSession::cancel_handle`]) is raced at every await.
//! Dropping the `send` future cancels the turn too.
//!
//! # Secrets and persistence
//!
//! The [`Transcript`] is built from redacted data and lives in memory only, as long as the
//! session object. The session writes nothing to disk and holds no keys.

mod approval;
mod cancel;
mod config;
mod engine;
mod events;
mod prompt;
mod repeat;
mod transcript;

pub use cancel::SessionCancel;
pub use config::{RetryPolicy, SessionConfig, SessionLimits};
pub use engine::{ExplorationSession, LimitKind, SessionError, TurnOutcome};
pub use events::{MemoryEvents, SessionEvent, SessionEvents, SessionState};
pub use prompt::{
    CHAT_PROMPT_VERSION, PromptKind, SESSION_PROMPT_VERSION, chat_system_prompt, prompt_for,
    system_prompt,
};
pub use transcript::{
    MAX_TRANSCRIPT_ENTRIES, NotRunReason, Notice, RerunRequest, SqlEntry, ToolCallEntry,
    ToolCallStatus, Transcript, TranscriptEntry,
};
