// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The agent tab inside the workspace (ADR-0051).
//!
//! State only. Picking an agent returns [`Effect::StartAgent`]; the view starts it on tokio and
//! reports with [`Workspace::agent_ready`], then everything the agent does arrives on one ordered
//! channel and is applied by [`Workspace::agent_message`]. The tab talks to the runner task
//! through an [`AgentControl`], which needs no runtime. Closing the tab, quitting, a crash and
//! turning AI off all end the same way: the control closes, the queues are answered with a deny
//! or a cancel, the statement in flight is cancelled and the idle session is closed.
//!
//! Statement approvals and the production gate use the seam of ADR-0053 (`approvals_mut`), so
//! nothing is forked; the agent's own permission requests wait in a sibling queue.

use sqler_acp::{AgentEvent, AgentProfile, ExitInfo, StopReason};
use sqler_ai_tools::{ConnectionInfo, Decision};
use sqler_config::ConnectionId;
use sqler_driver_api::{CancelHandle, Session};
use tokio::sync::mpsc::unbounded_channel;

use super::ai_session::join;
use super::state::{Effect, Workspace, tab_connection};
use crate::ai::agent::{
    AGENT_AI_OFF, AGENT_BUSY, AGENT_NOT_READY, AGENT_STOPPED_AI_OFF, AGENT_STOPPED_QUIT,
    AGENT_UNKNOWN, AgentAbort, AgentFailure, AgentMsg, AgentPhase, AgentProfiles, AgentReady,
    AgentStart, AgentTab, app_launch_options,
};
use crate::ai::session::{
    SESSION_CONFIG_BROKEN, SESSION_EMPTY, SESSION_NO_CONNECTION, STALE_ANSWER,
};
use crate::ai::{AgentRow, AiAction, HubModel, ai_status};
use crate::query::QueryTab;
use crate::tabs::{TabId, TabKind};

impl Workspace {
    /// The agent tab `tab`, if it is one.
    #[must_use]
    pub fn agent(&self, tab: TabId) -> Option<&AgentTab> {
        self.agents.get(tab)
    }

    /// The active tab and its state when it is an agent tab.
    #[must_use]
    pub fn active_agent(&self) -> Option<(TabId, &AgentTab)> {
        let id = self.tabs.active()?.id();
        self.agents.get(id).map(|tab| (id, tab))
    }

    /// The agents the picker offers, once they are read.
    #[must_use]
    pub fn agent_profiles(&self) -> Option<&AgentProfiles> {
        self.agents.profiles.as_ref()
    }

    /// How many agent tabs have their tool endpoint open.
    #[must_use]
    pub fn agent_endpoints(&self) -> usize {
        self.agents
            .tabs
            .values()
            .filter(|t| t.endpoint_url.is_some())
            .count()
    }

    /// Whether an agent can start on `conn`: it exists, AI is on for it and `ai.toml` is read.
    /// No provider is needed: the agent brings its own model.
    ///
    /// # Errors
    /// The text to show.
    pub fn agent_gate(&self, conn: &ConnectionId) -> Result<(), &'static str> {
        let profile = self.sidebar.profile(conn).ok_or(SESSION_NO_CONNECTION)?;
        if !profile.spec().ai.enabled {
            return Err(AGENT_AI_OFF);
        }
        if self.ai.load_error().is_some() {
            return Err(SESSION_CONFIG_BROKEN);
        }
        Ok(())
    }

    /// Fills the agent part of the AI view model.
    pub(super) fn with_agents(&self, mut model: HubModel) -> HubModel {
        let mut ids: Vec<TabId> = self.agents.tabs.keys().copied().collect();
        ids.sort();
        model.agents = ids
            .into_iter()
            .filter_map(|id| {
                let tab = self.agents.get(id)?;
                Some(AgentRow {
                    tab: id,
                    title: self.tabs.get(id)?.title(),
                    connection: tab.info().name.clone(),
                    agent: tab.agent_name().map(str::to_owned),
                    label: tab.label(),
                })
            })
            .collect();
        model.new_agent_blocked = match self.ai_target() {
            Some(conn) => self.agent_gate(&conn).err(),
            None => Some(SESSION_NO_CONNECTION),
        };
        model
    }

    fn agent_notice(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    /// Runs an agent action. The returned effect is carried out by the view.
    pub(super) fn dispatch_agent(&mut self, action: AiAction) -> Effect {
        match action {
            AiAction::OpenAgent => self.open_agent(),
            AiAction::PickAgent(tab, id) => self.pick_agent(tab, &id),
            AiAction::SetAgentDraft(tab, text) => {
                if let Some(t) = self.agents.tabs.get_mut(&tab) {
                    t.draft = text;
                }
                Effect::None
            }
            AiAction::SendAgentPrompt(tab) => self.send_agent_prompt(tab),
            AiAction::CancelAgent(tab) => self.cancel_agent(tab),
            AiAction::RestartAgent(tab) => self.restart_agent(tab),
            AiAction::AllowAgentRequest(tab, id, option)
            | AiAction::RejectAgentRequest(tab, id, option) => {
                self.answer_permission(tab, id, &option)
            }
            _ => Effect::None,
        }
    }

    /// The connection as the tools see it, from the saved profile.
    fn agent_info(&self, conn: &ConnectionId) -> Option<ConnectionInfo> {
        let profile = self.sidebar.profile(conn)?;
        let spec = profile.spec();
        Some(
            ConnectionInfo::new(
                profile.id().as_str(),
                spec.name.clone(),
                spec.driver.clone(),
                spec.environment,
            )
            .with_send_rows(profile.ai_sends_rows()),
        )
    }

    fn open_agent(&mut self) -> Effect {
        let Some(conn) = self.ai_target() else {
            return self.agent_notice(SESSION_NO_CONNECTION);
        };
        if let Err(message) = self.agent_gate(&conn) {
            return self.agent_notice(message);
        }
        let (Some(info), Some(index), Some(profile)) = (
            self.agent_info(&conn),
            self.sidebar.index_of(&conn),
            self.sidebar.profile(&conn),
        ) else {
            return self.agent_notice(SESSION_NO_CONNECTION);
        };
        let status = ai_status(self.ai.config(), Some(profile.spec()));
        let connection = tab_connection(self.sidebar.entries()[index].config());
        let tab = self.tabs.open(connection, TabKind::Agent);
        self.tab_conns.insert(tab, conn.clone());
        // The gate of the production second confirmation (ADR-0024).
        self.queries.insert(tab, QueryTab::new());
        self.show_connection_tabs();
        self.agents
            .tabs
            .insert(tab, AgentTab::new(conn.clone(), info, status));
        let load = if self.agents.profiles.is_none() && !self.agents.loading {
            self.agents.loading = true;
            Effect::LoadAgentProfiles
        } else {
            Effect::None
        };
        join(self.ensure_browser(&conn), load)
    }

    /// The profiles were read (or could not be): the picker offers them. A file that could not
    /// be used is told in a notice; the built-in agents are offered anyway.
    pub fn agent_profiles_loaded(&mut self, profiles: AgentProfiles) {
        if let Some(error) = &profiles.error {
            self.notice = Some(error.clone());
        }
        self.agents.profiles = Some(profiles);
        self.agents.loading = false;
    }

    fn pick_agent(&mut self, tab: TabId, id: &str) -> Effect {
        let picking = self
            .agents
            .get(tab)
            .is_some_and(|t| t.phase == AgentPhase::Picking);
        if !picking {
            return Effect::None;
        }
        let Some(profiles) = &self.agents.profiles else {
            return self.agent_notice(AGENT_NOT_READY);
        };
        let Some(profile) = profiles.get(id).cloned() else {
            return self.agent_notice(AGENT_UNKNOWN);
        };
        self.begin_agent(tab, profile)
    }

    fn restart_agent(&mut self, tab: TabId) -> Effect {
        let Some(t) = self.agents.get(tab) else {
            return Effect::None;
        };
        if !matches!(t.phase, AgentPhase::Stopped(_) | AgentPhase::Failed(_)) {
            return Effect::None;
        }
        let Some(profile) = t.profile.clone() else {
            return Effect::None;
        };
        self.begin_agent(tab, profile)
    }

    /// Builds the start of the agent `profile` for `tab` and moves the tab to `Starting`.
    fn begin_agent(&mut self, tab: TabId, profile: AgentProfile) -> Effect {
        let Some(conn) = self.agents.get(tab).map(|t| t.conn.clone()) else {
            return Effect::None;
        };
        if let Err(message) = self.agent_gate(&conn) {
            return self.agent_notice(message);
        }
        let request = match self.request_for(&conn) {
            Ok(request) => request,
            Err(message) => return self.agent_notice(message),
        };
        let Some(info) = self.agent_info(&conn) else {
            return self.agent_notice(SESSION_NO_CONNECTION);
        };
        let config = self.ai.config().clone();
        let Some(t) = self.agents.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        let (tx, host) = unbounded_channel();
        if matches!(t.phase, AgentPhase::Stopped(_) | AgentPhase::Failed(_)) {
            t.transcript.push_notice("Restarting the agent.");
        }
        t.info = info.clone();
        t.phase = AgentPhase::Starting;
        t.profile = Some(profile.clone());
        t.agent = None;
        let (abort_handle, abort) = AgentAbort::new();
        if let Some(old) = t.abort.replace(abort_handle) {
            old.fire();
        }
        let start = AgentStart {
            tab,
            profile,
            info,
            request,
            config,
            audit: None,
            cwd: std::path::PathBuf::new(),
            launch: app_launch_options(),
            tx,
            abort,
        };
        Effect::StartAgent {
            start: Box::new(start),
            host,
        }
    }

    /// The agent was started (or could not be).
    pub fn agent_ready(&mut self, tab: TabId, result: Result<AgentReady, AgentFailure>) -> Effect {
        let Some(t) = self.agents.tabs.get_mut(&tab) else {
            // The tab was closed while the agent started: tear it down.
            if let Ok(ready) = result {
                ready.control.close();
            }
            return Effect::None;
        };
        match result {
            Ok(ready) if t.phase == AgentPhase::Starting => {
                t.agent = Some(ready.agent);
                t.endpoint_url = Some(ready.endpoint_url);
                t.source = Some(ready.source);
                t.control = Some(ready.control);
                t.phase = AgentPhase::Ready;
            }
            Ok(ready) => {
                // Stopped while it started (AI turned off, or it already ended).
                ready.control.close();
            }
            Err(failure) => {
                if t.phase == AgentPhase::Starting {
                    t.phase = AgentPhase::Failed(failure);
                }
            }
        }
        Effect::None
    }

    fn send_agent_prompt(&mut self, tab: TabId) -> Effect {
        let Some(conn) = self.agents.get(tab).map(|t| t.conn.clone()) else {
            return Effect::None;
        };
        let enabled = self
            .sidebar
            .profile(&conn)
            .is_some_and(|p| p.spec().ai.enabled);
        if !enabled {
            return self.agent_notice(AGENT_AI_OFF);
        }
        let catalog = self.ready_catalog(&conn);
        let Some(t) = self.agents.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        let refusal = match &t.phase {
            AgentPhase::Picking | AgentPhase::Starting => Some(AGENT_NOT_READY.to_owned()),
            AgentPhase::Running { .. } => Some(AGENT_BUSY.to_owned()),
            AgentPhase::Stopped(f) | AgentPhase::Failed(f) => Some(f.message.clone()),
            AgentPhase::Ready => None,
        };
        if let Some(message) = refusal {
            return self.agent_notice(message);
        }
        let text = t.draft.trim().to_owned();
        if text.is_empty() {
            return self.agent_notice(SESSION_EMPTY);
        }
        let Some(control) = t.control.as_ref() else {
            return self.agent_notice(AGENT_NOT_READY);
        };
        if let Some(source) = &t.source {
            source.set_catalog(catalog);
        }
        let turn = t.next_turn;
        t.next_turn += 1;
        t.phase = AgentPhase::Running { turn };
        t.draft.clear();
        t.transcript.begin_turn();
        t.transcript.push_user(&text);
        control.prompt(turn, text);
        Effect::None
    }

    /// Ends the turn: cancels the prompt, denies what waits, closes the gate and asks the
    /// database to cancel the statement in flight. The process is killed only when the agent
    /// ignores the cancel (the grace period of the launch options).
    fn cancel_agent(&mut self, tab: TabId) -> Effect {
        let Some(t) = self.agents.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        if !t.is_running() {
            return Effect::None;
        }
        if let Some(control) = &t.control {
            control.cancel();
        }
        t.approvals.deny_all();
        t.permissions.cancel_all();
        let handle = t.source.as_ref().and_then(|s| s.cancel_in_flight());
        if let Some(gate) = self.queries.get_mut(&tab) {
            gate.dismiss();
        }
        handle.map_or(Effect::None, Effect::CancelQuery)
    }

    /// Answers the permission request `id` of an agent tab with one of the agent's options. A
    /// request that is not the one that waits any more, or an option it did not offer, does
    /// nothing but tell the user.
    fn answer_permission(&mut self, tab: TabId, id: u64, option: &str) -> Effect {
        let Some(t) = self.agents.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        if t.permissions.answer_id(id, option) {
            return Effect::None;
        }
        self.agent_notice(STALE_ANSWER)
    }

    /// Applies one message of an agent's host channel. A message for a tab that is gone is
    /// dropped; a dropped ask is a deny and a dropped permission request is a cancel.
    pub fn agent_message(&mut self, tab: TabId, message: AgentMsg) -> Effect {
        let Some(t) = self.agents.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        let listening = matches!(
            t.phase,
            AgentPhase::Starting | AgentPhase::Ready | AgentPhase::Running { .. }
        );
        match message {
            AgentMsg::Event(AgentEvent::Update { update, .. }) => {
                t.transcript.apply(&update);
                Effect::None
            }
            AgentMsg::Event(AgentEvent::Stderr(line)) => {
                t.transcript.push_stderr(&line);
                Effect::None
            }
            AgentMsg::Event(AgentEvent::Exited(exit)) => self.agent_exited(tab, &exit),
            AgentMsg::Event(_) => Effect::None,
            AgentMsg::Ask(ask) => {
                if listening {
                    self.ai_ask(tab, ask);
                } else {
                    // A stopped agent gets no write through.
                    let _ = ask.reply.send(Decision::Deny);
                }
                Effect::None
            }
            AgentMsg::Permission(ask) => {
                if listening {
                    t.permissions.push(ask);
                } else {
                    drop(ask);
                }
                Effect::None
            }
            AgentMsg::PromptDone { turn, result } => {
                if t.phase != (AgentPhase::Running { turn }) {
                    return Effect::None;
                }
                t.phase = AgentPhase::Ready;
                t.approvals.deny_all();
                t.permissions.cancel_all();
                match result {
                    Ok(StopReason::Cancelled) => t.transcript.push_notice("Cancelled."),
                    Ok(StopReason::Refusal) => {
                        t.transcript.push_notice("The agent refused to continue.");
                    }
                    Ok(StopReason::MaxTokens) => {
                        t.transcript
                            .push_notice("The agent stopped at its token limit.");
                    }
                    Ok(StopReason::MaxTurnRequests) => {
                        t.transcript
                            .push_notice("The agent stopped at its turn limit.");
                    }
                    Ok(_) => {}
                    Err(message) => t.transcript.push_notice(message),
                }
                if let Some(gate) = self.queries.get_mut(&tab) {
                    gate.dismiss();
                }
                Effect::None
            }
        }
    }

    /// The agent process ended: the tab is stopped and nothing waits any more.
    fn agent_exited(&mut self, tab: TabId, exit: &ExitInfo) -> Effect {
        let Some(t) = self.agents.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        if matches!(t.phase, AgentPhase::Stopped(_) | AgentPhase::Failed(_)) {
            // Stopped by SQLer already; that reason stays.
            return Effect::None;
        }
        let tail = if exit.stderr_tail.is_empty() {
            t.transcript.stderr()
        } else {
            exit.stderr_tail.clone()
        };
        let failure = AgentFailure {
            message: format!("The agent stopped ({exit})."),
            install_hint: None,
            stderr_tail: tail,
        };
        self.halt_agent(tab, Some(AgentPhase::Stopped(failure)))
    }

    /// Ends the live part of an agent tab: closes the control, denies and cancels what waits,
    /// dismisses the gate, cancels the statement in flight and closes the idle session. With
    /// `phase`, the tab moves there.
    fn halt_agent(&mut self, tab: TabId, phase: Option<AgentPhase>) -> Effect {
        let Some(t) = self.agents.tabs.get_mut(&tab) else {
            return Effect::None;
        };
        if let Some(control) = t.control.take() {
            control.close();
        }
        // A start that is still going gives up and kills what it launched.
        if let Some(abort) = t.abort.take() {
            abort.fire();
        }
        t.endpoint_url = None;
        t.approvals.deny_all();
        t.permissions.cancel_all();
        let cancel = t
            .source
            .as_ref()
            .and_then(|s| s.cancel_in_flight())
            .map_or(Effect::None, Effect::CancelQuery);
        let close = t
            .source
            .take()
            .and_then(|s| s.shut())
            .map_or(Effect::None, Effect::CloseSession);
        if let Some(phase) = phase {
            t.phase = phase;
        }
        if let Some(gate) = self.queries.get_mut(&tab) {
            gate.dismiss();
        }
        join(cancel, close)
    }

    /// Stops the agents of `conn` (AI was turned off for it).
    pub(super) fn stop_agents_of(&mut self, conn: &ConnectionId) -> Effect {
        let tabs: Vec<TabId> = self
            .agents
            .tabs
            .iter()
            .filter(|(_, t)| &t.conn == conn && t.is_live_or_starting())
            .map(|(id, _)| *id)
            .collect();
        tabs.into_iter()
            .map(|tab| {
                let stopped = AgentPhase::Stopped(AgentFailure::plain(AGENT_STOPPED_AI_OFF));
                self.halt_agent(tab, Some(stopped))
            })
            .fold(Effect::None, join)
    }

    /// The tab is going away: stops its agent and forgets the state. A stop that is still
    /// running on tokio finishes there.
    pub(super) fn close_agent(&mut self, tab: TabId) -> Effect {
        let effect = self.halt_agent(tab, None);
        self.agents.tabs.remove(&tab);
        effect
    }

    /// The app is quitting: closes every agent and returns whether one was live, with the
    /// handles of the statements in flight.
    pub(super) fn stop_all_agents(&mut self) -> (bool, Vec<CancelHandle>) {
        let mut live = false;
        let mut handles = Vec::new();
        for t in self.agents.tabs.values_mut() {
            if let Some(control) = t.control.take() {
                control.close();
                live = true;
            }
            // A tab that is still starting has no control yet: abort the start, so the process
            // it launched is killed, and let the late `agent_ready` close what it adopts.
            if let Some(abort) = t.abort.take() {
                abort.fire();
            }
            if t.phase == AgentPhase::Starting {
                t.phase = AgentPhase::Stopped(AgentFailure::plain(AGENT_STOPPED_QUIT));
                live = true;
            }
            t.endpoint_url = None;
            t.approvals.deny_all();
            t.permissions.cancel_all();
            handles.extend(t.source.as_ref().and_then(|s| s.cancel_in_flight()));
        }
        (live, handles)
    }

    /// Shuts every agent's connection source and returns the idle sessions to close.
    pub(super) fn shut_agent_sources(&mut self) -> Vec<Box<dyn Session>> {
        self.agents
            .tabs
            .values()
            .filter_map(|t| t.source.as_ref().and_then(|s| s.shut()))
            .collect()
    }

    /// A ready agent tab with this conversation, for the screenshot scenes and tests. No process
    /// runs: the tab is `Ready` and has no control.
    #[cfg(any(test, feature = "screenshots"))]
    pub fn agent_fixture(
        &mut self,
        conn: &ConnectionId,
        profile: AgentProfile,
        messages: Vec<AgentMsg>,
    ) -> Option<TabId> {
        let info = self.agent_info(conn)?;
        let index = self.sidebar.index_of(conn)?;
        let status = ai_status(self.ai.config(), Some(self.sidebar.profile(conn)?.spec()));
        let connection = tab_connection(self.sidebar.entries()[index].config());
        let tab = self.tabs.open(connection, TabKind::Agent);
        self.tab_conns.insert(tab, conn.clone());
        self.queries.insert(tab, QueryTab::new());
        let mut agent = AgentTab::new(conn.clone(), info, status);
        agent.profile = Some(profile);
        agent.phase = AgentPhase::Ready;
        self.agents.tabs.insert(tab, agent);
        for message in messages {
            let _ = self.agent_message(tab, message);
        }
        Some(tab)
    }
}
