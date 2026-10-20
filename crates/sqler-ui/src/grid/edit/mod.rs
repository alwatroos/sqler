// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The headless model of staged edits for the results grid (ADR-0030).
//!
//! Read-only is the default and is enforced by type: [`check_editable`] returns an
//! [`EditTarget`] only for a result that comes from one base table with a fully present key (or
//! an explicit keyless opt-in), and [`StagedEdits`] can only be built from an [`EditTarget`].
//! [`StagedEdits::plan`] turns the staged changes into an [`ApplyPlan`]: the complete, ordered list
//! of statements, with every value bound through `Query::params`. [`apply()`] runs exactly that
//! list inside `BEGIN`/`COMMIT` on a borrowed session and rolls back on any row-count mismatch,
//! driver error or cancel before `COMMIT`. An error or cancel returned by `COMMIT` itself is
//! [`ApplyOutcome::CommitUnknown`]: the server may have committed.
//!
//! # Production gate
//!
//! This module does not ask the user, but [`apply()`] takes only a [`ConfirmedPlan`], which is
//! built by [`ApplyPlan::confirm`] (a `prod` plan needs the user's consent, so the caller shows
//! [`ApplyPlan::render_preview`] first; [`ApplyPlan::needs_confirmation`] is the question to ask)
//! or by [`ApplyPlan::confirm_non_production`]. The latter is for connections that are not `prod`.
//! The types make *forgetting* the step impossible; they cannot tell whether the booleans the
//! caller passes are true, so the caller derives `production` from the connection and
//! `user_confirmed` from a real user step. Both methods consume the plan, so one [`ApplyPlan`]
//! yields at most one [`ConfirmedPlan`] and `apply` consumes that: a second apply (for example
//! after `Committed`, which would repeat the `INSERT`s) needs a plan rebuilt from the staged
//! edits.
//!
//! # Preconditions
//!
//! * `resolved_schema` (see [`EditRequest`]) must be what the server resolves the unqualified
//!   `FROM` name *as written in the query* to, looked up per query on its session (not a
//!   session-level constant): temporary tables shadow the loaded one.
//! * **Required guard for the caller (T11):** [`apply()`] must run on a session with no open user
//!   transaction. The driver API cannot report transaction state, so the caller must not apply
//!   while the query tab or session has an open user transaction as far as the app knows (a state
//!   flag in the workspace). Otherwise: `SQLite` fails `BEGIN` ("cannot start a transaction
//!   within a transaction") and nothing is sent after it; `PostgreSQL` only warns on `BEGIN`, so
//!   the plan's `COMMIT` commits (and a failed apply's `ROLLBACK` discards) the user's pending
//!   work; `MySQL`'s `START TRANSACTION` implicitly commits it.
//! * After [`ApplyOutcome::CommitUnknown`] the caller re-reads the data before any retry.

mod apply;
mod editability;
mod plan;
mod staged;
#[cfg(test)]
mod testing;

pub use apply::{ApplyFailure, ApplyOutcome, apply};
pub use editability::{
    EditRequest, EditSource, EditTarget, Editability, ReadOnlyReason, RowIdentity, check_editable,
};
pub use plan::{
    ApplyPlan, ConfirmationRequired, ConfirmedPlan, PlannedStatement, PreviewParam,
    PreviewStatement, PreviewWhen, display_value,
};
pub use staged::{ChangeKind, EditError, ModelRow, NewRowId, RowRef, StagedEdits};
