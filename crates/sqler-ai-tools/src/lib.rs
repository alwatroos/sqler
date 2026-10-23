// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tool set exposed to models, with every guard behind it (ADR-0036).
//!
//! One [`ToolSet`] serves every AI surface (the in-app chat, MCP, ACP). A host builds it from a
//! [`SessionSource`] (connections, catalogs and lent sessions), [`Limits`] and optionally an
//! [`Approver`] and a [`UiHost`]. [`ToolSet::definitions`] exports the tools as
//! [`sqler_ai_api::ToolDefinition`]s with JSON Schemas; [`ToolSet::call`] runs one
//! [`sqler_ai_api::ToolCall`] and always returns a [`sqler_ai_api::ToolResult`].
//!
//! # Tools
//!
//! Catalog tools (`list_connections`, `list_schemas`, `list_tables`, `describe_table`,
//! `describe_function`, `search_schema`) answer from the `sqler-catalog` cache and do not touch
//! the database when the node is cached. `describe_function` may read a definition text through
//! one lent session and caches it. SQL tools (`run_query`, `sample_rows`, `explain_query`) share
//! one guarded path. `open_in_editor` and `open_results_tab` exist only with a [`UiHost`].
//!
//! # Guard rules
//!
//! - Statements are split and classified by `sqler-sql` (fail-safe: anything not proven
//!   read-only is not `Read`). A driver without a dialect gives one `Unknown` statement.
//! - `Read` runs without asking. Write, DDL, other and unknown statements need an
//!   [`Approver`] decision per statement, shown the exact SQL. No approver means refuse.
//! - All approvals of a call are collected before the first statement runs; a denial stops the
//!   call with nothing run.
//! - A `prod` connection asks a second time ([`ApprovalStage::ProdSecond`]).
//! - `EXPLAIN ANALYZE` needs approval ([`ApprovalReason::ExplainAnalyze`]).
//! - A read-only connection refuses anything that needs approval.
//!
//! # Limits and cancellation
//!
//! [`Limits`] cap rows per statement and time per call. `prod` ignores `unlimited`. Rows beyond
//! the cap are dropped, the statement says `truncated: "row_limit"` and carries a notice. At the
//! deadline the tool calls the driver's [`sqler_driver_api::CancelHandle`], drops the stream,
//! marks `time_limit`, and returns; statements not yet started are counted in `not_run`. A driver
//! that cannot interrupt a step (`SQLite` before its first row) stops later than the tool returns.
//! Dropping the call's future cancels it; the lent session is then dropped without `release`.

//! # Redaction, untrusted data, audit
//!
//! Every call goes through one path (ADR-0037), so no tool can skip these steps. The result is
//! redacted ([`Redaction`]): values of columns on the redaction list, of statements that mention
//! a listed word, and of every row when the connection does not send rows, plus driver error
//! texts that could echo a value. The JSON is then wrapped in an untrusted-data envelope
//! ([`wrap_untrusted`]; a consumer parses it with [`unwrap_untrusted`]). Finally an
//! [`AuditEntry`] goes to the optional [`AuditSink`] ([`AuditLog`] persists to
//! `ai-audit.jsonl`). Entries hold SQL and arguments passed through credential redaction and
//! size caps, and the SQL of a statement that mentions a listed word has its literals masked;
//! never cell values or statement error texts.
//!
//! # External tools
//!
//! A host can add tools with [`ToolSet::with_external_tools`] (plugins use this). They are listed
//! after the built-in tools, their arguments are checked against a small subset of JSON Schema,
//! and every call leaves through the same path as a built-in call: redaction, the untrusted
//! envelope and the audit row apply. A statement that needs approval goes through the surface's
//! own [`Approver`] ([`ExternalContext::approve`]); one denial, or no approver, fails the call.
//!
//! # Exploration sessions
//!
//! The [`session`] module holds the headless engine of the in-app exploration chat (ADR-0039):
//! a tool loop over one connection with step, call, token and time limits, loop detection,
//! retries, cancellation, approvals through the tool set and a memory-only transcript.

#![forbid(unsafe_code)]

mod approve;
mod assist;
mod audit;
mod catalog_tools;
mod error;
mod exec;
mod external;
mod guard;
mod json;
mod limits;
mod query_tools;
mod redact;
mod schema;
pub mod session;
mod source;
mod toolset;
mod ui;
mod untrusted;

pub use approve::{ApprovalReason, ApprovalRequest, ApprovalStage, Approver, Decision};
pub use assist::{
    Assist, AssistError, AssistOptions, AssistSearch, Conversation, DataSearch, Explanation,
    FixProposal, GeneratedSql, GroundedTable, Grounding, PROMPT_VERSION, ParsedAnswer, SqlAnswer,
    Turn, TurnKind, extract_sql, parse_sql_answer, unified_diff,
};
pub use audit::{
    AUDIT_FILE_NAME, AUDIT_FORMAT_VERSION, ApprovalOutcome, AuditConnection, AuditCursor,
    AuditEntry, AuditError, AuditFailure, AuditLog, AuditOptions, AuditPage, AuditQuery, AuditSink,
    AuditStatement, CallOutcome, MAX_AUDIT_SQL_BYTES, MAX_AUDIT_STATEMENTS, MAX_AUDIT_TEXT_BYTES,
    MemoryAudit, Surface,
};
pub use error::ToolError;
pub use external::{
    EXTERNAL_NAME_SEPARATOR, EXTERNAL_TOOL, ExternalApproval, ExternalContext, ExternalTool,
    ExternalTools, MAX_EXTERNAL_DESCRIPTION_BYTES,
};
pub use limits::{Caps, Limits};
pub use redact::{REDACTED, RedactScope, Redaction, RedactionSummary};
pub use schema::{definitions, names};
pub use source::{ConnectionInfo, SessionSource};
pub use toolset::{CallReport, ExecutedStatement, ToolSet, Truncation};
pub use ui::UiHost;
pub use untrusted::{
    UNTRUSTED_CLOSE, UNTRUSTED_NOTICE, UNTRUSTED_OPEN, unwrap_untrusted, wrap_untrusted,
    wrap_untrusted_text,
};
