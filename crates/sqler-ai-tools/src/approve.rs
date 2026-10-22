// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The approver: how a host is asked to confirm a statement.

use sqler_ai_api::BoxFuture;
use sqler_sql::StatementClass;

use crate::source::ConnectionInfo;

/// The answer to an [`ApprovalRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// Run the statement.
    Approve,
    /// Do not run it; the whole call stops and nothing runs.
    Deny,
}

/// Which confirmation this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApprovalStage {
    /// The first (and for non-prod, only) confirmation.
    Confirm,
    /// The second confirmation that `prod` connections require.
    ProdSecond,
}

/// Why a statement needs approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApprovalReason {
    /// The statement changes data.
    Write,
    /// The statement changes the schema or permissions.
    Ddl,
    /// Transaction control, settings and the like.
    Other,
    /// The classifier did not recognise the statement (it is never treated as a read).
    Unknown,
    /// `EXPLAIN ANALYZE` executes the statement it explains.
    ExplainAnalyze,
}

impl ApprovalReason {
    /// A stable lowercase name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Ddl => "ddl",
            Self::Other => "other",
            Self::Unknown => "unknown",
            Self::ExplainAnalyze => "explain_analyze",
        }
    }
}

/// One question to the approver: run exactly this statement?
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ApprovalRequest {
    /// The tool that wants to run it.
    pub tool: &'static str,
    /// The connection it would run on.
    pub connection: ConnectionInfo,
    /// The exact SQL text that would be sent to the driver.
    pub sql: String,
    /// What the classifier made of it.
    pub class: StatementClass,
    /// Why approval is needed.
    pub reason: ApprovalReason,
    /// Which confirmation this is.
    pub stage: ApprovalStage,
    /// The zero-based index of the statement in this call.
    pub index: usize,
    /// The number of statements in this call.
    pub count: usize,
}

/// Asks the user (or a policy) whether a statement may run.
///
/// All approvals of a call are asked, in statement order, before the first statement runs.
/// A call with no approver refuses every statement that needs one.
pub trait Approver: Send + Sync {
    /// Decides one request. Dropping the future means "not approved".
    fn approve<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision>;
}
