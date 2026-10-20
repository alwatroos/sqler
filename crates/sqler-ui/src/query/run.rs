// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The run state machine of one query tab.
//!
//! `idle -> [prompting] -> [confirming] -> running -> idle`. Nothing here touches a session or a
//! thread: the workspace asks [`QueryTab::request`], carries out the returned [`Step`] and feeds
//! the job's [`RunEvent`]s back in.

use std::time::Duration;

use sqler_sql::{Dialect, StatementClass};
use thiserror::Error;

use super::params::{BoundStatement, ParamError, ParamForm, ParamInput, bind};
use super::plan::Plan;
use crate::grid::group_thousands;

/// Identifies one run of a query tab; events of an older run are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId(u64);

impl RunId {
    /// A run ID from a counter value.
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The counter value.
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }

    /// The ID after this one.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// What a running job reports, in order. `index` is the [`BoundStatement::index`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RunEvent {
    /// The statement is being executed.
    Started {
        /// The statement.
        index: usize,
    },
    /// The statement finished and its result is complete.
    Finished {
        /// The statement.
        index: usize,
        /// Rows streamed into its grid.
        rows: usize,
        /// Rows affected, if the backend reports it.
        rows_affected: Option<u64>,
        /// Wall-clock time of the statement.
        duration: Duration,
    },
    /// The statement failed; the run stops.
    Failed {
        /// The statement.
        index: usize,
        /// The error text.
        message: String,
        /// Time until the failure.
        duration: Duration,
    },
    /// The statement was cancelled; the run stops.
    Cancelled {
        /// The statement.
        index: usize,
    },
}

/// Why a request was refused or a prompt rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RunError {
    /// A run, a prompt or a confirmation is already open.
    #[error("A query is already running or waiting for input")]
    Busy,
    /// There is no parameter prompt open.
    #[error("No parameter prompt is open")]
    NotPrompting,
    /// The parameter values cannot be bound; the prompt stays open.
    #[error(transparent)]
    Param(#[from] ParamError),
}

/// Where a tab is in a run.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Phase {
    /// Nothing is happening.
    Idle,
    /// The parameter prompt is open.
    Prompting(ParamForm),
    /// The prod confirmation is open; it lists these statements.
    Confirming(Vec<BoundStatement>),
    /// A job is running.
    Running {
        /// The run.
        run: RunId,
        /// Cancel was requested and the job has not ended yet.
        cancelling: bool,
    },
}

/// What the workspace does after a request or an answer.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Step {
    /// The parameter prompt is open ([`QueryTab::phase`] has the form).
    Prompt,
    /// The confirmation is open ([`QueryTab::phase`] has the statements).
    Confirm,
    /// Start the job with these statements, then call [`QueryTab::started`].
    Start(Vec<BoundStatement>),
}

/// How one statement went.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StatementStatus {
    /// Waiting for its turn.
    Pending,
    /// Executing.
    Running,
    /// Finished.
    Done {
        /// Rows in the grid.
        rows: usize,
        /// Rows affected, if the backend reports it.
        rows_affected: Option<u64>,
        /// Wall-clock time.
        duration: Duration,
    },
    /// Failed.
    Failed {
        /// The error text.
        message: String,
        /// Time until the failure.
        duration: Duration,
    },
    /// Cancelled.
    Cancelled,
    /// Skipped because an earlier statement failed or was cancelled.
    NotRun,
}

/// One statement of the last run and its outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultEntry {
    /// The statement's index in the run, from 0.
    pub index: usize,
    /// The label of the result strip, such as `#2 DELETE`.
    pub title: String,
    /// The exact SQL.
    pub sql: String,
    /// The outcome.
    pub status: StatementStatus,
}

impl ResultEntry {
    /// The status line: `1,024 rows · 12 ms`, `3 rows affected · 4 ms`, `Error: <msg>`,
    /// `Cancelled`, `Not run`, `Running…` or `Pending`.
    #[must_use]
    pub fn status_text(&self) -> String {
        match &self.status {
            StatementStatus::Pending => "Pending".to_owned(),
            StatementStatus::Running => "Running…".to_owned(),
            StatementStatus::Done {
                rows,
                rows_affected,
                duration,
            } => {
                let (n, what) = match rows_affected {
                    Some(n) if *rows == 0 => (usize::try_from(*n).unwrap_or(usize::MAX), true),
                    _ => (*rows, false),
                };
                let noun = if n == 1 { "row" } else { "rows" };
                let tail = if what { " affected" } else { "" };
                format!(
                    "{} {noun}{tail} · {}",
                    group_thousands(n),
                    format_duration(*duration)
                )
            }
            StatementStatus::Failed { message, .. } => format!("Error: {message}"),
            StatementStatus::Cancelled => "Cancelled".to_owned(),
            StatementStatus::NotRun => "Not run".to_owned(),
        }
    }
}

/// `12 ms` below one second, `1.2 s` from then on.
fn format_duration(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{} ms", d.as_millis())
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

/// The first keyword of `sql`, uppercase, skipping comments; the class name when there is none.
fn verb(sql: &str, class: StatementClass) -> String {
    let mut rest = sql;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    let word: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if word.is_empty() {
        class.as_str().to_uppercase()
    } else {
        word.to_uppercase()
    }
}

/// The run state of one query tab.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryTab {
    phase: Phase,
    pending: Option<Plan>,
    results: Vec<ResultEntry>,
}

impl Default for QueryTab {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryTab {
    /// An idle tab with no results.
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            pending: None,
            results: Vec::new(),
        }
    }

    /// Where the tab is in a run.
    #[must_use]
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The statements of the last (or current) run.
    #[must_use]
    pub fn results(&self) -> &[ResultEntry] {
        &self.results
    }

    /// Whether a job is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Running { .. })
    }

    /// The parameter form while the prompt is open, for editing.
    pub fn form_mut(&mut self) -> Option<&mut ParamForm> {
        match &mut self.phase {
            Phase::Prompting(form) => Some(form),
            _ => None,
        }
    }

    /// Asks to run `plan`. `prod` tells that the connection is labelled production.
    ///
    /// With parameters the prompt opens; otherwise the plan is bound at once. On prod, any
    /// statement not classified read-only opens the confirmation; else the run starts.
    ///
    /// # Errors
    /// [`RunError::Busy`] unless the tab is idle; [`RunError::Param`] if binding fails.
    pub fn request(
        &mut self,
        dialect: Option<Dialect>,
        plan: Plan,
        prod: bool,
    ) -> Result<Step, RunError> {
        if self.phase != Phase::Idle {
            return Err(RunError::Busy);
        }
        if plan.params.is_empty() {
            let bound = bind(dialect, &plan, &[])?;
            return Ok(self.after_bind(bound, prod));
        }
        self.phase = Phase::Prompting(ParamForm::new(&plan.params));
        self.pending = Some(plan);
        Ok(Step::Prompt)
    }

    /// Answers the parameter prompt with `inputs`.
    ///
    /// # Errors
    /// [`RunError::NotPrompting`] outside the prompt; [`RunError::Param`] when a value does not
    /// fit (the prompt stays open and the row shows the reason).
    pub fn submit(
        &mut self,
        dialect: Option<Dialect>,
        prod: bool,
        inputs: &[ParamInput],
    ) -> Result<Step, RunError> {
        let (Phase::Prompting(form), Some(plan)) = (&mut self.phase, &self.pending) else {
            return Err(RunError::NotPrompting);
        };
        for input in inputs {
            if let Some(i) = form.rows().iter().position(|r| r.key == input.key) {
                form.set_kind(i, input.kind);
                form.set_text(i, input.text.clone());
            }
        }
        match bind(dialect, plan, inputs) {
            Ok(bound) => {
                self.pending = None;
                Ok(self.after_bind(bound, prod))
            }
            Err(err) => {
                match &err {
                    ParamError::Invalid { key, reason } => form.set_error(key, reason.clone()),
                    ParamError::Missing(key) => form.set_error(key, err.to_string()),
                    ParamError::Parse(_) => {}
                }
                Err(err.into())
            }
        }
    }

    fn after_bind(&mut self, bound: Vec<BoundStatement>, prod: bool) -> Step {
        if prod && bound.iter().any(|s| !s.class.is_read_only()) {
            self.phase = Phase::Confirming(bound);
            Step::Confirm
        } else {
            self.phase = Phase::Idle;
            Step::Start(bound)
        }
    }

    /// Closes the parameter prompt or the confirmation; nothing runs. No-op otherwise.
    pub fn dismiss(&mut self) {
        if matches!(self.phase, Phase::Prompting(_) | Phase::Confirming(_)) {
            self.phase = Phase::Idle;
            self.pending = None;
        }
    }

    /// Accepts the confirmation and returns the statements to start; `None` outside it.
    pub fn confirm(&mut self) -> Option<Vec<BoundStatement>> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Confirming(bound) => Some(bound),
            other => {
                self.phase = other;
                None
            }
        }
    }

    /// The job for `run` starts: clears the old results and lists `statements` as pending.
    pub fn started(&mut self, run: RunId, statements: &[BoundStatement]) {
        self.pending = None;
        self.phase = Phase::Running {
            run,
            cancelling: false,
        };
        self.results = statements
            .iter()
            .map(|s| ResultEntry {
                index: s.index,
                title: format!("#{} {}", s.index + 1, verb(&s.sql, s.class)),
                sql: s.sql.clone(),
                status: StatementStatus::Pending,
            })
            .collect();
    }

    /// Cancel was requested for `run`. False if that run is not the current one.
    pub fn cancelling(&mut self, run: RunId) -> bool {
        match &mut self.phase {
            Phase::Running {
                run: current,
                cancelling,
            } if *current == run => {
                *cancelling = true;
                true
            }
            _ => false,
        }
    }

    /// Applies an event of `run`. False (and no change) if the run is stale or the index unknown.
    pub fn event(&mut self, run: RunId, event: RunEvent) -> bool {
        if !self.is_current(run) {
            return false;
        }
        let (index, status) = match event {
            RunEvent::Started { index } => (index, StatementStatus::Running),
            RunEvent::Finished {
                index,
                rows,
                rows_affected,
                duration,
            } => (
                index,
                StatementStatus::Done {
                    rows,
                    rows_affected,
                    duration,
                },
            ),
            RunEvent::Failed {
                index,
                message,
                duration,
            } => (index, StatementStatus::Failed { message, duration }),
            RunEvent::Cancelled { index } => (index, StatementStatus::Cancelled),
        };
        let Some(pos) = self.results.iter().position(|r| r.index == index) else {
            return false;
        };
        let stops = matches!(
            status,
            StatementStatus::Failed { .. } | StatementStatus::Cancelled
        );
        self.results[pos].status = status;
        if stops {
            for entry in &mut self.results[pos + 1..] {
                entry.status = StatementStatus::NotRun;
            }
        }
        true
    }

    /// The job of `run` ended and returned the session. False if the run is stale.
    pub fn finished(&mut self, run: RunId) -> bool {
        self.end(run)
    }

    /// The job of `run` was lost (no session came back). False if the run is stale.
    pub fn lost(&mut self, run: RunId) -> bool {
        self.end(run)
    }

    fn end(&mut self, run: RunId) -> bool {
        if !self.is_current(run) {
            return false;
        }
        for entry in &mut self.results {
            match entry.status {
                StatementStatus::Pending => entry.status = StatementStatus::NotRun,
                StatementStatus::Running => {
                    entry.status = StatementStatus::Failed {
                        message: "The run ended unexpectedly".to_owned(),
                        duration: Duration::ZERO,
                    };
                }
                _ => {}
            }
        }
        self.phase = Phase::Idle;
        true
    }

    fn is_current(&self, run: RunId) -> bool {
        matches!(self.phase, Phase::Running { run: current, .. } if current == run)
    }
}

#[cfg(test)]
mod tests {
    use super::super::buffer::{BufferSnapshot, RunMode};
    use super::super::params::{ParamKey, ParamKind};
    use super::super::plan::plan;
    use super::*;

    const D: Option<Dialect> = Some(Dialect::Postgres);

    fn planned(text: &str) -> Plan {
        plan(D, &BufferSnapshot::at_cursor(text, 0), RunMode::All).unwrap()
    }

    fn start(tab: &mut QueryTab, bound: &[BoundStatement]) -> RunId {
        let run = RunId::new(1);
        tab.started(run, bound);
        run
    }

    fn statuses(tab: &QueryTab) -> Vec<&StatementStatus> {
        tab.results().iter().map(|r| &r.status).collect()
    }

    fn dur(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn plain_run_starts_at_once() {
        let mut tab = QueryTab::new();
        let step = tab
            .request(D, planned("select 1; select 2"), false)
            .unwrap();
        let Step::Start(bound) = step else {
            panic!("{step:?}")
        };
        assert_eq!(bound.len(), 2);
        assert_eq!(tab.phase(), &Phase::Idle);
        let run = start(&mut tab, &bound);
        assert_eq!(
            tab.phase(),
            &Phase::Running {
                run,
                cancelling: false
            }
        );
        assert_eq!(tab.results().len(), 2);
        assert_eq!(tab.results()[0].title, "#1 SELECT");
        assert_eq!(statuses(&tab)[0], &StatementStatus::Pending);
    }

    #[test]
    fn a_second_request_is_busy_in_every_phase() {
        let mut tab = QueryTab::new();
        tab.request(D, planned("select :a"), false).unwrap();
        assert_eq!(
            tab.request(D, planned("select 1"), false),
            Err(RunError::Busy)
        );
        tab.dismiss();
        let Step::Start(bound) = tab.request(D, planned("select 1"), false).unwrap() else {
            panic!()
        };
        start(&mut tab, &bound);
        assert_eq!(
            tab.request(D, planned("select 1"), false),
            Err(RunError::Busy)
        );
        let mut tab = QueryTab::new();
        assert_eq!(
            tab.request(D, planned("drop table t"), true),
            Ok(Step::Confirm)
        );
        assert_eq!(
            tab.request(D, planned("select 1"), true),
            Err(RunError::Busy)
        );
    }

    #[test]
    fn dismissing_the_prompt_runs_nothing() {
        let mut tab = QueryTab::new();
        let step = tab.request(D, planned("select :a"), false).unwrap();
        assert_eq!(step, Step::Prompt);
        assert!(matches!(tab.phase(), Phase::Prompting(f) if f.rows().len() == 1));
        tab.dismiss();
        assert_eq!(tab.phase(), &Phase::Idle);
        assert_eq!(tab.results().len(), 0);
        assert_eq!(tab.submit(D, false, &[]), Err(RunError::NotPrompting));
    }

    #[test]
    fn prompt_then_start() {
        let mut tab = QueryTab::new();
        tab.request(D, planned("select :a"), false).unwrap();
        let inputs = [ParamInput {
            key: ParamKey::Named(":a".into()),
            kind: ParamKind::Auto,
            text: "5".into(),
        }];
        let Step::Start(bound) = tab.submit(D, false, &inputs).unwrap() else {
            panic!()
        };
        assert_eq!(bound[0].query.sql, "select $1");
        assert_eq!(tab.phase(), &Phase::Idle);
    }

    #[test]
    fn a_bad_value_keeps_the_prompt_open_with_the_reason() {
        let mut tab = QueryTab::new();
        tab.request(D, planned("select :a"), false).unwrap();
        let inputs = [ParamInput {
            key: ParamKey::Named(":a".into()),
            kind: ParamKind::Integer,
            text: "x".into(),
        }];
        let err = tab.submit(D, false, &inputs).unwrap_err();
        assert!(matches!(err, RunError::Param(ParamError::Invalid { .. })));
        let Phase::Prompting(form) = tab.phase() else {
            panic!()
        };
        assert!(form.rows()[0].error.is_some());
        assert_eq!(form.rows()[0].text, "x");
        let err = tab.submit(D, false, &[]).unwrap_err();
        assert!(matches!(err, RunError::Param(ParamError::Missing(_))));
    }

    #[test]
    fn a_missing_value_is_shown_on_its_row() {
        let mut tab = QueryTab::new();
        tab.request(D, planned("select :a"), false).unwrap();
        let err = tab.submit(D, false, &[]).unwrap_err();
        assert!(matches!(err, RunError::Param(ParamError::Missing(_))));
        let Phase::Prompting(form) = tab.phase() else {
            panic!()
        };
        assert!(form.rows()[0].error.is_some());
    }

    #[test]
    fn prod_confirms_everything_that_is_not_read() {
        for sql in [
            "insert into t values (1)",
            "create table t (a int)",
            "set search_path = x",
            "vacuum",
            "frobnicate",
        ] {
            let mut tab = QueryTab::new();
            let step = tab.request(D, planned(sql), true).unwrap();
            assert_eq!(step, Step::Confirm, "{sql}");
            let Phase::Confirming(list) = tab.phase() else {
                panic!()
            };
            assert_eq!(list[0].sql, sql);
            assert!(!list[0].class.is_read_only());
        }
    }

    #[test]
    fn prod_confirms_inside_run_all_and_lists_the_exact_sql() {
        let mut tab = QueryTab::new();
        let plan = planned("select 1; delete from t where id = :id; select 2");
        assert_eq!(tab.request(D, plan, true), Ok(Step::Prompt));
        let inputs = [ParamInput {
            key: ParamKey::Named(":id".into()),
            kind: ParamKind::Auto,
            text: "9".into(),
        }];
        assert_eq!(tab.submit(D, true, &inputs), Ok(Step::Confirm));
        let Phase::Confirming(list) = tab.phase() else {
            panic!()
        };
        assert_eq!(list.len(), 3);
        let writes: Vec<_> = list.iter().filter(|s| !s.class.is_read_only()).collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].sql, "delete from t where id = :id");
        assert_eq!(writes[0].values.len(), 1);
        let bound = tab.confirm().unwrap();
        assert_eq!(bound.len(), 3);
        assert_eq!(tab.phase(), &Phase::Idle);
        assert!(tab.confirm().is_none());
    }

    #[test]
    fn declining_the_confirmation_runs_nothing() {
        let mut tab = QueryTab::new();
        tab.request(D, planned("select 1; drop table t"), true)
            .unwrap();
        tab.dismiss();
        assert_eq!(tab.phase(), &Phase::Idle);
        assert_eq!(tab.results().len(), 0);
        assert!(tab.confirm().is_none());
    }

    #[test]
    fn prod_with_only_reads_does_not_confirm() {
        let mut tab = QueryTab::new();
        let step = tab.request(D, planned("select 1; select 2"), true).unwrap();
        assert!(matches!(step, Step::Start(_)));
    }

    #[test]
    fn non_prod_never_confirms() {
        let mut tab = QueryTab::new();
        let step = tab.request(D, planned("drop table t"), false).unwrap();
        assert!(matches!(step, Step::Start(_)));
    }

    #[test]
    fn no_dialect_is_unknown_and_confirms_on_prod() {
        let p = plan(
            None,
            &BufferSnapshot::at_cursor("select 1", 0),
            RunMode::All,
        )
        .unwrap();
        let mut tab = QueryTab::new();
        assert_eq!(tab.request(None, p, true), Ok(Step::Confirm));
    }

    #[test]
    fn events_fill_the_entries() {
        let mut tab = QueryTab::new();
        let Step::Start(bound) = tab
            .request(D, planned("select 1; delete from t; select 3"), false)
            .unwrap()
        else {
            panic!()
        };
        let run = start(&mut tab, &bound);
        assert_eq!(tab.results()[1].title, "#2 DELETE");
        assert!(tab.event(run, RunEvent::Started { index: 0 }));
        assert_eq!(statuses(&tab)[0], &StatementStatus::Running);
        tab.event(
            run,
            RunEvent::Finished {
                index: 0,
                rows: 1024,
                rows_affected: None,
                duration: dur(12),
            },
        );
        tab.event(
            run,
            RunEvent::Finished {
                index: 1,
                rows: 0,
                rows_affected: Some(3),
                duration: dur(4),
            },
        );
        assert_eq!(tab.results()[0].status_text(), "1,024 rows · 12 ms");
        assert_eq!(tab.results()[1].status_text(), "3 rows affected · 4 ms");
        assert_eq!(tab.results()[2].status_text(), "Pending");
        assert!(tab.finished(run));
        assert_eq!(tab.results()[2].status_text(), "Not run");
        assert_eq!(tab.phase(), &Phase::Idle);
    }

    #[test]
    fn stop_on_first_error_marks_the_rest_not_run() {
        let mut tab = QueryTab::new();
        let Step::Start(bound) = tab
            .request(D, planned("select 1; select 2; select 3"), false)
            .unwrap()
        else {
            panic!()
        };
        let run = start(&mut tab, &bound);
        tab.event(
            run,
            RunEvent::Finished {
                index: 0,
                rows: 1,
                rows_affected: None,
                duration: dur(1),
            },
        );
        tab.event(
            run,
            RunEvent::Failed {
                index: 1,
                message: "boom".into(),
                duration: dur(2),
            },
        );
        assert_eq!(tab.results()[1].status_text(), "Error: boom");
        assert_eq!(statuses(&tab)[2], &StatementStatus::NotRun);
        assert!(matches!(
            statuses(&tab)[0],
            StatementStatus::Done { rows: 1, .. }
        ));
    }

    #[test]
    fn cancel_marks_cancelled_and_not_run() {
        let mut tab = QueryTab::new();
        let Step::Start(bound) = tab
            .request(D, planned("select 1; select 2"), false)
            .unwrap()
        else {
            panic!()
        };
        let run = start(&mut tab, &bound);
        assert!(tab.cancelling(run));
        assert_eq!(
            tab.phase(),
            &Phase::Running {
                run,
                cancelling: true
            }
        );
        tab.event(run, RunEvent::Cancelled { index: 0 });
        assert_eq!(tab.results()[0].status_text(), "Cancelled");
        assert_eq!(tab.results()[1].status_text(), "Not run");
        assert!(tab.finished(run));
        assert!(!tab.is_running());
    }

    #[test]
    fn stale_run_ids_are_ignored() {
        let mut tab = QueryTab::new();
        let Step::Start(bound) = tab.request(D, planned("select 1"), false).unwrap() else {
            panic!()
        };
        let old = RunId::new(1);
        let run = RunId::new(2);
        tab.started(run, &bound);
        assert!(!tab.event(old, RunEvent::Started { index: 0 }));
        assert!(!tab.event(
            old,
            RunEvent::Failed {
                index: 0,
                message: "x".into(),
                duration: dur(0)
            }
        ));
        assert!(!tab.cancelling(old));
        assert!(!tab.finished(old));
        assert!(!tab.lost(old));
        assert_eq!(statuses(&tab)[0], &StatementStatus::Pending);
        assert!(tab.is_running());
        assert!(!tab.event(run, RunEvent::Started { index: 9 }));
        assert!(tab.finished(run));
        assert!(!tab.finished(run));
    }

    #[test]
    fn a_new_run_clears_old_results() {
        let mut tab = QueryTab::new();
        let Step::Start(bound) = tab
            .request(D, planned("select 1; select 2"), false)
            .unwrap()
        else {
            panic!()
        };
        let run = start(&mut tab, &bound);
        tab.finished(run);
        assert_eq!(tab.results().len(), 2);
        let Step::Start(bound) = tab.request(D, planned("select 9"), false).unwrap() else {
            panic!()
        };
        assert_eq!(
            tab.results().len(),
            2,
            "old results stay until the run starts"
        );
        tab.started(RunId::new(2), &bound);
        assert_eq!(tab.results().len(), 1);
    }

    #[test]
    fn lost_run_fails_the_running_entry() {
        let mut tab = QueryTab::new();
        let Step::Start(bound) = tab
            .request(D, planned("select 1; select 2"), false)
            .unwrap()
        else {
            panic!()
        };
        let run = start(&mut tab, &bound);
        tab.event(run, RunEvent::Started { index: 0 });
        assert!(tab.lost(run));
        assert!(tab.results()[0].status_text().starts_with("Error: "));
        assert_eq!(tab.results()[1].status_text(), "Not run");
        assert_eq!(tab.phase(), &Phase::Idle);
    }

    #[test]
    fn status_texts() {
        let entry = |status| ResultEntry {
            index: 0,
            title: String::new(),
            sql: String::new(),
            status,
        };
        let done = |rows, rows_affected, ms| {
            entry(StatementStatus::Done {
                rows,
                rows_affected,
                duration: dur(ms),
            })
            .status_text()
        };
        assert_eq!(done(1, None, 0), "1 row · 0 ms");
        assert_eq!(done(0, None, 5), "0 rows · 5 ms");
        assert_eq!(done(0, Some(1), 5), "1 row affected · 5 ms");
        assert_eq!(done(2, Some(2), 5), "2 rows · 5 ms");
        assert_eq!(done(3, None, 1500), "3 rows · 1.5 s");
        assert_eq!(entry(StatementStatus::Running).status_text(), "Running…");
    }

    #[test]
    fn titles_use_the_first_keyword() {
        assert_eq!(
            verb("  -- hi\n /* x */ delete from t", StatementClass::Write),
            "DELETE"
        );
        assert_eq!(verb("(select 1)", StatementClass::Read), "READ");
        assert_eq!(verb("-- only", StatementClass::Unknown), "UNKNOWN");
    }
}
