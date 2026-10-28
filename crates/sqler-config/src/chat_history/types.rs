// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plain records of the chat history store and its limits.
//!
//! `sqler-config` is a low layer and cannot see the AI session types, so a transcript entry is
//! stored as a [`StoredEntry`]. There is no field for the exact SQL a statement ran with: only
//! the credential-redacted text to show. A new entry kind needs a schema version bump.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The schema version of `chat-history.db` (`PRAGMA user_version`).
pub const CHAT_HISTORY_SCHEMA_VERSION: u32 = 1;
/// The most conversations kept; the oldest by last update go first.
pub const MAX_STORED_CONVERSATIONS: usize = 100;
/// The most payload bytes kept over all conversations; the oldest conversations go first.
pub const MAX_STORED_HISTORY_BYTES: u64 = 32 << 20;
/// The most rows kept in one stored result.
pub const MAX_STORED_ROWS: usize = 50;
/// The most bytes of one stored entry.
pub const MAX_STORED_ENTRY_BYTES: usize = 128 << 10;
/// The most bytes of one stored text.
pub const MAX_STORED_TEXT_BYTES: usize = 64 << 10;
/// The most characters of a conversation title.
pub const MAX_STORED_TITLE_CHARS: usize = 80;

/// The id of a stored conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConversationId(pub(crate) String);

impl ConversationId {
    /// An id from text (for previews and tests).
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

/// One line of the conversation list.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConversationSummary {
    /// The conversation.
    pub id: ConversationId,
    /// The id of the connection the chat is about.
    pub connection: String,
    /// The first user message, scrubbed and cut; empty until there is one.
    pub title: String,
    /// When the first entry was stored (Unix milliseconds).
    pub created_ms: i64,
    /// When the last entry was stored (Unix milliseconds).
    pub updated_ms: i64,
    /// How many entries are stored.
    pub entries: u64,
}

impl ConversationSummary {
    /// A list line built by hand, for previews and tests of code that shows the list.
    #[must_use]
    pub fn new(
        id: ConversationId,
        connection: impl Into<String>,
        title: impl Into<String>,
        created_ms: i64,
        updated_ms: i64,
        entries: u64,
    ) -> Self {
        Self {
            id,
            connection: connection.into(),
            title: title.into(),
            created_ms,
            updated_ms,
            entries,
        }
    }
}

/// A conversation with its entries.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct StoredConversation {
    /// The list line.
    pub summary: ConversationSummary,
    /// The stable number of `entries[0]`; `entries[i]` has number `first_seq + i`.
    pub first_seq: u64,
    /// The entries, oldest first.
    pub entries: Vec<StoredEntry>,
}

/// How a stored tool call ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StoredStatus {
    /// The tool returned a result.
    Ok,
    /// The tool returned an error result.
    Error {
        /// The stable error code.
        code: String,
        /// The redacted message.
        message: String,
    },
    /// The call did not run. The reason is `limit:model_steps`, `limit:tool_calls`,
    /// `limit:tokens`, `limit:time`, `repeated` or `cancelled`.
    NotRun {
        /// Why.
        reason: String,
    },
    /// The turn was cancelled while the call was in flight.
    Cancelled,
}

/// A stored engine line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StoredNotice {
    /// A model step was retried.
    Retrying {
        /// The retry number.
        attempt: u32,
        /// The wait before it, in milliseconds.
        delay_ms: u64,
    },
    /// A limit stopped the turn (`model_steps`, `tool_calls`, `tokens` or `time`).
    LimitReached {
        /// Which limit.
        limit: String,
    },
    /// The model repeated a call.
    LoopDetected {
        /// The tool.
        tool: String,
    },
    /// The turn was cancelled.
    Cancelled,
    /// The provider failed.
    ProviderError {
        /// The provider error text.
        message: String,
    },
    /// The model stopped at its output cap.
    OutputTruncated,
}

/// One stored transcript entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StoredEntry {
    /// What the user typed.
    User {
        /// The text.
        text: String,
    },
    /// What the model answered.
    Assistant {
        /// The text.
        text: String,
    },
    /// A tool call.
    ToolCall {
        /// The call id.
        call_id: String,
        /// The tool name.
        tool: String,
        /// The redacted arguments.
        arguments: Value,
        /// How it ended.
        status: StoredStatus,
        /// The result content.
        result: Value,
    },
    /// A statement that ran. The exact SQL is never stored.
    Sql {
        /// The call that ran it.
        call_id: String,
        /// The id of the connection.
        connection: String,
        /// The credential-redacted SQL to show.
        display_sql: String,
        /// The statement class name.
        class: String,
        /// Whether the user approved it.
        approved: bool,
        /// Rows kept.
        rows: u64,
        /// Why the rows were cut, if they were.
        truncated: Option<String>,
        /// Whether the database reported an error.
        failed: bool,
    },
    /// An engine line.
    Notice {
        /// The line.
        notice: StoredNotice,
    },
}

/// Whether the store works.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChatHistoryStatus {
    /// The worker is still opening the file.
    Opening,
    /// The store works.
    Ready,
    /// The store is off, for the reason given. The text never echoes file content.
    Off(String),
}

/// Why a read failed. The texts are fixed and never carry file content.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ChatHistoryError {
    /// The store is off, for the reason given.
    #[error("{0}")]
    Off(String),
    /// The worker did not answer in time.
    #[error("the chat history did not answer in time")]
    Timeout,
    /// The database reported an error.
    #[error("{0}")]
    Database(&'static str),
}

/// Tuning of a [`ChatHistory`](crate::ChatHistory), for tests. The defaults are the contract.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ChatHistoryOptions {
    /// The longest a write waits before it is committed (default 1 s).
    pub flush_interval: Duration,
    /// The most conversations kept.
    pub max_conversations: usize,
    /// The most payload bytes kept.
    pub max_bytes: u64,
    /// The most rows of one stored result.
    pub max_rows: usize,
    /// The most bytes of one entry.
    pub max_entry_bytes: usize,
    /// The most bytes of one text.
    pub max_text_bytes: usize,
}

impl Default for ChatHistoryOptions {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(1),
            max_conversations: MAX_STORED_CONVERSATIONS,
            max_bytes: MAX_STORED_HISTORY_BYTES,
            max_rows: MAX_STORED_ROWS,
            max_entry_bytes: MAX_STORED_ENTRY_BYTES,
            max_text_bytes: MAX_STORED_TEXT_BYTES,
        }
    }
}
