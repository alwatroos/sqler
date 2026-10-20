// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Running queries inside the workspace: the run lifecycle of every query tab.
//!
//! Like the object browser this is state only. A run lends the tab's session to a [`QueryJob`]
//! (the tab has no session until the job returns it), keeps the session's cancel handle and the
//! cancel flags of the job and its grids, and returns an [`Effect`] the view carries out. The
//! decisions (parameter prompt, production confirmation, event bookkeeping) live in
//! [`QueryTab`].

use sqler_driver_api::sqler_core::Environment;
use sqler_driver_api::{CancelFlag, CancelHandle};
use sqler_sql::Dialect;
use tokio::sync::mpsc::unbounded_channel;

use super::state::{Effect, Workspace};
use crate::grid::{GridTab, grid_channel};
use crate::query::{
    BoundStatement, BufferSnapshot, ParamInput, ParamKind, Phase, QueryJob, QueryJobDone, QueryTab,
    RunError, RunEvent, RunId, RunMode, Step, plan,
};
use crate::tabs::{TabId, TabKind};

/// A run in progress: what it takes to stop it.
pub(super) struct ActiveRun {
    run: RunId,
    handle: CancelHandle,
    stop: CancelFlag,
    sinks: Vec<CancelFlag>,
}

impl ActiveRun {
    /// Stops the run between statements and ends the ingestion of every grid.
    fn raise_flags(&self) {
        self.stop.cancel();
        for flag in &self.sinks {
            flag.cancel();
        }
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
            Dialect::from_driver_id(connection.driver.as_str()),
            connection.environment == Some(Environment::Prod),
        ))
    }

    /// Runs part of the editor buffer of a query tab.
    ///
    /// Depending on the buffer and the connection this opens the parameter prompt, opens the
    /// production confirmation, or starts the run at once. A tab that is already running,
    /// prompting or confirming refuses the request with a notice.
    #[allow(clippy::needless_pass_by_value)] // the editor hands its snapshot over
    pub fn run_query(&mut self, tab: TabId, mode: RunMode, buf: BufferSnapshot) -> Effect {
        let Some((dialect, prod)) = self.query_context(tab) else {
            return Effect::None;
        };
        if self.tabs.get(tab).map(crate::tabs::Tab::kind) != Some(TabKind::Query) {
            return self.query_notice("Open a query tab first.");
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
        if let Some(query) = self.queries.get_mut(&tab) {
            query.dismiss();
        }
    }

    /// Accepts the production confirmation of a query tab and starts the run.
    pub fn confirm_query(&mut self, tab: TabId) -> Effect {
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
        let Some(session) = self.take_session(tab) else {
            return self.query_notice("The connection of this tab is not available.");
        };
        let run = RunId::new(self.next_run);
        self.next_run += 1;
        let mut sinks = Vec::with_capacity(statements.len());
        let mut grids = Vec::with_capacity(statements.len());
        for _ in statements {
            let (model, sink) = grid_channel();
            grids.push(GridTab::new(model));
            sinks.push(sink);
        }
        let active = ActiveRun {
            run,
            handle: session.cancel_handle(),
            stop: CancelFlag::new(),
            sinks: sinks
                .iter()
                .map(crate::grid::GridSink::cancel_flag)
                .collect(),
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
        };
        if let Some(query) = self.queries.get_mut(&tab) {
            query.started(run, statements);
        }
        self.query_runs.insert(tab, active);
        Effect::RunQuery {
            job,
            grids,
            events: receiver,
        }
    }

    /// Cancels the run of a query tab, or closes its prompt or confirmation.
    pub fn cancel_query(&mut self, tab: TabId) -> Effect {
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
            _ => self.query_notice("No query is running."),
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
    pub fn query_event(&mut self, tab: TabId, run: RunId, event: RunEvent) {
        if let Some(query) = self.queries.get_mut(&tab) {
            query.event(run, event);
        }
    }

    /// The job ended and returned the session, which goes back to its tab (or is closed when the
    /// tab is gone).
    pub fn query_finished(&mut self, done: QueryJobDone) -> Effect {
        let QueryJobDone { tab, run, session } = done;
        if self.query_runs.get(&tab).is_some_and(|a| a.run == run) {
            self.query_runs.remove(&tab);
        }
        match self.queries.get_mut(&tab) {
            Some(query) => {
                query.finished(run);
                self.restore_session(tab, session);
                Effect::None
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
}
