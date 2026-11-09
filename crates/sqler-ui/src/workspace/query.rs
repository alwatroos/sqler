// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Running queries inside the workspace: the run lifecycle of every query tab.
//!
//! Like the object browser this is state only. A run lends the tab's session to a [`QueryJob`]
//! (the tab has no session until the job returns it), keeps the session's cancel handle and the
//! cancel flags of the job and its grids, and returns an [`Effect`] the view carries out. The
//! decisions (parameter prompt, production confirmation, event bookkeeping) live in
//! [`QueryTab`].

use sqler_driver_api::CancelHandle;
use sqler_driver_api::sqler_core::Environment;
use sqler_sql::Dialect;
use tokio::sync::mpsc::unbounded_channel;

use super::state::{Effect, Purpose, Workspace};
use crate::connections::ConnectFailure;
use crate::grid::edit::SchemaProbe;
use crate::grid::{GridLimits, GridTab, grid_channel_with};
use crate::query::{
    BoundStatement, BufferSnapshot, EXPLAIN_UNSUPPORTED, ParamInput, ParamKind, Phase, QueryJob,
    QueryJobDone, QueryTab, RunError, RunEvent, RunId, RunMode, RunState, RunStop, Step,
    ToolbarContext, explain_for, explain_supported, plan,
};
use crate::tabs::{TabId, TabKind};

/// Shown when the run is started again after the connection came back.
pub(crate) const RESTORED_RETRY: &str = "Connection restored; running the query again.";
/// Shown when the connection came back but the query may have run, so the user decides.
pub(crate) const RESTORED_RUN_AGAIN: &str = "Connection lost and restored; run again.";
/// Shown when the lost connection had an open transaction.
pub(crate) const RESTORED_TX_LOST: &str = "Connection lost and restored; the open transaction was lost and its uncommitted changes are gone. Run again.";
/// Shown when the re-run lost the connection too (one reconnect per run).
pub(crate) const LOST_AGAIN: &str =
    "The connection was lost again; run the query again to reconnect.";
/// Shown when a run is requested while the reconnect is still going.
pub(crate) const RECONNECTING: &str = "The connection is being restored; wait a moment.";
/// Shown when no reconnect could be started.
const LOST_NO_RECONNECT: &str = "Connection lost; open the connection again and run again.";
/// Shown when the user cancels while the reconnect still runs.
const RETRY_DROPPED: &str = "The query will not run again; the connection is still being restored.";

/// A run that lost its connection and waits for the reconnect of its tab.
pub(super) struct AutoRetry {
    /// The statements to run again; `None` when the user has to run again.
    pub(super) statements: Option<Vec<BoundStatement>>,
    /// The notice to show when the connection is back.
    pub(super) notice: &'static str,
}

/// What to do after a run lost its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// The user cancelled: nothing extra.
    Nothing,
    /// Show the notice; no reconnect.
    Notice(&'static str),
    /// Reconnect, then show the notice, or run again when `rerun`.
    Reconnect { rerun: bool, notice: &'static str },
}

/// Decides what follows a lost connection at statement `pos` of a run.
///
/// Only the first statement can run again, so nothing else ran on the dead session in this run.
#[allow(clippy::fn_params_excessive_bools)] // a pure truth table, named at the call
fn decide(
    pos: usize,
    sent: bool,
    read_only: bool,
    in_tx: bool,
    retry: bool,
    cancelled: bool,
) -> Decision {
    if cancelled {
        Decision::Nothing
    } else if retry {
        Decision::Notice(LOST_AGAIN)
    } else if in_tx {
        Decision::Reconnect {
            rerun: false,
            notice: RESTORED_TX_LOST,
        }
    } else if pos == 0 && (!sent || read_only) {
        Decision::Reconnect {
            rerun: true,
            notice: RESTORED_RETRY,
        }
    } else {
        Decision::Reconnect {
            rerun: false,
            notice: RESTORED_RUN_AGAIN,
        }
    }
}

/// A run in progress: what it takes to stop it.
pub(super) struct ActiveRun {
    run: RunId,
    handle: CancelHandle,
    stop: RunStop,
    /// The statements of the run, for a re-run after a reconnect.
    statements: Vec<BoundStatement>,
    /// Whether the session was inside a transaction when the run started.
    in_tx: bool,
    /// Whether this run is the automatic re-run.
    retry: bool,
}

impl ActiveRun {
    /// Stops the run between statements and ends the ingestion of every grid, waking one that is
    /// paused on a full page.
    fn raise_flags(&self) {
        self.stop.cancel();
    }
}

impl Workspace {
    /// The run state of a query tab.
    #[must_use]
    pub fn query_tab(&self, tab: TabId) -> Option<&QueryTab> {
        self.queries.get(&tab)
    }

    fn query_notice(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    /// The dialect of the tab's connection and whether it is labelled production.
    fn query_context(&self, tab: TabId) -> Option<(Option<Dialect>, bool)> {
        let connection = self.tabs.get(tab)?.connection();
        Some((
            self.dialect_of(&connection.driver),
            connection.environment == Some(Environment::Prod),
        ))
    }

    /// Whether `run` is the run in progress on the tab.
    pub(super) fn is_current_run(&self, tab: TabId, run: RunId) -> bool {
        self.query_runs.get(&tab).is_some_and(|a| a.run == run)
    }

    /// Runs part of the editor buffer of a query tab.
    ///
    /// Depending on the buffer and the connection this opens the parameter prompt, opens the
    /// production confirmation, or starts the run at once. A tab that is already running,
    /// prompting or confirming refuses the request with a notice. A selection runs exactly its
    /// own text; without one, `Statement` runs the statement at the cursor up to its `;`.
    #[allow(clippy::needless_pass_by_value)] // the editor hands its snapshot over
    pub fn run_query(&mut self, tab: TabId, mode: RunMode, buf: BufferSnapshot) -> Effect {
        // A run asked for by the user is never the automatic re-run.
        self.retry_marks.remove(&tab);
        let Some((dialect, prod)) = self.query_context(tab) else {
            return Effect::None;
        };
        if self.tabs.get(tab).map(crate::tabs::Tab::kind) != Some(TabKind::Query) {
            return self.query_notice("Open a query tab first.");
        }
        if self.auto_retry_pending(tab) {
            return self.query_notice(RECONNECTING);
        }
        // Pending edits belong to the grids of the last run; a new run would replace them.
        if let Some(message) = self.run_blocked_by_edits(tab) {
            return self.query_notice(message);
        }
        let Some(query) = self.queries.get_mut(&tab) else {
            return Effect::None;
        };
        if *query.phase() != Phase::Idle {
            return self.query_notice(RunError::Busy.to_string());
        }
        if !self.has_session(tab) {
            return self.query_notice("The connection of this tab is not available.");
        }
        let plan = match plan(dialect, &buf, mode) {
            Ok(plan) => plan,
            Err(err) => return self.query_notice(err.to_string()),
        };
        let Some(query) = self.queries.get_mut(&tab) else {
            return Effect::None;
        };
        match query.request(dialect, plan, prod) {
            Ok(Step::Start(statements)) => self.start_query(tab, &statements),
            Ok(_) => Effect::None,
            Err(err) => self.query_notice(err.to_string()),
        }
    }

    /// Explains the statement at the cursor: wraps it with the driver's explain prefix and hands
    /// the text to [`Workspace::run_query`], so the parameter prompt and the production gate
    /// apply as for any run. `ANALYZE` is never added.
    #[allow(clippy::needless_pass_by_value)] // the editor hands its snapshot over
    pub fn explain_query(&mut self, tab: TabId, buf: BufferSnapshot) -> Effect {
        let Some((dialect, _)) = self.query_context(tab) else {
            return Effect::None;
        };
        if self.tabs.get(tab).map(crate::tabs::Tab::kind) != Some(TabKind::Query) {
            return self.query_notice("Open a query tab first.");
        }
        if self
            .queries
            .get(&tab)
            .is_some_and(|q| *q.phase() != Phase::Idle)
        {
            return self.query_notice(RunError::Busy.to_string());
        }
        // Explain always takes the statement at the cursor, whatever is selected.
        let buf = BufferSnapshot {
            selection: buf.cursor..buf.cursor,
            ..buf
        };
        let plan = match plan(dialect, &buf, RunMode::Statement) {
            Ok(plan) => plan,
            Err(err) => return self.query_notice(err.to_string()),
        };
        let Some(statement) = plan.statements.first() else {
            return self.query_notice("No statement to run");
        };
        let driver = self
            .tabs
            .get(tab)
            .map(|t| t.connection().driver.as_str().to_owned())
            .unwrap_or_default();
        match explain_for(&driver, dialect, &statement.sql) {
            Some(text) => self.run_query(tab, RunMode::All, BufferSnapshot::at_cursor(text, 0)),
            None => self.query_notice(EXPLAIN_UNSUPPORTED),
        }
    }

    /// What decides which toolbar items of a query tab are enabled; `None` for other tabs.
    #[must_use]
    pub fn query_toolbar(&self, tab: TabId) -> Option<ToolbarContext> {
        let query = self.queries.get(&tab)?;
        if self.tabs.get(tab)?.kind() != TabKind::Query {
            return None;
        }
        let run = match query.phase() {
            Phase::Idle => RunState::Idle,
            Phase::Prompting(_) | Phase::Confirming(_) => RunState::Busy,
            Phase::Running {
                cancelling: true, ..
            } => RunState::Cancelling,
            _ => RunState::Running,
        };
        let connection = self.tabs.get(tab)?.connection();
        Some(ToolbarContext {
            run,
            connected: self.has_session(tab),
            explain: explain_supported(
                connection.driver.as_str(),
                Dialect::from_driver_id(connection.driver.as_str()),
            ),
            ai: self.ai_toolbar(tab),
        })
    }

    /// Answers the parameter prompt of a query tab with the values the user typed.
    ///
    /// A value that does not fit keeps the prompt open and shows the reason on its row.
    pub fn submit_query_params(&mut self, tab: TabId, inputs: &[ParamInput]) -> Effect {
        let Some((dialect, prod)) = self.query_context(tab) else {
            return Effect::None;
        };
        let Some(query) = self.queries.get_mut(&tab) else {
            return Effect::None;
        };
        match query.submit(dialect, prod, inputs) {
            Ok(Step::Start(statements)) => self.start_query(tab, &statements),
            Ok(_) | Err(RunError::Param(_)) => Effect::None,
            Err(err) => self.query_notice(err.to_string()),
        }
    }

    /// Changes how the text of parameter `index` is read in the open prompt.
    pub fn set_query_param_kind(&mut self, tab: TabId, index: usize, kind: ParamKind) {
        if let Some(form) = self.queries.get_mut(&tab).and_then(QueryTab::form_mut) {
            form.set_kind(index, kind);
        }
    }

    /// Closes the parameter prompt or the confirmation of a query tab; nothing runs.
    pub fn dismiss_query_prompt(&mut self, tab: TabId) {
        if self.approvals_mut(tab).is_some() {
            return self.answer_prod_gate(tab, sqler_ai_tools::Decision::Deny);
        }
        if let Some(query) = self.queries.get_mut(&tab) {
            query.dismiss();
        }
    }

    /// Accepts the production confirmation of a query tab and starts the run.
    pub fn confirm_query(&mut self, tab: TabId) -> Effect {
        if self.approvals_mut(tab).is_some() {
            self.answer_prod_gate(tab, sqler_ai_tools::Decision::Approve);
            return Effect::None;
        }
        let Some(query) = self.queries.get_mut(&tab) else {
            return Effect::None;
        };
        match query.confirm() {
            Some(statements) => self.start_query(tab, &statements),
            None => Effect::None,
        }
    }

    /// Lends the tab's session to a new job.
    fn start_query(&mut self, tab: TabId, statements: &[BoundStatement]) -> Effect {
        // A prompt or confirmation can stay open while edits are staged on the old grids.
        if let Some(message) = self.run_blocked_by_edits(tab) {
            return self.query_notice(message);
        }
        let Some(session) = self.take_session(tab) else {
            return self.query_notice("The connection of this tab is not available.");
        };
        let run = RunId::new(self.next_run);
        self.next_run += 1;
        let limits = GridLimits::from_settings(self.settings().settings());
        let mut sinks = Vec::with_capacity(statements.len());
        let mut grids = Vec::with_capacity(statements.len());
        for _ in statements {
            let (model, sink) = grid_channel_with(limits);
            grids.push(GridTab::new(model));
            sinks.push(sink);
        }
        // One lookup per statement that reads a single unqualified table, run on the run's session
        // right after it, so the grid can tell which schema that name means (ADR-0033).
        let dialect = self.query_context(tab).and_then(|(dialect, _)| dialect);
        let probes: Vec<Option<SchemaProbe>> = dialect.map_or_else(Vec::new, |d| {
            statements
                .iter()
                .map(|s| SchemaProbe::for_sql(d, &s.sql))
                .collect()
        });
        let active = ActiveRun {
            run,
            handle: session.cancel_handle(),
            stop: RunStop::new(&sinks),
            statements: statements.to_vec(),
            in_tx: self.in_transaction(tab),
            retry: self.retry_marks.remove(&tab),
        };
        let (events, receiver) = unbounded_channel();
        let job = QueryJob {
            tab,
            run,
            session,
            statements: statements.to_vec(),
            sinks,
            stop: active.stop.clone(),
            events,
            probes,
        };
        self.edit.run_started(tab);
        if let Some(query) = self.queries.get_mut(&tab) {
            query.started(run, statements);
        }
        self.query_runs.insert(tab, active);
        self.layout
            .reveal_panel(crate::workbench::PanelView::Results);
        Effect::RunQuery {
            job,
            grids,
            events: receiver,
        }
    }

    /// Cancels the run of a query tab, or closes its prompt or confirmation.
    pub fn cancel_query(&mut self, tab: TabId) -> Effect {
        if self.is_applying(tab) {
            return self.cancel_apply(tab);
        }
        let Some(query) = self.queries.get_mut(&tab) else {
            return Effect::None;
        };
        match query.phase() {
            Phase::Prompting(_) | Phase::Confirming(_) => {
                query.dismiss();
                Effect::None
            }
            Phase::Running { .. } => match self.query_runs.get(&tab) {
                Some(active) => {
                    active.raise_flags();
                    let (run, handle) = (active.run, active.handle.clone());
                    if let Some(query) = self.queries.get_mut(&tab) {
                        query.cancelling(run);
                    }
                    Effect::CancelQuery(handle)
                }
                None => Effect::None,
            },
            _ => {
                if self.auto_retry_pending(tab) {
                    self.auto_retries.remove(&tab);
                    return self.query_notice(RETRY_DROPPED);
                }
                self.query_notice("No query is running.")
            }
        }
    }

    /// Whether a re-run waits for the reconnect of the tab. An entry without a running reconnect
    /// is stale and dropped.
    fn auto_retry_pending(&mut self, tab: TabId) -> bool {
        if !self.auto_retries.contains_key(&tab) {
            return false;
        }
        if self.reconnect_running(tab) {
            return true;
        }
        self.auto_retries.remove(&tab);
        false
    }

    /// Whether a reconnect request of the tab is in flight.
    fn reconnect_running(&self, tab: TabId) -> bool {
        self.requests
            .values()
            .any(|p| matches!(p, Purpose::Reconnect { tab: t, .. } if *t == tab))
    }

    /// What follows a run whose statement `pos` lost the connection. The dead session is already
    /// back in the tab.
    fn after_lost_run(&mut self, tab: TabId, active: ActiveRun, pos: usize, sent: bool) -> Effect {
        let read_only = active
            .statements
            .first()
            .is_some_and(|s| s.class.is_read_only());
        let decision = decide(
            pos,
            sent,
            read_only,
            active.in_tx || self.in_transaction(tab),
            active.retry,
            active.stop.is_cancelled(),
        );
        match decision {
            Decision::Nothing => Effect::None,
            Decision::Notice(text) => self.query_notice(text),
            Decision::Reconnect { rerun, notice } => {
                let effect = self.reconnect_tab(tab);
                if !self.reconnect_running(tab) {
                    return self.query_notice(LOST_NO_RECONNECT);
                }
                self.auto_retries.insert(
                    tab,
                    AutoRetry {
                        statements: rerun.then_some(active.statements),
                        notice,
                    },
                );
                effect
            }
        }
    }

    /// The reconnect of the tab succeeded: shows the notice and runs the statements again when the
    /// lost run asked for it. On production a write opens the confirmation again.
    pub(super) fn auto_retry_restored(&mut self, tab: TabId) -> Effect {
        let Some(entry) = self.auto_retries.remove(&tab) else {
            return Effect::None;
        };
        self.notice = Some(entry.notice.to_owned());
        let Some(statements) = entry.statements else {
            return Effect::None;
        };
        let prod = self.production(tab);
        let Some(query) = self.queries.get_mut(&tab) else {
            return Effect::None;
        };
        self.retry_marks.insert(tab);
        match query.retry(statements, prod) {
            Ok(Step::Start(statements)) => self.start_query(tab, &statements),
            Ok(_) => Effect::None,
            Err(err) => {
                self.retry_marks.remove(&tab);
                self.query_notice(err.to_string())
            }
        }
    }

    /// The reconnect of the tab failed: the run is not repeated and the tab keeps its dead
    /// session. `failure` is `None` when a prompt shows the reason itself.
    pub(super) fn auto_retry_failed(&mut self, tab: TabId, failure: Option<&ConnectFailure>) {
        if self.auto_retries.remove(&tab).is_some()
            && let Some(failure) = failure
        {
            self.notice = Some(format!("Could not restore the connection: {failure}"));
        }
    }

    /// Stops the run of a tab that is going away; returns the handle to ask the database with.
    pub(super) fn cancel_query_run(&mut self, tab: TabId) -> Option<CancelHandle> {
        let active = self.query_runs.remove(&tab)?;
        active.raise_flags();
        Some(active.handle)
    }

    /// Stops every run (the app is quitting); returns the handles to ask the databases with.
    pub fn cancel_all_queries(&mut self) -> Vec<CancelHandle> {
        self.query_runs
            .drain()
            .map(|(_, active)| {
                active.raise_flags();
                active.handle
            })
            .collect()
    }

    /// Applies one progress event of a run; events of an older run are ignored.
    ///
    /// A statement that ended (finished, failed or cancelled) returns [`Effect::RecordHistory`]
    /// for the query history; every other event returns [`Effect::None`].
    #[allow(clippy::needless_pass_by_value)] // the view hands each event over
    pub fn query_event(&mut self, tab: TabId, run: RunId, event: RunEvent) -> Effect {
        if !self.queries.contains_key(&tab) {
            return Effect::None;
        }
        // The schema lookup is not a status: the workspace keeps it for the editing flow.
        if let RunEvent::Resolved { index, schema } = event {
            self.keep_resolved(tab, run, index, schema);
            return Effect::None;
        }
        let before = self.statement_before(tab, &event);
        let Some(query) = self.queries.get_mut(&tab) else {
            return Effect::None;
        };
        if !query.event(run, event.clone()) {
            return Effect::None;
        }
        self.follow_transaction(tab, &event, before);
        self.record_event(tab, &event)
    }

    /// The job ended and returned the session, which goes back to its tab (or is closed when the
    /// tab is gone).
    pub fn query_finished(&mut self, done: QueryJobDone) -> Effect {
        let QueryJobDone {
            tab,
            run,
            session,
            lost_at,
            lost_sent,
        } = done;
        let active = if self.query_runs.get(&tab).is_some_and(|a| a.run == run) {
            self.query_runs.remove(&tab)
        } else {
            None
        };
        match self.queries.get_mut(&tab) {
            Some(query) => {
                query.finished(run);
                self.restore_session(tab, session);
                match (active, lost_at) {
                    (Some(active), Some(pos)) => self.after_lost_run(tab, active, pos, lost_sent),
                    _ => Effect::None,
                }
            }
            None => Effect::CloseSession(session),
        }
    }

    /// The job ended without returning the session (its task panicked): the tab loses its
    /// connection.
    pub fn query_lost(&mut self, tab: TabId, run: RunId) {
        if self.query_runs.get(&tab).is_some_and(|a| a.run == run) {
            self.query_runs.remove(&tab);
        }
        self.edit.session_gone(tab);
        if let Some(query) = self.queries.get_mut(&tab)
            && query.lost(run)
        {
            self.notice =
                Some("The query ended unexpectedly; open the connection again.".to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

    use super::*;
    use crate::sidebar::SidebarModel;
    use crate::workspace::WorkspaceAction;
    use crate::workspace::state::RequestId;
    use crate::workspace::testing::FakeSession;

    fn ws() -> Workspace {
        let driver = DriverId::new("sqlite").unwrap();
        Workspace::new(SidebarModel::from_connections(vec![
            ConnectionConfig::new("dev", driver.clone(), ":memory:")
                .with_environment(Environment::Dev),
            ConnectionConfig::new("prod", driver, ":memory:").with_environment(Environment::Prod),
            ConnectionConfig::new("ms", DriverId::new("mssql").unwrap(), "x"),
        ]))
    }

    fn open(
        w: &mut Workspace,
        index: usize,
    ) -> (TabId, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let Effect::Connect { request, .. } = w.dispatch(WorkspaceAction::OpenConnection(index))
        else {
            panic!("expected Connect");
        };
        let request: RequestId = request;
        let (session, closed) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        (w.tabs().active().map(crate::tabs::Tab::id).unwrap(), closed)
    }

    fn buf(text: &str) -> BufferSnapshot {
        BufferSnapshot::at_cursor(text, 0)
    }

    fn start(w: &mut Workspace, tab: TabId, text: &str) -> (QueryJob, Vec<GridTab>) {
        match w.run_query(tab, RunMode::All, buf(text)) {
            Effect::RunQuery { job, grids, .. } => (job, grids),
            other => panic!("expected RunQuery, got {other:?}"),
        }
    }

    fn run_job(job: QueryJob) -> QueryJobDone {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(job.run())
    }

    #[test]
    fn session_is_lent_and_returned() {
        let mut w = ws();
        let (tab, closed) = open(&mut w, 0);
        assert!(w.has_session(tab));
        let (job, grids) = start(&mut w, tab, "select 1; select 2");
        assert_eq!(grids.len(), 2);
        assert!(!w.has_session(tab));
        assert!(w.query_tab(tab).unwrap().is_running());
        let done = run_job(job);
        assert!(matches!(w.query_finished(done), Effect::None));
        assert!(w.has_session(tab));
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
        assert!(!closed.load(Ordering::SeqCst));
    }

    #[test]
    fn second_run_is_refused_while_running() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        let _first = start(&mut w, tab, "select 1");
        assert!(matches!(
            w.run_query(tab, RunMode::All, buf("select 2")),
            Effect::None
        ));
        let notice = w.take_notice().unwrap();
        assert!(notice.contains("already running"), "{notice}");
    }

    #[test]
    fn closing_the_tab_mid_run_closes_the_returned_session() {
        let mut w = ws();
        let (tab, closed) = open(&mut w, 0);
        let (job, _grids) = start(&mut w, tab, "select 1");
        let stop = job.stop.clone();
        let sink_flag = job.sinks[0].cancel_flag();
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseTab(tab)),
            Effect::CancelQuery(_)
        ));
        assert!(stop.is_cancelled());
        assert!(sink_flag.is_cancelled());
        let done = run_job(job);
        let Effect::CloseSession(session) = w.query_finished(done) else {
            panic!("expected CloseSession");
        };
        assert!(!w.has_session(tab));
        drop(
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(session.close()),
        );
        assert!(closed.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_emits_cancel_query_and_marks_cancelling() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        let (job, _grids) = start(&mut w, tab, "select 1");
        assert!(matches!(w.cancel_query(tab), Effect::CancelQuery(_)));
        assert!(job.stop.is_cancelled());
        assert_eq!(
            *w.query_tab(tab).unwrap().phase(),
            Phase::Running {
                run: job.run,
                cancelling: true
            }
        );
    }

    #[test]
    fn cancel_without_run_only_notifies() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        assert!(matches!(w.cancel_query(tab), Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some("No query is running."));
    }

    #[test]
    fn prod_confirms_a_write_inside_run_all() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 1);
        let effect = w.run_query(tab, RunMode::All, buf("select 1; delete from t"));
        assert!(matches!(effect, Effect::None));
        assert!(w.has_session(tab));
        let Phase::Confirming(statements) = w.query_tab(tab).unwrap().phase() else {
            panic!("expected a confirmation");
        };
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[1].sql, "delete from t");
        // Declining runs nothing, not even the read.
        w.dismiss_query_prompt(tab);
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
        assert!(w.has_session(tab));
        w.run_query(tab, RunMode::All, buf("select 1; delete from t"));
        assert!(matches!(w.confirm_query(tab), Effect::RunQuery { .. }));
    }

    #[test]
    fn prod_read_only_runs_without_confirmation() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 1);
        assert!(matches!(
            w.run_query(tab, RunMode::All, buf("select 1")),
            Effect::RunQuery { .. }
        ));
    }

    #[test]
    fn parameters_prompt_then_run() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        assert!(matches!(
            w.run_query(tab, RunMode::All, buf("select :id")),
            Effect::None
        ));
        let Phase::Prompting(form) = w.query_tab(tab).unwrap().phase() else {
            panic!("expected a prompt");
        };
        let mut inputs = form.inputs();
        inputs[0].text = "7".to_owned();
        assert!(matches!(
            w.submit_query_params(tab, &inputs),
            Effect::RunQuery { .. }
        ));
    }

    #[test]
    fn dismissed_prompt_runs_nothing() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        w.run_query(tab, RunMode::All, buf("select :id"));
        w.dismiss_query_prompt(tab);
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
        assert!(w.has_session(tab));
    }

    #[test]
    fn empty_buffer_notifies() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        assert!(matches!(
            w.run_query(tab, RunMode::All, buf("  ")),
            Effect::None
        ));
        assert_eq!(w.take_notice().as_deref(), Some("No statement to run"));
    }

    #[test]
    fn quit_cancels_a_running_query() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        let (job, _grids) = start(&mut w, tab, "select 1");
        let Effect::Batch(effects) = w.dispatch(WorkspaceAction::Quit) else {
            panic!("expected Batch");
        };
        assert!(job.stop.is_cancelled());
        assert!(matches!(effects[0], Effect::CancelQuery(_)));
        assert!(matches!(effects[1], Effect::CloseSessions(_)));
    }

    #[test]
    fn lost_job_frees_the_tab() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        let (job, _grids) = start(&mut w, tab, "select 1");
        w.query_lost(tab, job.run);
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
        assert!(w.take_notice().is_some());
        assert!(!w.has_session(tab));
    }

    #[test]
    fn cancel_wakes_a_paused_job_and_the_page_comes_from_settings() {
        use crate::grid::{IngestState, SyntheticSource, spawn_ingest};
        use crate::settings::SettingsModel;

        let dir = std::env::temp_dir().join(format!("sqler-ui-query-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.toml"), "result_page_rows = 1000\n").unwrap();
        let settings = SettingsModel::load(Ok(sqler_config::ConfigDir::new(&dir)));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(settings.settings().result_page_rows, 1000);

        let mut w = ws().with_settings(settings);
        let (tab, _) = open(&mut w, 0);
        let (mut job, mut grids) = start(&mut w, tab, "select 1");
        // Feed the sink with a source bigger than one page, on its own thread.
        let sink = job.sinks.remove(0);
        let handle = spawn_ingest(SyntheticSource::new(5_000, 2, 256), sink).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !matches!(grids[0].state(), IngestState::Paused) {
            assert!(std::time::Instant::now() < deadline, "never paused");
            grids[0].poll(usize::MAX);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(grids[0].row_count(), 1000);
        assert!(!handle.is_finished());
        // Cancel raises the GridCancel, which wakes the paused ingestion.
        assert!(matches!(w.cancel_query(tab), Effect::CancelQuery(_)));
        assert!(matches!(
            handle.join().unwrap(),
            Err(sqler_driver_api::DriverError::Cancelled)
        ));
    }

    fn sql_of(effect: Effect) -> String {
        match effect {
            Effect::RunQuery { job, .. } => job.statements[0].query.sql.clone(),
            other => panic!("expected RunQuery, got {other:?}"),
        }
    }

    #[test]
    fn explain_wraps_the_statement_at_the_cursor() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        let snapshot = BufferSnapshot::at_cursor("select 1;\nselect 2;", 12);
        let effect = w.explain_query(tab, snapshot);
        assert_eq!(sql_of(effect), "EXPLAIN QUERY PLAN select 2");
    }

    #[test]
    fn explain_of_a_prod_write_confirms_the_wrapped_text() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 1);
        let effect = w.explain_query(tab, buf("delete from t"));
        assert!(matches!(effect, Effect::None));
        let Phase::Confirming(statements) = w.query_tab(tab).unwrap().phase() else {
            panic!("expected a confirmation");
        };
        assert_eq!(statements[0].sql, "EXPLAIN QUERY PLAN delete from t");
        w.dismiss_query_prompt(tab);
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
        assert!(w.has_session(tab));
        w.explain_query(tab, buf("delete from t"));
        assert!(matches!(w.confirm_query(tab), Effect::RunQuery { .. }));
    }

    #[test]
    fn explain_while_busy_reports_busy_before_planning() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        let _first = start(&mut w, tab, "select 1");
        let effect = w.explain_query(tab, buf("select from where ("));
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.take_notice().unwrap(), RunError::Busy.to_string());
    }

    #[test]
    fn explain_of_a_prod_read_runs_at_once() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 1);
        assert_eq!(
            sql_of(w.explain_query(tab, buf("select 1"))),
            "EXPLAIN QUERY PLAN select 1"
        );
    }

    #[test]
    fn explain_is_unsupported_on_mssql() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 2);
        assert!(matches!(
            w.explain_query(tab, buf("select 1")),
            Effect::None
        ));
        assert_eq!(w.take_notice().as_deref(), Some(EXPLAIN_UNSUPPORTED));
        assert!(!w.query_toolbar(tab).unwrap().explain);
    }

    #[test]
    fn explain_is_refused_while_busy_and_on_empty_text() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        assert!(matches!(w.explain_query(tab, buf("  ")), Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some("No statement to run"));
        let _first = start(&mut w, tab, "select 1");
        assert!(matches!(
            w.explain_query(tab, buf("select 2")),
            Effect::None
        ));
        let notice = w.take_notice().unwrap();
        assert!(notice.contains("already running"), "{notice}");
    }

    #[test]
    fn toolbar_follows_the_run() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 0);
        let c = w.query_toolbar(tab).unwrap();
        assert_eq!(c.run, RunState::Idle);
        assert!(c.connected && c.explain);
        let _first = start(&mut w, tab, "select 1");
        let c = w.query_toolbar(tab).unwrap();
        assert_eq!(c.run, RunState::Running);
        assert!(!c.connected);
        w.cancel_query(tab);
        assert_eq!(w.query_toolbar(tab).unwrap().run, RunState::Cancelling);
    }

    #[test]
    fn toolbar_is_busy_while_confirming() {
        let mut w = ws();
        let (tab, _) = open(&mut w, 1);
        w.run_query(tab, RunMode::All, buf("delete from t"));
        assert_eq!(w.query_toolbar(tab).unwrap().run, RunState::Busy);
    }

    // ---- lost connection and automatic reconnect ----

    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use sqler_driver_api::{
        BoxFuture, CancelFlag, DriverError, QueryEvent, QueryStream, Session, sqler_core::Query,
    };

    use crate::connections::{ConnectFailure, ConnectionAction as A, HostKeyPrompt};

    struct Events(VecDeque<Result<QueryEvent, DriverError>>);

    impl QueryStream for Events {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            Box::pin(std::future::ready(self.0.pop_front()))
        }
    }

    /// A session that answers the first `ok` statements and loses the connection after that,
    /// either when executing or in the stream.
    struct LostSession {
        ok: usize,
        calls: usize,
        sent: bool,
        in_stream: bool,
        closed: Arc<AtomicBool>,
    }

    impl LostSession {
        fn boxed(ok: usize, sent: bool) -> (Box<dyn Session>, Arc<AtomicBool>) {
            Self::build(ok, sent, false)
        }

        fn build(ok: usize, sent: bool, in_stream: bool) -> (Box<dyn Session>, Arc<AtomicBool>) {
            let closed = Arc::new(AtomicBool::new(false));
            let session = Self {
                ok,
                calls: 0,
                sent,
                in_stream,
                closed: closed.clone(),
            };
            (Box::new(session), closed)
        }

        fn lost(&self) -> DriverError {
            DriverError::ConnectionLost {
                message: "gone".into(),
                sent: self.sent,
            }
        }
    }

    impl Session for LostSession {
        fn cancel_handle(&self) -> CancelHandle {
            CancelFlag::new().handle()
        }

        fn execute<'s>(
            &'s mut self,
            _query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            self.calls += 1;
            let result: Result<Box<dyn QueryStream + 's>, DriverError> = if self.calls <= self.ok {
                Ok(Box::new(Events(VecDeque::from([
                    Ok(QueryEvent::Columns(Vec::new())),
                    Ok(QueryEvent::Done {
                        rows_affected: Some(0),
                    }),
                ]))))
            } else if self.in_stream {
                Ok(Box::new(Events(VecDeque::from([
                    Ok(QueryEvent::Columns(Vec::new())),
                    Err(self.lost()),
                ]))))
            } else {
                Err(self.lost())
            };
            Box::pin(std::future::ready(result))
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            self.closed.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    fn open_with(w: &mut Workspace, index: usize, session: Box<dyn Session>) -> TabId {
        let Effect::Connect { request, .. } = w.dispatch(WorkspaceAction::OpenConnection(index))
        else {
            panic!("expected Connect");
        };
        w.connect_finished(request, Ok(session));
        w.tabs().active().map(crate::tabs::Tab::id).unwrap()
    }

    fn flat(effect: Effect) -> Vec<Effect> {
        match effect {
            Effect::Batch(effects) => effects.into_iter().flat_map(flat).collect(),
            Effect::None => Vec::new(),
            other => vec![other],
        }
    }

    /// Runs the `RunQuery` effect to its end and applies its events and its result. Returns what
    /// the job reported about a lost connection, and the effect of `query_finished`.
    fn finish_run(w: &mut Workspace, tab: TabId, effect: Effect) -> (Option<usize>, bool, Effect) {
        let Effect::RunQuery {
            job, mut events, ..
        } = effect
        else {
            panic!("expected RunQuery, got {effect:?}");
        };
        let run = job.run;
        let done = run_job(job);
        while let Ok(event) = events.try_recv() {
            w.query_event(tab, run, event);
        }
        let (at, sent) = (done.lost_at, done.lost_sent);
        (at, sent, w.query_finished(done))
    }

    fn run_text(w: &mut Workspace, tab: TabId, text: &str) -> Effect {
        w.run_query(tab, RunMode::All, buf(text))
    }

    fn connect_request(effect: &Effect) -> RequestId {
        match effect {
            Effect::Connect { request, .. } => *request,
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    fn run_effect(effects: Vec<Effect>) -> Effect {
        effects
            .into_iter()
            .find(|e| matches!(e, Effect::RunQuery { .. }))
            .unwrap_or(Effect::None)
    }

    const WRITE: &str = "insert into t values (1)";

    #[test]
    fn decide_truth_table() {
        let again = Decision::Reconnect {
            rerun: true,
            notice: RESTORED_RETRY,
        };
        let user = Decision::Reconnect {
            rerun: false,
            notice: RESTORED_RUN_AGAIN,
        };
        let tx = Decision::Reconnect {
            rerun: false,
            notice: RESTORED_TX_LOST,
        };
        // (pos, sent, read_only, in_tx, retry, cancelled)
        assert_eq!(decide(0, false, false, false, false, false), again);
        assert_eq!(decide(0, true, true, false, false, false), again);
        assert_eq!(decide(0, true, false, false, false, false), user);
        assert_eq!(decide(1, false, true, false, false, false), user);
        assert_eq!(decide(0, false, false, true, false, false), tx);
        assert_eq!(
            decide(0, false, true, false, true, false),
            Decision::Notice(LOST_AGAIN)
        );
        assert_eq!(
            decide(0, false, true, true, true, false),
            Decision::Notice(LOST_AGAIN)
        );
        assert_eq!(decide(0, false, true, false, true, true), Decision::Nothing);
    }

    #[test]
    fn an_unsent_write_reconnects_and_runs_again_once() {
        let mut w = ws();
        let (dead, _) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, WRITE);
        let (at, sent, effect) = finish_run(&mut w, tab, effect);
        assert_eq!((at, sent), (Some(0), false));
        let request = connect_request(&effect);
        let (fresh, _) = FakeSession::boxed();
        let effects = flat(w.connect_finished(request, Ok(fresh)));
        assert_eq!(w.take_notice().as_deref(), Some(RESTORED_RETRY));
        assert!(effects.iter().any(|e| matches!(e, Effect::CloseSession(_))));
        let run = run_effect(effects);
        assert!(matches!(run, Effect::RunQuery { .. }));
        assert!(w.query_tab(tab).unwrap().is_running());
    }

    #[test]
    fn the_dead_session_is_closed() {
        let mut w = ws();
        let (dead, dead_closed) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, WRITE);
        let (_, _, effect) = finish_run(&mut w, tab, effect);
        let request = connect_request(&effect);
        let (fresh, _) = FakeSession::boxed();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for e in flat(w.connect_finished(request, Ok(fresh))) {
            if let Effect::CloseSession(session) = e {
                drop(rt.block_on(session.close()));
            }
        }
        assert!(dead_closed.load(Ordering::SeqCst));
    }

    #[test]
    fn a_sent_read_runs_again_but_a_sent_write_does_not() {
        let mut w = ws();
        let (dead, _) = LostSession::boxed(0, true);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "select 1");
        let (at, sent, effect) = finish_run(&mut w, tab, effect);
        assert_eq!((at, sent), (Some(0), true));
        let request = connect_request(&effect);
        let (fresh, _) = FakeSession::boxed();
        let run = run_effect(flat(w.connect_finished(request, Ok(fresh))));
        assert!(matches!(run, Effect::RunQuery { .. }));
        assert_eq!(w.take_notice().as_deref(), Some(RESTORED_RETRY));

        let mut w = ws();
        let (dead, _) = LostSession::boxed(0, true);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, WRITE);
        let (_, _, effect) = finish_run(&mut w, tab, effect);
        let request = connect_request(&effect);
        let (fresh, _) = FakeSession::boxed();
        let effects = flat(w.connect_finished(request, Ok(fresh)));
        assert!(!effects.iter().any(|e| matches!(e, Effect::RunQuery { .. })));
        assert_eq!(w.take_notice().as_deref(), Some(RESTORED_RUN_AGAIN));
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
        assert!(w.has_session(tab));
    }

    #[test]
    fn a_lost_open_transaction_is_not_run_again() {
        let mut w = ws();
        let (dead, _) = LostSession::boxed(1, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "begin");
        let (at, _, effect) = finish_run(&mut w, tab, effect);
        assert_eq!(at, None);
        assert!(matches!(effect, Effect::None));
        assert!(w.in_transaction(tab));
        let effect = run_text(&mut w, tab, "select 1");
        let (at, sent, effect) = finish_run(&mut w, tab, effect);
        assert_eq!((at, sent), (Some(0), false));
        let request = connect_request(&effect);
        let (fresh, _) = FakeSession::boxed();
        let effects = flat(w.connect_finished(request, Ok(fresh)));
        assert!(!effects.iter().any(|e| matches!(e, Effect::RunQuery { .. })));
        assert_eq!(w.take_notice().as_deref(), Some(RESTORED_TX_LOST));
    }

    #[test]
    fn a_second_loss_does_not_reconnect_again() {
        let mut w = ws();
        let (dead, _) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "select 1");
        let (_, _, effect) = finish_run(&mut w, tab, effect);
        let request = connect_request(&effect);
        let (also_dead, _) = LostSession::boxed(0, false);
        let run = run_effect(flat(w.connect_finished(request, Ok(also_dead))));
        let (at, _, effect) = finish_run(&mut w, tab, run);
        assert_eq!(at, Some(0));
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(LOST_AGAIN));
        assert!(
            w.requests
                .values()
                .all(|p| !matches!(p, Purpose::Reconnect { .. }))
        );
        assert!(w.auto_retries.is_empty());
    }

    #[test]
    fn a_failed_reconnect_shows_the_failure_and_leaves_the_tab_usable() {
        let mut w = ws();
        let (dead, _) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "select 1");
        let (_, _, effect) = finish_run(&mut w, tab, effect);
        let request = connect_request(&effect);
        let failure = ConnectFailure::MissingSecret {
            kind: sqler_config::SecretKind::DatabasePassword,
        };
        let text = failure.to_string();
        assert!(matches!(
            w.connect_finished(request, Err(failure)),
            Effect::None
        ));
        assert_eq!(
            w.take_notice().unwrap(),
            format!("Could not restore the connection: {text}")
        );
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
        assert!(w.has_session(tab));
        assert!(matches!(
            run_text(&mut w, tab, "select 1"),
            Effect::RunQuery { .. }
        ));
    }

    #[test]
    fn an_unknown_host_key_shows_its_prompt_and_no_notice() {
        let mut w = ws();
        let (dead, _) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "select 1");
        let (_, _, effect) = finish_run(&mut w, tab, effect);
        let request = connect_request(&effect);
        let prompt = HostKeyPrompt {
            host: "h".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        };
        w.connect_finished(request, Err(ConnectFailure::UnknownHostKey(prompt)));
        assert!(matches!(
            w.dialog(),
            Some(crate::connections::Dialog::TrustHostKey { .. })
        ));
        assert_eq!(w.take_notice(), None);
        assert!(w.auto_retries.is_empty());
    }

    #[test]
    fn prod_asks_again_before_the_retried_write() {
        let mut w = ws();
        let (dead, _) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 1, dead);
        let effect = run_text(&mut w, tab, WRITE);
        assert!(matches!(effect, Effect::None));
        let effect = w.confirm_query(tab);
        let (at, _, effect) = finish_run(&mut w, tab, effect);
        assert_eq!(at, Some(0));
        let request = connect_request(&effect);
        let (fresh, _) = FakeSession::boxed();
        let effects = flat(w.connect_finished(request, Ok(fresh)));
        assert!(!effects.iter().any(|e| matches!(e, Effect::RunQuery { .. })));
        assert!(matches!(
            w.query_tab(tab).unwrap().phase(),
            Phase::Confirming(_)
        ));
        assert!(matches!(w.confirm_query(tab), Effect::RunQuery { .. }));
    }

    #[test]
    fn a_loss_after_the_first_statement_asks_to_run_again() {
        let mut w = ws();
        let (dead, _) = LostSession::build(1, true, true);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "select 1; select 2");
        let (at, sent, effect) = finish_run(&mut w, tab, effect);
        assert_eq!((at, sent), (Some(1), true));
        let request = connect_request(&effect);
        let (fresh, _) = FakeSession::boxed();
        let effects = flat(w.connect_finished(request, Ok(fresh)));
        assert!(!effects.iter().any(|e| matches!(e, Effect::RunQuery { .. })));
        assert_eq!(w.take_notice().as_deref(), Some(RESTORED_RUN_AGAIN));
    }

    #[test]
    fn a_run_is_refused_while_the_reconnect_runs() {
        let mut w = ws();
        let (dead, _) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "select 1");
        let (_, _, effect) = finish_run(&mut w, tab, effect);
        let _request = connect_request(&effect);
        assert!(matches!(run_text(&mut w, tab, "select 2"), Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(RECONNECTING));
        // Cancel drops the pending re-run.
        assert!(matches!(w.cancel_query(tab), Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(RETRY_DROPPED));
        assert!(w.auto_retries.is_empty());
    }

    #[test]
    fn cancelling_the_reconnect_or_closing_the_tab_drops_the_retry() {
        let mut w = ws();
        let conn = w.sidebar().entries()[0].id().clone();
        let (dead, _) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "select 1");
        let (_, _, _) = finish_run(&mut w, tab, effect);
        assert_eq!(w.auto_retries.len(), 1);
        w.dispatch_connection(A::CancelReconnect(conn));
        assert!(w.auto_retries.is_empty());

        let (dead, _) = LostSession::boxed(0, false);
        let tab = open_with(&mut w, 0, dead);
        let effect = run_text(&mut w, tab, "select 1");
        let (_, _, _) = finish_run(&mut w, tab, effect);
        assert_eq!(w.auto_retries.len(), 1);
        w.dispatch(WorkspaceAction::CloseTab(tab));
        assert!(w.auto_retries.is_empty() && w.retry_marks.is_empty());
    }
}
