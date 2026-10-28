// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The typed, in-memory transcript of a session.
//!
//! Everything in it comes from redacted [`CallReport`] data or from the user's and the model's
//! text. Nothing is written to disk here; the UI stores entries through the chat history
//! (ADR-0077), and [`Transcript::restore`] seeds a transcript from a stored chat.

use std::time::Duration;

use serde_json::Value;
use sqler_ai_api::ToolCall;
use sqler_config::redact_credentials;
use sqler_sql::StatementClass;

use crate::session::LimitKind;
use crate::toolset::{CallReport, Truncation};
use crate::untrusted::unwrap_untrusted;

/// Why a call the model asked for did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotRunReason {
    /// A session limit was reached first.
    Limit(LimitKind),
    /// The same call had already run too often in this turn.
    Repeated,
    /// The turn was cancelled.
    Cancelled,
}

/// How a tool call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolCallStatus {
    /// The tool returned a result.
    Ok,
    /// The tool returned an error result (the code and message are from the audit entry).
    Error {
        /// The stable error code, such as `denied` or `invalid_arguments`.
        code: String,
        /// The credential-redacted, capped message.
        message: String,
    },
    /// The session did not run the call. The model never saw it.
    NotRun(NotRunReason),
    /// The turn was cancelled while the call was in flight, after the approval step. Statements
    /// may have run, so this is not a promise that nothing happened. The audit log has one
    /// `cancelled` entry for the call that says which statements were sent. The model never saw
    /// it.
    Cancelled,
}

/// One tool call of the model.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ToolCallEntry {
    /// The call id given by the model.
    pub call_id: String,
    /// The tool name as called.
    pub tool: String,
    /// The arguments, credential-redacted and capped (the audit entry's copy). `Null` for a call
    /// that did not run, because nothing has redacted it.
    pub arguments: Value,
    /// How it ended.
    pub status: ToolCallStatus,
    /// The result content with the untrusted-data envelope removed; `Null` if there is none.
    pub result: Value,
}

/// A statement the database ran for a tool call.
///
/// The exact SQL is kept private, for [`SqlEntry::rerun`] only. Everything that shows or logs
/// the entry (the events, `Debug`) uses the credential-redacted `display_sql`.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SqlEntry {
    /// The call that ran it.
    pub call_id: String,
    /// The id of the connection.
    pub connection: String,
    /// The exact SQL that was sent, for a re-run.
    pub(crate) sql: String,
    /// The SQL to show: the audit copy (credential-redacted, capped), or the
    /// credential-redacted SQL beyond the audit statement cap.
    pub display_sql: String,
    /// The statement class.
    pub class: StatementClass,
    /// Whether the user approved it explicitly (a plain read needs none).
    pub approved: bool,
    /// Rows kept.
    pub rows: u64,
    /// Whether and why the rows were cut.
    pub truncated: Option<Truncation>,
    /// Whether the database reported an error.
    pub failed: bool,
}

/// What the UI feeds to the run flow (ADR-0024) to run a statement again. The session itself
/// runs nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RerunRequest {
    /// The id of the connection.
    pub connection: String,
    /// The exact SQL.
    pub sql: String,
}

impl std::fmt::Debug for SqlEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlEntry")
            .field("call_id", &self.call_id)
            .field("connection", &self.connection)
            .field("display_sql", &self.display_sql)
            .field("class", &self.class)
            .field("approved", &self.approved)
            .field("rows", &self.rows)
            .field("truncated", &self.truncated)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl SqlEntry {
    /// The request that runs this statement again.
    #[must_use]
    pub fn rerun(&self) -> RerunRequest {
        RerunRequest {
            connection: self.connection.clone(),
            sql: self.sql.clone(),
        }
    }

    /// Whether the exact SQL is known, so the statement can be run again. `false` for a
    /// statement restored from the chat history, which stores only the redacted display text.
    #[must_use]
    pub fn can_rerun(&self) -> bool {
        !self.sql.is_empty()
    }
}

/// A line the engine adds on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Notice {
    /// A model step is retried after `delay`.
    Retrying {
        /// The retry number, from 1.
        attempt: u32,
        /// The wait before it.
        delay: Duration,
    },
    /// The turn stopped at a limit.
    LimitReached(LimitKind),
    /// The model repeated a call; the turn stopped.
    LoopDetected {
        /// The tool that was repeated.
        tool: String,
    },
    /// The turn was cancelled.
    Cancelled,
    /// The provider failed; the turn stopped.
    ProviderError {
        /// The provider error text.
        message: String,
    },
    /// The model stopped because of its output cap, so its answer may be cut.
    OutputTruncated,
}

/// One line of the transcript.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TranscriptEntry {
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
    ToolCall(ToolCallEntry),
    /// A statement that ran.
    Sql(SqlEntry),
    /// An engine line.
    Notice(Notice),
}

/// The most entries a [`Transcript`] keeps before a new user turn drops the oldest ones.
///
/// A single turn can add more than this; entries are never dropped inside a turn.
pub const MAX_TRANSCRIPT_ENTRIES: usize = 500;

/// Everything said and done in a session, in order.
///
/// When a new user turn starts and the transcript holds [`MAX_TRANSCRIPT_ENTRIES`] or more
/// entries, the oldest ones are dropped until there is room for the new one. The stable number
/// of `entries()[i]` is `dropped() + i`: it does not change when older entries are dropped.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transcript {
    entries: Vec<TranscriptEntry>,
    dropped: usize,
}

impl Transcript {
    /// All entries, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// How many of the oldest entries were dropped to stay within the cap.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// The statements that ran, oldest first.
    pub fn sql(&self) -> impl Iterator<Item = &SqlEntry> {
        self.entries.iter().filter_map(|e| match e {
            TranscriptEntry::Sql(s) => Some(s),
            _ => None,
        })
    }

    /// A transcript seeded with restored entries: the last [`MAX_TRANSCRIPT_ENTRIES`] of them,
    /// where `dropped` is the stable number of `entries[0]`.
    pub(crate) fn restore(mut entries: Vec<TranscriptEntry>, mut dropped: usize) -> Self {
        if entries.len() > MAX_TRANSCRIPT_ENTRIES {
            let excess = entries.len() - MAX_TRANSCRIPT_ENTRIES;
            entries.drain(..excess);
            dropped += excess;
        }
        Self { entries, dropped }
    }

    pub(crate) fn push(&mut self, entry: TranscriptEntry) {
        if matches!(entry, TranscriptEntry::User { .. })
            && self.entries.len() >= MAX_TRANSCRIPT_ENTRIES
        {
            let excess = self.entries.len() + 1 - MAX_TRANSCRIPT_ENTRIES;
            self.entries.drain(..excess);
            self.dropped += excess;
        }
        self.entries.push(entry);
    }

    /// Appends the entry of a call that did not run.
    pub(crate) fn push_not_run(&mut self, call: &ToolCall, reason: NotRunReason) {
        self.entries.push(TranscriptEntry::ToolCall(ToolCallEntry {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            arguments: Value::Null,
            status: ToolCallStatus::NotRun(reason),
            result: Value::Null,
        }));
    }

    /// Appends the entry of a call whose future was dropped by a cancel after the approval step.
    pub(crate) fn push_cancelled(&mut self, call: &ToolCall) {
        self.entries.push(TranscriptEntry::ToolCall(ToolCallEntry {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            arguments: Value::Null,
            status: ToolCallStatus::Cancelled,
            result: Value::Null,
        }));
    }

    /// Appends the `ToolCall` entry of a call that ran, then one `Sql` entry per statement.
    pub(crate) fn push_report(&mut self, call: &ToolCall, report: &CallReport) {
        let status = match &report.audit.error {
            Some(failure) => ToolCallStatus::Error {
                code: failure.code.clone(),
                message: failure.message.clone(),
            },
            None => ToolCallStatus::Ok,
        };
        let result = unwrap_untrusted(&report.result.content)
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or(Value::Null);
        self.entries.push(TranscriptEntry::ToolCall(ToolCallEntry {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            arguments: report.audit.arguments.clone(),
            status,
            result,
        }));
        let connection = report.connection.clone().unwrap_or_default();
        for (i, run) in report.executed.iter().enumerate() {
            let display_sql = report.audit.statements.get(i).map_or_else(
                || redact_credentials(&run.sql).into_owned(),
                |s| s.sql.clone(),
            );
            self.entries.push(TranscriptEntry::Sql(SqlEntry {
                call_id: call.id.clone(),
                connection: connection.clone(),
                sql: run.sql.clone(),
                display_sql,
                class: run.class,
                approved: run.approved,
                rows: run.rows,
                truncated: run.truncated,
                failed: run.failed,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::field_reassign_with_default)]

    use serde_json::json;
    use sqler_ai_api::ToolResult;

    use super::*;
    use crate::audit::{AuditEntry, AuditFailure, AuditStatement};
    use crate::redact::RedactionSummary;
    use crate::toolset::ExecutedStatement;
    use crate::untrusted::wrap_untrusted;

    fn executed(sql: &str) -> ExecutedStatement {
        ExecutedStatement {
            sql: sql.to_owned(),
            class: StatementClass::Read,
            approved: false,
            rows: 3,
            truncated: Some(Truncation::RowLimit),
            failed: false,
        }
    }

    fn report(statements: Vec<ExecutedStatement>, audit: AuditEntry) -> CallReport {
        CallReport {
            result: ToolResult::ok("c1", wrap_untrusted(&json!({"n": 1}))),
            connection: Some("db".to_owned()),
            executed: statements,
            audit,
            redaction: RedactionSummary::default(),
        }
    }

    #[test]
    fn maps_a_report_to_entries() {
        let mut audit = AuditEntry::default();
        audit.arguments = json!({"sql": "select 1"});
        let mut st = AuditStatement::default();
        st.sql = "select 1 /* shown */".to_owned();
        audit.statements = vec![st];
        let call = ToolCall::new("c1", "run_query", "{}");
        let mut t = Transcript::default();
        t.push_report(&call, &report(vec![executed("select 1")], audit));
        let [TranscriptEntry::ToolCall(tc), TranscriptEntry::Sql(sql)] = t.entries() else {
            panic!("{:?}", t.entries());
        };
        assert_eq!(tc.status, ToolCallStatus::Ok);
        assert_eq!(tc.arguments, json!({"sql": "select 1"}));
        assert_eq!(tc.result, json!({"n": 1}));
        assert_eq!(sql.display_sql, "select 1 /* shown */");
        assert_eq!(sql.sql, "select 1");
        assert_eq!(sql.rows, 3);
        assert_eq!(sql.truncated, Some(Truncation::RowLimit));
        assert_eq!(t.sql().count(), 1);
    }

    #[test]
    fn display_sql_falls_back_beyond_the_audit_cap() {
        let call = ToolCall::new("c1", "run_query", "{}");
        let mut t = Transcript::default();
        t.push_report(
            &call,
            &report(vec![executed("select 1"), executed("select 2")], {
                let mut a = AuditEntry::default();
                a.statements = vec![AuditStatement::default()];
                a.statements[0].sql = "select 1".to_owned();
                a
            }),
        );
        let sql: Vec<_> = t.sql().collect();
        assert_eq!(sql[1].display_sql, "select 2");
    }

    #[test]
    fn error_code_and_message_come_from_the_audit() {
        let mut audit = AuditEntry::default();
        audit.error = Some(AuditFailure::new("denied", "statement 0 was not approved"));
        let call = ToolCall::new("c1", "run_query", "{}");
        let mut t = Transcript::default();
        t.push_report(&call, &report(vec![], audit));
        let TranscriptEntry::ToolCall(tc) = &t.entries()[0] else {
            panic!()
        };
        assert_eq!(
            tc.status,
            ToolCallStatus::Error {
                code: "denied".into(),
                message: "statement 0 was not approved".into()
            }
        );
    }

    #[test]
    fn rerun_copies_the_exact_sql() {
        let call = ToolCall::new("c1", "run_query", "{}");
        let mut t = Transcript::default();
        t.push_report(
            &call,
            &report(vec![executed("select  1 -- x")], AuditEntry::default()),
        );
        let r = t.sql().next().unwrap().rerun();
        assert_eq!(r.connection, "db");
        assert_eq!(r.sql, "select  1 -- x");
    }

    #[test]
    fn debug_and_display_never_show_the_exact_sql_beyond_the_audit_cap() {
        let secret = "update t set k = 'AKIAIOSFODNN7EXAMPLE' where password = 'hunter2'";
        let call = ToolCall::new("c1", "run_query", "{}");
        let mut t = Transcript::default();
        t.push_report(
            &call,
            &report(vec![executed(secret)], AuditEntry::default()),
        );
        let entry = t.sql().next().unwrap();
        assert!(!format!("{entry:?}").contains("hunter2"), "{entry:?}");
        assert!(
            !entry.display_sql.contains("hunter2"),
            "{}",
            entry.display_sql
        );
        assert_eq!(entry.rerun().sql, secret);
    }

    #[test]
    fn a_user_entry_drops_the_oldest_beyond_the_cap() {
        let mut t = Transcript::default();
        for i in 0..MAX_TRANSCRIPT_ENTRIES {
            t.push(TranscriptEntry::Assistant {
                text: i.to_string(),
            });
        }
        assert_eq!(t.dropped(), 0);
        // Not a user entry: no trimming, even above the cap.
        t.push(TranscriptEntry::Assistant { text: "x".into() });
        assert_eq!(t.entries().len(), MAX_TRANSCRIPT_ENTRIES + 1);
        assert_eq!(t.dropped(), 0);
        t.push(TranscriptEntry::User { text: "u".into() });
        assert_eq!(t.entries().len(), MAX_TRANSCRIPT_ENTRIES);
        assert_eq!(t.dropped(), 2);
        // Stable number 2 is the first entry kept.
        assert_eq!(
            t.entries()[0],
            TranscriptEntry::Assistant { text: "2".into() }
        );
    }

    #[test]
    fn not_run_calls_carry_no_arguments() {
        let call = ToolCall::new("c9", "run_query", r#"{"password":"hunter2"}"#);
        let mut t = Transcript::default();
        t.push_not_run(&call, NotRunReason::Repeated);
        assert!(!format!("{t:?}").contains("hunter2"));
    }
}
