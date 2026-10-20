// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! History value types.

use std::fmt;
use std::time::{Duration, SystemTime};

use crate::ConnectionId;

/// Version written to, and the highest read from, `history.jsonl`.
pub const HISTORY_FORMAT_VERSION: u32 = 1;
/// The default number of entries kept (see [`HistoryOptions::max_entries`]).
pub const DEFAULT_HISTORY_CAP: usize = 10_000;
/// The longest SQL text stored per entry, in bytes; longer text is cut and the
/// entry is marked [`HistoryEntry::truncated`].
pub const MAX_HISTORY_SQL_BYTES: usize = 262_144;

/// The largest accepted `max_entries`.
pub(crate) const MAX_HISTORY_CAP: usize = 100_000;

/// Whether and how much history is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryOptions {
    /// When `false`, [`HistoryStore::record`](super::HistoryStore::record)
    /// stores nothing.
    pub enabled: bool,
    /// Entries kept, newest first out of the oldest. Clamped to `1..=100_000`.
    pub max_entries: usize,
}

impl Default for HistoryOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: DEFAULT_HISTORY_CAP,
        }
    }
}

impl HistoryOptions {
    /// The options with `max_entries` clamped into its range.
    pub(crate) fn clamped(self) -> Self {
        Self {
            enabled: self.enabled,
            max_entries: self.max_entries.clamp(1, MAX_HISTORY_CAP),
        }
    }
}

/// How an executed query ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HistoryOutcome {
    /// The query completed.
    Ok,
    /// The query failed. The error text is never recorded.
    Error,
    /// The user cancelled the query.
    Cancelled,
}

impl HistoryOutcome {
    pub(crate) fn as_wire(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_wire(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(Self::Ok),
            "error" => Some(Self::Error),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A query to record. There is deliberately no field for bound parameters or
/// error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHistoryEntry {
    /// The SQL text as executed. Credentials are redacted before storing.
    pub sql: String,
    /// The connection it ran on.
    pub connection: ConnectionId,
    /// When execution started.
    pub started_at: SystemTime,
    /// How long it ran.
    pub duration: Duration,
    /// How it ended.
    pub outcome: HistoryOutcome,
    /// Rows returned or affected, when known.
    pub rows: Option<u64>,
}

/// A stored history entry. Times have millisecond precision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub(crate) sql: String,
    pub(crate) connection: ConnectionId,
    pub(crate) started_at: SystemTime,
    pub(crate) duration: Duration,
    pub(crate) outcome: HistoryOutcome,
    pub(crate) rows: Option<u64>,
    pub(crate) truncated: bool,
}

impl HistoryEntry {
    /// The stored SQL text (redacted, and cut when [`truncated`](Self::truncated)).
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// The connection it ran on.
    #[must_use]
    pub fn connection(&self) -> &ConnectionId {
        &self.connection
    }

    /// When execution started.
    #[must_use]
    pub fn started_at(&self) -> SystemTime {
        self.started_at
    }

    /// How long it ran.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// How it ended.
    #[must_use]
    pub fn outcome(&self) -> HistoryOutcome {
        self.outcome
    }

    /// Rows returned or affected, when known.
    #[must_use]
    pub fn rows(&self) -> Option<u64> {
        self.rows
    }

    /// Whether the SQL was cut to [`MAX_HISTORY_SQL_BYTES`].
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// A search over the stored entries.
#[derive(Debug, Clone, Copy)]
pub struct HistoryQuery<'a> {
    /// Whitespace-separated terms; each must occur in the SQL, ignoring case.
    /// Empty matches everything.
    pub text: &'a str,
    /// Only entries of this connection.
    pub connection: Option<&'a ConnectionId>,
    /// At most this many results.
    pub limit: usize,
}

impl Default for HistoryQuery<'_> {
    fn default() -> Self {
        Self {
            text: "",
            connection: None,
            limit: usize::MAX,
        }
    }
}

/// A line of `history.jsonl` that could not be loaded. It stays in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryIssue {
    /// 1-based line number in the file as loaded.
    pub line: usize,
    /// What is wrong with it.
    pub kind: HistoryIssueKind,
}

/// Why a history line was not loaded. Messages never echo the line.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HistoryIssueKind {
    /// Not valid UTF-8 or JSON, wrong shape, or a bad value.
    Malformed {
        /// 1-based column of the first problem, when known.
        column: Option<usize>,
    },
    /// Written by a newer `SQLer`.
    NewerVersion {
        /// The version found on the line.
        version: u64,
    },
}

impl fmt::Display for HistoryIssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { column: Some(c) } => write!(f, "malformed entry near column {c}"),
            Self::Malformed { column: None } => f.write_str("malformed entry"),
            Self::NewerVersion { version } => write!(
                f,
                "written by a newer SQLer (version {version}, this build reads up to {HISTORY_FORMAT_VERSION})"
            ),
        }
    }
}

impl fmt::Display for HistoryIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.kind)
    }
}
