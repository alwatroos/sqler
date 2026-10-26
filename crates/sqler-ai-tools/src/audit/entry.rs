// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The audit entry types.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::redact::RedactionSummary;

macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        #[non_exhaustive]
        pub enum $name {
            $($(#[$vm])* $var,)+
        }

        impl $name {
            /// The name written to the audit file.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$var => $text,)+ }
            }

            /// Parses a name written by [`Self::as_str`].
            #[must_use]
            pub fn from_wire(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$var),)+ _ => None }
            }
        }
    };
}

wire_enum! {
    /// Which front end made the call.
    Surface {
        /// The in-app assistant.
        Assist => "assist",
        /// The in-app agent session.
        Session => "session",
        /// The MCP server.
        Mcp => "mcp",
        /// An ACP agent.
        Agent => "agent",
        /// The read-only AI chat panel.
        Chat => "chat",
        /// The host did not say.
        #[default]
        Unspecified => "unspecified",
    }
}

wire_enum! {
    /// What happened to approval, for a statement or for a whole call.
    ApprovalOutcome {
        /// Nothing needed approval.
        #[default]
        NotNeeded => "not_needed",
        /// The user approved.
        Approved => "approved",
        /// The user denied.
        Denied => "denied",
        /// Refused without asking: no approver, or a read-only connection.
        Refused => "refused",
        /// Approval was needed but never asked, because an earlier statement stopped the call.
        NotAsked => "not_asked",
    }
}

wire_enum! {
    /// Whether the call returned a result or an error.
    CallOutcome {
        /// A result.
        #[default]
        Ok => "ok",
        /// An error body.
        Error => "error",
        /// The call's future was dropped (client cancel, shutdown, UI close) before it finished.
        /// The entry is best effort: a statement marked `ran` was sent, or may have been sent.
        Cancelled => "cancelled",
    }
}

/// The connection a call used.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct AuditConnection {
    /// The connection id.
    pub id: String,
    /// The display name.
    pub name: String,
}

impl AuditConnection {
    /// A connection reference.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// Why a call failed. The message is credential-redacted and capped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct AuditFailure {
    /// The error code of the tool result.
    pub code: String,
    /// The message.
    pub message: String,
}

impl AuditFailure {
    /// A failure.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// One statement of a call. Holds no values and no error text, only whether it failed.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct AuditStatement {
    /// The SQL, cut to `MAX_AUDIT_SQL_BYTES` and credential-redacted.
    pub sql: String,
    /// Whether the SQL was cut.
    pub sql_truncated: bool,
    /// The statement class (`read`, `write`, `ddl`, `other`, `unknown`).
    pub class: String,
    /// The approval outcome of this statement.
    pub approval: ApprovalOutcome,
    /// Whether the statement was sent to the database.
    pub ran: bool,
    /// Rows returned, if it ran.
    pub rows: Option<u64>,
    /// `row_limit` or `time_limit`, if the result was cut.
    pub truncated: Option<String>,
    /// Whether the database reported an error.
    pub failed: bool,
}

/// One tool call.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct AuditEntry {
    /// A unique id: `<ts_ms hex>-<pid hex>-<counter hex>`.
    pub id: String,
    /// When the call started, in Unix milliseconds.
    pub ts_ms: u64,
    /// The front end.
    pub surface: Surface,
    /// The tool name as called (even if unknown).
    pub tool: String,
    /// The connection, if the call named one that exists.
    pub connection: Option<AuditConnection>,
    /// The arguments, credential-redacted and capped.
    pub arguments: serde_json::Value,
    /// Whether any argument text was cut.
    pub arguments_truncated: bool,
    /// The statements, at most `MAX_AUDIT_STATEMENTS`.
    pub statements: Vec<AuditStatement>,
    /// Statements beyond the cap.
    pub statements_omitted: u32,
    /// The call-level approval outcome.
    pub approval: ApprovalOutcome,
    /// Result or error.
    pub outcome: CallOutcome,
    /// The error, if the outcome is `error`.
    pub error: Option<AuditFailure>,
    /// The size of the result content that went to the model.
    pub result_bytes: u64,
    /// Rows returned by all statements.
    pub rows: u64,
    /// How long the call took.
    pub duration_ms: u64,
    /// What redaction hid.
    pub redaction: RedactionSummary,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A process-unique entry id.
pub(crate) fn new_id(ts_ms: u64) -> String {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{ts_ms:x}-{:x}-{n:x}", std::process::id())
}
