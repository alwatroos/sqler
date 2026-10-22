// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI session tab inside the workspace (ADR-0053).
//!
//! State only, in the lend-and-return pattern of query runs. The idle `ExplorationSession` sits in
//! the tab; a turn moves it into [`Effect::RunAiTurn`], the view runs it on tokio, and it comes
//! back in [`HostMsg::TurnDone`]. Everything the session tells the UI (events, approval asks, UI
//! tool calls, the end of the turn) arrives on one ordered channel per tab and is applied by
//! [`Workspace::ai_session_message`]. Closing the tab or quitting cancels the turn and the
//! statement in flight, denies what waits and closes the session.
//!
//! The production second confirmation is the ADR-0024 gate: a `QueryTab` stored under the AI
//! tab's id in `Workspace::queries`, driven by `QueryTab::request` and answered through
//! `confirm_query` / `dismiss_query_prompt`, which route here for an AI tab.

use sqler_ai_tools::session::TranscriptEntry;
use sqler_ai_tools::{ApprovalStage, ConnectionInfo, Decision, ToolError};
use sqler_config::{ConnectionId, ProviderConfig, UseCase};
use sqler_driver_api::CancelHandle;
use sqler_driver_api::Session;
use tokio::sync::mpsc::unbounded_channel;

use super::ai::NO_PROVIDER;
use super::browser::OpenDataJob;
use super::state::{Effect, Workspace, tab_connection};
use crate::ai::session::{
    AiSessionStart, AiSessionTab, AiTurnJob, ENTRY_GONE, HostMsg, SESSION_AI_OFF, SESSION_BUSY,
    SESSION_CONFIG_BROKEN, SESSION_EMPTY, SESSION_NO_CONNECTION, SESSION_NOT_BUILT,
    SESSION_STARTING, STALE_ANSWER, SessionPhase, SessionReady, TAB_CLOSED, UiCall, UiRequest,
    WRONG_CONNECTION,
};
use crate::ai::{AiAction, ApprovalQueue, HubModel, SessionRow, ai_status, open_prod_gate};
use crate::connections::ConnectRequest;
use crate::grid::{GridLimits, GridTab, grid_channel_with};
use crate::query::{BufferSnapshot, Phase, QueryTab, RunMode};
use crate::tabs::{TabId, TabKind};
use sqler_ai_tools::session::SessionEvent;

/// The title of a results tab the model opened without one.
const DEFAULT_RESULTS_TITLE: &str = "AI query";

/// The effect that does `first`, then `then`.
pub(super) fn join(first: Effect, then: Effect) -> Effect {
    match (first, then) {
        (Effect::None, then) => then,
        (first, Effect::None) => first,
        (first, then) => Effect::Batch(vec![first, then]),
    }
}

impl Workspace {
    /// The AI session tab `tab`, if it is one.
    #[must_use]
    pub fn ai_session(&self, tab: TabId) -> Option<&AiSessionTab> {
        self.ai_sessions.get(tab)
    }

    /// The active tab and its state when it is an AI session tab.
    #[must_use]
    pub fn active_ai_session(&self) -> Option<(TabId, &AiSessionTab)> {
        let id = self.tabs.active()?.id();
        self.ai_sessions.get(id).map(|tab| (id, tab))
    }

    /// The approval queue of the tab, when it is an AI session tab or an agent tab: the one seam
    /// every statement approval and the production gate go through (ADR-0053, ADR-0051).
    pub(crate) fn approvals_mut(&mut self, tab: TabId) -> Option<&mut ApprovalQueue> {
        if let Some(t) = self.ai_sessions.tabs.get_mut(&tab) {
            return Some(&mut t.approvals);
        }
        self.agents.tabs.get_mut(&tab).map(|t| &mut t.approvals)
    }

    /// The approval queue of the tab, read only.
    pub(crate) fn approvals(&self, tab: TabId) -> Option<&ApprovalQueue> {
        self.ai_sessions
            .tabs
            .get(&tab)
            .map(|t| &t.approvals)
            .or_else(|| self.agents.tabs.get(&tab).map(|t| &t.approvals))
    }

    /// Whether a session can start on `conn`: the provider and model of the Exploration use case,
    /// or the reason it cannot.
    ///
    /// # Errors
    /// The text to show: AI is off, `ai.toml` is broken, no model is chosen or the provider kind
    /// is not in this build.
    pub fn session_gate(
        &self,
        conn: &ConnectionId,
    ) -> Result<(ProviderConfig, String), &'static str> {
        let profile = self.sidebar.profile(conn).ok_or(SESSION_NO_CONNECTION)?;
        if !profile.spec().ai.enabled {
            return Err(SESSION_AI_OFF);
        }
        if self.ai.load_error().is_some() {
            return Err(SESSION_CONFIG_BROKEN);
        }
        self.ai_audit()?;
        let config = self.ai.config();
        let choice = config
            .models
            .for_use_case(UseCase::Exploration)
            .ok_or(NO_PROVIDER)?;
        let provider = config.provider(&choice.provider).ok_or(NO_PROVIDER)?;
        if !self.ai.kinds().contains(&provider.kind) {
            return Err(SESSION_NOT_BUILT);
        }
        Ok((provider.clone(), choice.model.clone()))
    }

    /// Fills the session part of the AI view model.
    pub(super) fn with_sessions(&self, mut model: HubModel) -> HubModel {
        let mut ids: Vec<TabId> = self.ai_sessions.tabs.keys().copied().collect();
        ids.sort();
        model.sessions = ids
            .into_iter()
            .filter_map(|id| {
                let tab = self.ai_sessions.get(id)?;
                Some(SessionRow {
                    tab: id,
                    title: self.tabs.get(id)?.title(),
                    connection: tab.info().name.clone(),
                    label: tab.label(),
                })
            })
            .collect();
        model.new_session_blocked = match self.ai_target() {
            Some(conn) => self.session_gate(&conn).err(),
            None => Some(SESSION_NO_CONNECTION),
        };
        self.with_agents(model)
    }

    fn session_notice(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    /// Runs an AI session action. The returned effect is carried out by the view.
    pub(super) fn dispatch_session(&mut self, action: AiAction) -> Effect {
        match action {
            AiAction::OpenSession => self.open_ai_session(),
            AiAction::SetSessionDraft(tab, text) => {
                if let Some(t) = self.ai_sessions.tabs.get_mut(&tab) {
                    t.draft = text;
                }
                Effect::None
            }
            AiAction::SendMessage(tab) => self.send_ai_message(tab),
            AiAction::CancelSession(tab) => self.cancel_ai_session(tab),
            AiAction::ApproveStatement(tab, id) => self.answer_card(tab, id, Decision::Approve),
            AiAction::DenyStatement(tab, id) => self.answer_card(tab, id, Decision::Deny),
            AiAction::RerunSql(tab, index) => self.open_sql(tab, index, true),
            AiAction::OpenSqlInEditor(tab, index) => self.open_sql(tab, index, false),
            _ => Effect::None,
        }
    }

    fn open_ai_session(&mut self) -> Effect {
        let Some(conn) = self.ai_target() else {
            return self.session_notice(SESSION_NO_CONNECTION);
        };
        let (provider, model) = match self.session_gate(&conn) {
            Ok(ok) => ok,
            Err(NO_PROVIDER) => return self.no_provider(),
            Err(message) => return self.session_notice(message),
        };
        let (Some(profile), Some(index)) =
            (self.sidebar.profile(&conn), self.sidebar.index_of(&conn))
        else {
            return self.session_notice(SESSION_NO_CONNECTION);
        };
        let request = match ConnectRequest::from_profile(profile) {
            Ok(request) => request,
            Err(err) => return self.session_notice(err.to_string()),
        };
        let spec = profile.spec();
        let info = ConnectionInfo::new(
            profile.id().as_str(),
            spec.name.clone(),
            spec.driver.clone(),
            spec.environment,
        )
        .with_send_rows(profile.ai_sends_rows());
        let status = ai_status(self.ai.config(), Some(spec));
        let connection = tab_connection(self.sidebar.entries()[index].config());
        let audit = match self.ai_audit() {
            Ok(audit) => audit.clone(),
            Err(message) => return self.session_notice(message),
        };
        let tab = self.tabs.open(connection, TabKind::AiSession);
        self.tab_conns.insert(tab, conn.clone());
        // The gate of the production second confirmation (ADR-0024).
        self.queries.insert(tab, QueryTab::new());
        self.show_connection_tabs();
        let (tx, host) = unbounded_channel();
        self.ai_sessions.tabs.insert(
            tab,
            AiSessionTab::new(conn.clone(), info.clone(), status, tx.clone()),
        );
        let start = AiSessionStart {
            tab,
            provider,
            model,
            info,
            request,
            config: self.ai.config().clone(),
            audit,
            tx,
        };
        join(
            self.ensure_browser(&conn),
            Effect::StartAiSession {
                start: Box::new(start),
                host,
            },
        )
    }

    /// The session was built (or could not be).
    pub fn ai_session_ready(&mut self, tab: TabId, result: Result<SessionReady, String>) -> Effect {
        let Some(t) = self.ai_sessions.tabs.get_mut(&tab) else {
            // The tab was closed while the session was built.
            return match result {
                Ok(ready) => ready
                    .source
                    .shut()
                    .map_or(Effect::None, Effect::CloseSession),
                Err(_) => Effect::None,
            };
        };
        match result {
            Ok(ready) => {
                t.cancel = Some(ready.session.cancel_handle());
                t.session = Some(Box::new(ready.session));
                t.source = Some(ready.source);
                t.phase = SessionPhase::Ready;
            }
            Err(message) => t.phase = SessionPhase::Failed(message),
        }
        Effect::None
    }

    fn send_ai_message(&mut self, tab: TabId) -> Effect {
        let Some(conn) = self.ai_sessions.get(tab).map(|t| t.conn.clone()) else {
            return Effect::None;
        };
        let enabled = self
            .sidebar
            .profile(&conn)
            .is_some_and(|p| p.spec().ai.enabled);
        if !enabled {
            return self.session_notice(SESSION_AI_OFF);
        }
        let catalog = self.ready_catalog(&conn);
        let Some(t) = self.ai_sessions.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        let refusal = match &t.phase {
            SessionPhase::Starting => Some(SESSION_STARTING.to_owned()),
            SessionPhase::Running { .. } => Some(SESSION_BUSY.to_owned()),
            SessionPhase::Failed(message) => Some(message.clone()),
            SessionPhase::Ready => None,
        };
        if let Some(message) = refusal {
            return self.session_notice(message);
        }
        let text = t.draft.trim().to_owned();
        if text.is_empty() {
            return self.session_notice(SESSION_EMPTY);
        }
        let Some(session) = t.session.take() else {
            return self.session_notice(SESSION_STARTING);
        };
        if let Some(source) = &t.source {
            source.set_catalog(catalog);
        }
        let turn = t.next_turn;
        t.next_turn += 1;
        t.phase = SessionPhase::Running { turn };
        t.draft.clear();
        t.streaming.clear();
        Effect::RunAiTurn(Box::new(AiTurnJob {
            tab,
            turn,
            session: *session,
            text,
            tx: t.tx.clone(),
        }))
    }

    /// Stops the turn: cancels the session, denies what waits, closes the gate and asks the
    /// database to cancel the statement in flight.
    fn cancel_ai_session(&mut self, tab: TabId) -> Effect {
        let Some(t) = self.ai_sessions.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        if !t.is_running() {
            return Effect::None;
        }
        if let Some(cancel) = &t.cancel {
            cancel.cancel();
        }
        t.approvals.deny_all();
        let handle = t.in_flight();
        if let Some(gate) = self.queries.get_mut(&tab) {
            gate.dismiss();
        }
        handle.map_or(Effect::None, Effect::CancelQuery)
    }

    /// Answers the approval card (the `Confirm` stage). The production second confirmation is
    /// answered through the gate only.
    ///
    /// The answer names the ask it is for. When that ask is not the one that waits any more, it
    /// does nothing but tell the user; it never answers another ask.
    pub(super) fn answer_card(&mut self, tab: TabId, id: u64, decision: Decision) -> Effect {
        let Some(approvals) = self.approvals_mut(tab) else {
            return Effect::None;
        };
        if approvals.answer_id(id, ApprovalStage::Confirm, decision) {
            return Effect::None;
        }
        self.session_notice(STALE_ANSWER)
    }

    /// The user answered the production confirmation of an AI tab: confirm or dismiss the gate,
    /// then answer the ask. Does nothing unless the gate is open for a waiting second stage.
    pub(super) fn answer_prod_gate(&mut self, tab: TabId, decision: Decision) {
        let waiting = self.approvals(tab).and_then(ApprovalQueue::gated_current);
        let Some(gate) = self.queries.get_mut(&tab) else {
            return;
        };
        let Some(id) = waiting else {
            return;
        };
        if !matches!(gate.phase(), Phase::Confirming(_)) {
            return;
        }
        match decision {
            Decision::Approve => {
                gate.confirm();
            }
            _ => gate.dismiss(),
        }
        if let Some(approvals) = self.approvals_mut(tab) {
            approvals.answer_id(id, ApprovalStage::ProdSecond, decision);
        }
    }

    /// Opens a statement of the transcript in a new editor tab; with `run`, the statement runs
    /// as soon as the editor is connected, through the normal run flow and its production gate.
    fn open_sql(&mut self, tab: TabId, index: usize, run: bool) -> Effect {
        let Some(t) = self.ai_sessions.get(tab) else {
            return Effect::None;
        };
        let Some(TranscriptEntry::Sql(entry)) = t.entry(index) else {
            return self.session_notice(ENTRY_GONE);
        };
        let rerun = entry.rerun();
        if rerun.connection != t.info().id {
            return self.session_notice(WRONG_CONNECTION);
        }
        let conn = t.conn.clone();
        let effect = self.open_editor_on(&conn, Some(rerun.sql.clone()));
        if let (true, Effect::Connect { request, .. }) = (run, &effect) {
            self.ai_sessions.reruns.insert(*request, rerun.sql);
        }
        effect
    }

    /// The editor tab that a re-run opened is connected: run its text.
    pub(super) fn rerun_opened(
        &mut self,
        before: Option<TabId>,
        effect: Effect,
        sql: String,
    ) -> Effect {
        let Some(tab) = self.tabs.active().map(crate::tabs::Tab::id) else {
            return effect;
        };
        let fresh = Some(tab) != before
            && self.tabs.get(tab).map(crate::tabs::Tab::kind) == Some(TabKind::Query)
            && self.has_session(tab);
        if !fresh {
            return effect;
        }
        let run = self.run_query(tab, RunMode::All, BufferSnapshot::at_cursor(sql, 0));
        join(effect, run)
    }

    /// Applies one message of a session's host channel. A message for a tab that is gone is
    /// dropped; a dropped ask is a deny and a dropped UI call is an error.
    pub fn ai_session_message(&mut self, tab: TabId, message: HostMsg) -> Effect {
        if !self.ai_sessions.tabs.contains_key(&tab) {
            if let HostMsg::Ui(call) = message {
                let _ = call.reply.send(Err(ToolError::Host(TAB_CLOSED.to_owned())));
            }
            return Effect::None;
        }
        match message {
            HostMsg::Event(event) => {
                self.apply_session_event(tab, event);
                Effect::None
            }
            HostMsg::Ask(ask) => {
                self.ai_ask(tab, ask);
                Effect::None
            }
            HostMsg::Ui(call) => self.ai_ui_call(tab, call),
            HostMsg::TurnDone(done) => {
                let done = *done;
                let Some(t) = self.ai_sessions.tabs.get_mut(&tab) else {
                    return Effect::None;
                };
                if t.phase != (SessionPhase::Running { turn: done.turn }) {
                    // A turn that is no longer the current one: its session is dropped.
                    return Effect::None;
                }
                let transcript = done.session.transcript();
                t.set_entries(transcript.entries(), transcript.dropped());
                t.session = Some(Box::new(done.session));
                t.phase = SessionPhase::Ready;
                t.streaming.clear();
                t.approvals.deny_all();
                if let Some(gate) = self.queries.get_mut(&tab) {
                    gate.dismiss();
                }
                Effect::None
            }
        }
    }

    fn apply_session_event(&mut self, tab: TabId, event: SessionEvent) {
        let Some(t) = self.ai_sessions.tabs.get_mut(&tab) else {
            return;
        };
        match event {
            SessionEvent::Entry(entry) => {
                if matches!(entry, TranscriptEntry::Assistant { .. }) {
                    t.streaming.clear();
                }
                t.push_entry(entry);
            }
            SessionEvent::TextDelta(text) => t.streaming.push_str(&text),
            SessionEvent::State(state) => t.state = state,
            SessionEvent::TurnEnded(_) => t.streaming.clear(),
            _ => {}
        }
    }

    /// Queues an ask of the tool set for the tab that owns it (an AI session tab or an agent
    /// tab). A tab that does not exist, a second production confirmation and a statement the
    /// gate cannot show are denied.
    pub(super) fn ai_ask(&mut self, tab: TabId, ask: crate::ai::ApprovalAsk) {
        let Some(waiting) = self.approvals(tab).map(ApprovalQueue::is_waiting) else {
            // No tab owns this ask any more.
            let _ = ask.reply.send(Decision::Deny);
            return;
        };
        let prod = ask.request.stage == ApprovalStage::ProdSecond;
        if prod {
            if waiting {
                // The gate holds one statement; a second one cannot be shown beside it.
                let _ = ask.reply.send(Decision::Deny);
                self.notice = Some(STALE_ANSWER.to_owned());
                return;
            }
            let driver = ask.request.connection.driver.clone();
            let opened = match self.queries.get_mut(&tab) {
                Some(gate) => open_prod_gate(gate, &driver, &ask.request.sql),
                None => Err(TAB_CLOSED),
            };
            if let Err(message) = opened {
                // Doubt errs toward refusing.
                let _ = ask.reply.send(Decision::Deny);
                self.notice = Some(message.to_owned());
                return;
            }
        }
        if let Some(approvals) = self.approvals_mut(tab) {
            let id = approvals.push(ask);
            if prod {
                approvals.bind_gate(id);
            }
        }
    }

    /// A tool only the UI can run. Both reply `Ok` once the effect is built.
    fn ai_ui_call(&mut self, tab: TabId, call: UiCall) -> Effect {
        let Some(t) = self.ai_sessions.get(tab) else {
            let _ = call.reply.send(Err(ToolError::Host(TAB_CLOSED.to_owned())));
            return Effect::None;
        };
        let conn = t.conn.clone();
        let id = t.info().id.clone();
        let UiCall { request, reply } = call;
        let wrong = |name: Option<&str>| name.is_some_and(|n| n != id);
        match request {
            UiRequest::OpenInEditor { connection, sql } => {
                if wrong(connection.as_deref()) {
                    let _ = reply.send(Err(ToolError::Host(WRONG_CONNECTION.to_owned())));
                    return Effect::None;
                }
                let effect = self.open_editor_on(&conn, Some(sql));
                let _ = reply.send(Ok(()));
                effect
            }
            UiRequest::OpenResults {
                connection,
                sql,
                title,
            } => {
                if wrong(Some(&connection)) {
                    let _ = reply.send(Err(ToolError::Host(WRONG_CONNECTION.to_owned())));
                    return Effect::None;
                }
                match self.open_results_tab(&conn, sql, title) {
                    Ok(effect) => {
                        let _ = reply.send(Ok(()));
                        effect
                    }
                    Err(message) => {
                        let _ = reply.send(Err(ToolError::Host(message)));
                        Effect::None
                    }
                }
            }
        }
    }

    fn open_results_tab(
        &mut self,
        conn: &ConnectionId,
        sql: String,
        title: Option<String>,
    ) -> Result<Effect, String> {
        let request = self.request_for(conn)?;
        let index = self
            .sidebar
            .index_of(conn)
            .ok_or_else(|| "The connection no longer exists.".to_owned())?;
        let connection = tab_connection(self.sidebar.entries()[index].config());
        let name = title.unwrap_or_else(|| DEFAULT_RESULTS_TITLE.to_owned());
        let tab = self.tabs.open_object(connection, TabKind::Grid, name);
        self.tab_conns.insert(tab, conn.clone());
        self.show_connection_tabs();
        let limits = GridLimits::from_settings(self.settings().settings());
        let (model, sink) = grid_channel_with(limits);
        self.grid_cancels.insert(tab, sink.canceller());
        Ok(Effect::OpenData {
            tab,
            grid: GridTab::new(model),
            job: OpenDataJob { request, sql, sink },
        })
    }

    /// The turn job of `tab` ended without reporting (the task was lost): the session is gone.
    pub fn ai_turn_lost(&mut self, tab: TabId, turn: u64) {
        let Some(t) = self.ai_sessions.tabs.get_mut(&tab) else {
            return;
        };
        if t.phase == (SessionPhase::Running { turn }) {
            t.phase = SessionPhase::Failed(
                "The turn ended unexpectedly and the session was lost. Open a new session."
                    .to_owned(),
            );
            t.streaming.clear();
            t.approvals.deny_all();
            if let Some(gate) = self.queries.get_mut(&tab) {
                gate.dismiss();
            }
        }
    }

    /// Stops the turn of every session of `conn` (AI was turned off for it).
    pub(super) fn stop_ai_sessions_of(&mut self, conn: &ConnectionId) -> Effect {
        let tabs: Vec<TabId> = self
            .ai_sessions
            .tabs
            .iter()
            .filter(|(_, t)| &t.conn == conn)
            .map(|(id, _)| *id)
            .collect();
        tabs.into_iter()
            .map(|tab| self.cancel_ai_session(tab))
            .fold(Effect::None, join)
    }

    /// The tab is going away: cancel its turn and statement, deny what waits and close the
    /// session. A session on loan is closed when its turn ends.
    pub(super) fn close_ai_session(&mut self, tab: TabId) -> Effect {
        let Some(mut t) = self.ai_sessions.tabs.remove(&tab) else {
            return Effect::None;
        };
        if let Some(cancel) = &t.cancel {
            cancel.cancel();
        }
        t.approvals.deny_all();
        let cancel = t.in_flight().map_or(Effect::None, Effect::CancelQuery);
        let close = t
            .source
            .as_ref()
            .and_then(|s| s.shut())
            .map_or(Effect::None, Effect::CloseSession);
        join(cancel, close)
    }

    /// The app is quitting: cancels every turn, denies what waits and returns the handles of the
    /// statements in flight.
    pub(super) fn cancel_ai_sessions(&mut self) -> Vec<CancelHandle> {
        let mut handles = Vec::new();
        for t in self.ai_sessions.tabs.values_mut() {
            if let Some(cancel) = &t.cancel {
                cancel.cancel();
            }
            t.approvals.deny_all();
            handles.extend(t.in_flight());
        }
        handles
    }

    /// Shuts every session source and returns the idle sessions to close.
    pub(super) fn shut_ai_sources(&mut self) -> Vec<Box<dyn Session>> {
        self.ai_sessions
            .tabs
            .values()
            .filter_map(|t| t.source.as_ref().and_then(|s| s.shut()))
            .collect()
    }
}
