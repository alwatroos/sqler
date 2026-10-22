// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI audit log: one entry per tool call, kept in `ai-audit.jsonl` (ADR-0037).
//!
//! An [`AuditEntry`] holds what was asked and what happened: the tool, the connection, the
//! (credential-redacted, capped) arguments and SQL, the approval outcome and counts. It never
//! holds cell values, statement error texts, configuration, keys or tokens.
//!
//! The file format follows the query history (ADR-0021): one versioned JSON object per line,
//! appended only, a torn last line is skipped on read, and old files are rotated by rename.
//! A write failure never fails a tool call; it is counted ([`AuditLog::failed_writes`]).
//!
//! A call whose future is dropped before it finishes leaves a best-effort entry with outcome
//! `cancelled`. The format stays version 1: a reader that does not know the value treats the
//! line as malformed and skips it, as it does any other unknown name.

mod draft;
mod entry;
mod query;
mod store;
mod wire;

use std::sync::{Mutex, PoisonError};

pub(crate) use draft::{AuditDraft, CancelPoint, DraftError, DraftRun, DraftStatement};
pub use entry::{
    ApprovalOutcome, AuditConnection, AuditEntry, AuditFailure, AuditStatement, CallOutcome,
    Surface,
};
pub use query::{AuditCursor, AuditPage, AuditQuery};
pub use store::{AuditError, AuditLog, AuditOptions};

/// The wire format version written to every line.
pub const AUDIT_FORMAT_VERSION: u32 = 1;
/// The audit file name inside the config directory.
pub const AUDIT_FILE_NAME: &str = "ai-audit.jsonl";
/// The most SQL bytes kept per statement and per `sql` argument.
pub const MAX_AUDIT_SQL_BYTES: usize = 16_384;
/// The most bytes kept per other text (arguments, error messages).
pub const MAX_AUDIT_TEXT_BYTES: usize = 1_024;
/// The most serialized bytes kept for all arguments of one call; beyond it only the size is kept.
pub const MAX_AUDIT_ARGUMENTS_BYTES: usize = 65_536;
/// The most statements kept per entry; the rest are counted in `statements_omitted`.
pub const MAX_AUDIT_STATEMENTS: usize = 64;

/// Where finished entries go. `record` must not panic and must not block for long.
pub trait AuditSink: Send + Sync {
    /// Takes one entry. Failures are the sink's to keep; they never reach the tool call.
    fn record(&self, entry: &AuditEntry);
}

/// A sink that keeps entries in memory, for tests and embedders.
#[derive(Debug, Default)]
pub struct MemoryAudit {
    entries: Mutex<Vec<AuditEntry>>,
}

impl MemoryAudit {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The entries recorded so far, oldest first.
    #[must_use]
    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl AuditSink for MemoryAudit {
    fn record(&self, entry: &AuditEntry) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(entry.clone());
    }
}

#[cfg(test)]
mod tests;
