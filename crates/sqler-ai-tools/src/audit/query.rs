// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Reading the audit log: filters, pages and cursors.

use super::entry::{ApprovalOutcome, AuditEntry, CallOutcome, Surface};

const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 5_000;

/// A position in the log: the last entry of the previous page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuditCursor {
    /// The `ts_ms` of the entry.
    pub ts_ms: u64,
    /// The `id` of the entry.
    pub id: String,
}

impl AuditCursor {
    /// A cursor after the entry with this id and time.
    #[must_use]
    pub fn new(ts_ms: u64, id: impl Into<String>) -> Self {
        Self {
            ts_ms,
            id: id.into(),
        }
    }
}

/// Which entries to read. Every filter that is set must match.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuditQuery {
    /// Only this connection id.
    pub connection: Option<String>,
    /// Only this surface.
    pub surface: Option<Surface>,
    /// Only this tool.
    pub tool: Option<String>,
    /// Only this call-level approval outcome.
    pub approval: Option<ApprovalOutcome>,
    /// Only this outcome.
    pub outcome: Option<CallOutcome>,
    /// Only entries at or after this time.
    pub since_ms: Option<u64>,
    /// Only entries at or before this time.
    pub until_ms: Option<u64>,
    /// Case-insensitive substring over tool, connection id and name, and statement SQL.
    pub text: Option<String>,
    /// The page size, 1 to 5000.
    pub limit: usize,
    /// Where the previous page ended.
    pub cursor: Option<AuditCursor>,
}

impl Default for AuditQuery {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditQuery {
    /// Matches everything; the limit is 200.
    #[must_use]
    pub fn new() -> Self {
        Self {
            connection: None,
            surface: None,
            tool: None,
            approval: None,
            outcome: None,
            since_ms: None,
            until_ms: None,
            text: None,
            limit: DEFAULT_LIMIT,
            cursor: None,
        }
    }

    /// Only this connection id.
    #[must_use]
    pub fn with_connection(mut self, id: impl Into<String>) -> Self {
        self.connection = Some(id.into());
        self
    }

    /// Only this surface.
    #[must_use]
    pub fn with_surface(mut self, surface: Surface) -> Self {
        self.surface = Some(surface);
        self
    }

    /// Only this tool.
    #[must_use]
    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    /// Only this call-level approval outcome.
    #[must_use]
    pub fn with_approval(mut self, approval: ApprovalOutcome) -> Self {
        self.approval = Some(approval);
        self
    }

    /// Only this outcome.
    #[must_use]
    pub fn with_outcome(mut self, outcome: CallOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// Only entries at or after `ms`.
    #[must_use]
    pub fn with_since_ms(mut self, ms: u64) -> Self {
        self.since_ms = Some(ms);
        self
    }

    /// Only entries at or before `ms`.
    #[must_use]
    pub fn with_until_ms(mut self, ms: u64) -> Self {
        self.until_ms = Some(ms);
        self
    }

    /// Case-insensitive substring over tool, connection id and name, and statement SQL.
    #[must_use]
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// The page size, clamped to 1..=5000.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.clamp(1, MAX_LIMIT);
        self
    }

    /// Continues after a previous page.
    #[must_use]
    pub fn with_cursor(mut self, cursor: AuditCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Whether `entry` passes every filter (the cursor is not a filter).
    pub(crate) fn matches(&self, entry: &AuditEntry, text_lower: Option<&str>) -> bool {
        let conn = entry.connection.as_ref();
        self.connection
            .as_deref()
            .is_none_or(|c| conn.is_some_and(|k| k.id == c))
            && self.surface.is_none_or(|s| entry.surface == s)
            && self.tool.as_deref().is_none_or(|t| entry.tool == t)
            && self.approval.is_none_or(|a| entry.approval == a)
            && self.outcome.is_none_or(|o| entry.outcome == o)
            && self.since_ms.is_none_or(|t| entry.ts_ms >= t)
            && self.until_ms.is_none_or(|t| entry.ts_ms <= t)
            && text_lower.is_none_or(|needle| {
                entry.tool.to_lowercase().contains(needle)
                    || conn.is_some_and(|k| {
                        k.id.to_lowercase().contains(needle)
                            || k.name.to_lowercase().contains(needle)
                    })
                    || entry
                        .statements
                        .iter()
                        .any(|s| s.sql.to_lowercase().contains(needle))
            })
    }
}

/// One page of entries.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AuditPage {
    /// The entries, newest first.
    pub entries: Vec<AuditEntry>,
    /// Set when the limit was reached; pass it to [`AuditQuery::with_cursor`] for the next page.
    pub next: Option<AuditCursor>,
    /// Lines that did not parse. Their content is never reported.
    pub skipped_lines: usize,
    /// Lines written by a newer version.
    pub newer_lines: usize,
}
