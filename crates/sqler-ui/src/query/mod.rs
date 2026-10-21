// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Running queries: from an editor buffer to result grids.
//!
//! Everything here is plain Rust and headless, so the editor view (T9) and the tests need no
//! `GPUI`. The pipeline:
//!
//! 1. [`plan()`] turns a [`BufferSnapshot`] (text, selection, cursor) and a [`RunMode`] into a
//!    [`Plan`], using the `sqler-sql` splitter, classifier and placeholder finder.
//! 2. [`QueryTab::request`] decides what the user must answer first: a parameter prompt
//!    ([`ParamForm`]), and on a production connection a confirmation for every statement not
//!    classified read-only. Declining either runs nothing.
//! 3. [`bind`] rewrites placeholders to the dialect's positional marker and keeps the values
//!    apart, in [`Query::params`](sqler_driver_api::sqler_core::Query). Values are never spliced
//!    into SQL.
//! 4. A [`QueryJob`] runs the [`BoundStatement`]s in order on the tab's session and streams each
//!    result into its own grid sink, reporting [`RunEvent`]s. It stops at the first error or
//!    cancellation and always hands the session back ([`QueryJobDone`]).
//! 5. [`QueryTab::event`] and [`QueryTab::finished`] fold the events into [`ResultEntry`]s.
//!
//! The workspace owns the lifecycle (lending the session, the cancel handles and the grids); a
//! driver is never imported here.
//!
//! # Usage
//!
//! The editor calls the workspace with a snapshot; the pure part looks like this:
//!
//! ```
//! use sqler_sql::Dialect;
//! use sqler_ui::query::{BufferSnapshot, QueryTab, RunMode, Step, plan};
//!
//! let buf = BufferSnapshot::at_cursor("select 1;\nselect 2;", 12);
//! let dialect = Some(Dialect::Sqlite);
//! let plan = plan(dialect, &buf, RunMode::Statement).unwrap();
//! assert_eq!(plan.statements[0].sql, "select 2");
//!
//! let mut tab = QueryTab::new();
//! let Step::Start(statements) = tab.request(dialect, plan, false).unwrap() else {
//!     unreachable!("no parameters, not prod");
//! };
//! assert_eq!(statements[0].query.sql, "select 2");
//! ```

mod action;
mod buffer;
mod explain;
mod job;
mod params;
mod plan;
mod run;
mod strip;
mod toolbar;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use action::{COMMANDS, QueryCommand, action_for, register_commands};
pub use buffer::{BufferSnapshot, RunMode};
pub use explain::{EXPLAIN_UNSUPPORTED, explain_for, explain_supported, explain_text};
pub use job::{QueryJob, QueryJobDone, RunStop};
pub use params::{
    BoundStatement, ParamError, ParamForm, ParamInput, ParamKey, ParamKind, ParamRow, bind,
    parse_value,
};
pub use plan::{Plan, PlanError, PlannedStatement, plan};
pub use run::{Phase, QueryTab, ResultEntry, RunError, RunEvent, RunId, StatementStatus, Step};
pub use strip::{StatusTone, status_tone, status_word};
pub use toolbar::{RunState, ToolbarContext, ToolbarItem, toolbar_tooltip};
