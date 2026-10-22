// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the agent tab (ADR-0051): the composer input and the focus handles of each
//! open agent tab, and the two effects. The agent starts on tokio through the bridge (ADR-0007),
//! never on the UI thread; everything it does comes back through one ordered host channel that is
//! drained into `Workspace::agent_message`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, ScrollHandle, Subscription, Task, Window,
};
use gpui_component::input::{InputEvent, InputState};
use sqler_acp::ExitInfo;
use sqler_ai_tools::AuditSink;
use sqler_config::ConfigDir;
use std::collections::HashMap;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;

use super::WorkspaceView;
use crate::ai::AiAction;
use crate::ai::agent::{AgentMsg, AgentProfiles, AgentStart, load_agent_profiles, start_agent};
use crate::tabs::{TabId, TabKind};
use crate::workspace::Effect;

/// The folder an agent works in, below the config directory; without one, below the temp folder.
const AGENT_DIR: &str = "agent-workspace";
const AGENT_TEMP_DIR: &str = "sqler-agent";

/// What one open agent tab keeps on the view side.
struct AgentUi {
    composer: Entity<InputState>,
    /// The Deny button's focus on the statement card: its default focus.
    deny: FocusHandle,
    /// The Reject button's focus on the permission card: its default focus.
    reject: FocusHandle,
    scroll: ScrollHandle,
    /// The transcript length last scrolled to.
    seen: usize,
    /// The statement approval and the permission request whose cards already took the focus.
    asked: (Option<u64>, Option<u64>),
    _subscription: Subscription,
}

/// The inputs, the host-channel tasks and the runs of the agent tabs.
#[derive(Default)]
pub(crate) struct AgentViews {
    tabs: HashMap<TabId, AgentUi>,
    /// One task per started agent: it starts it, reports `agent_ready` and follows the host
    /// channel. Dropping it (the tab closed, or the agent restarted) drops the channel, so every
    /// ask is denied and the agent is torn down.
    drains: HashMap<TabId, Task<()>>,
    /// Resolve with the exit of an agent once it and its endpoint are torn down.
    runs: Vec<oneshot::Receiver<ExitInfo>>,
}

/// The working folder of an agent session.
fn agent_cwd(dir: Option<&ConfigDir>) -> PathBuf {
    dir.map_or_else(
        || std::env::temp_dir().join(AGENT_TEMP_DIR),
        |dir| dir.root().join(AGENT_DIR),
    )
}

impl WorkspaceView {
    /// The composer input of an agent tab.
    pub(crate) fn agent_composer(&self, tab: TabId) -> Option<&Entity<InputState>> {
        self.agent_views.tabs.get(&tab).map(|ui| &ui.composer)
    }

    /// The focus of the Reject button of the permission card of `tab`.
    pub(crate) fn agent_reject_focus(&self, tab: TabId) -> Option<&FocusHandle> {
        self.agent_views.tabs.get(&tab).map(|ui| &ui.reject)
    }

    /// The scroll position of the transcript of `tab`.
    pub(crate) fn agent_scroll(&self, tab: TabId) -> Option<&ScrollHandle> {
        self.agent_views.tabs.get(&tab).map(|ui| &ui.scroll)
    }

    /// The focus of the Deny button of the statement card of an agent tab.
    pub(super) fn agent_deny_focus(&self, tab: TabId) -> Option<&FocusHandle> {
        self.agent_views.tabs.get(&tab).map(|ui| &ui.deny)
    }

    /// Sends the composer's text: the workspace takes the draft, and the input is cleared when
    /// the prompt was taken.
    pub(crate) fn agent_send(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch_ai(AiAction::SendAgentPrompt(tab), cx);
        let taken = self.ws.agent(tab).is_some_and(|a| a.draft().is_empty());
        if taken && let Some(ui) = self.agent_views.tabs.get(&tab) {
            ui.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Makes the inputs match the open agent tabs: one composer and the focus handles per tab,
    /// dropped with the tab. A new card takes the focus on its safe button; a growing transcript
    /// scrolls to its end.
    pub(super) fn sync_agent_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open: Vec<TabId> = self
            .ws
            .tabs()
            .iter()
            .filter(|t| t.kind() == TabKind::Agent)
            .map(crate::tabs::Tab::id)
            .filter(|id| self.ws.agent(*id).is_some())
            .collect();
        self.agent_views.tabs.retain(|id, _| open.contains(id));
        let active = self.ws.tabs().active().map(crate::tabs::Tab::id);
        for id in open {
            if !self.agent_views.tabs.contains_key(&id) {
                let name = self
                    .ws
                    .agent(id)
                    .map(|a| a.info().name.clone())
                    .unwrap_or_default();
                let ui = Self::new_agent_ui(id, &name, window, cx);
                self.agent_views.tabs.insert(id, ui);
            }
            let Some(agent) = self.ws.agent(id) else {
                continue;
            };
            let length = agent.transcript().entries().len();
            let statement = agent
                .approvals()
                .current()
                .filter(|a| a.request.stage == sqler_ai_tools::ApprovalStage::Confirm)
                .map(|a| a.id);
            let permission = agent.permissions().current().map(|p| p.id);
            let Some(ui) = self.agent_views.tabs.get_mut(&id) else {
                continue;
            };
            if ui.seen != length {
                ui.seen = length;
                ui.scroll.scroll_to_bottom();
            }
            if (statement, permission) != ui.asked {
                let new_statement = statement != ui.asked.0;
                ui.asked = (statement, permission);
                if active == Some(id) {
                    if statement.is_some() && new_statement {
                        window.focus(&ui.deny, cx);
                    } else if permission.is_some() {
                        window.focus(&ui.reject, cx);
                    }
                }
            }
        }
    }

    fn new_agent_ui(
        tab: TabId,
        connection: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AgentUi {
        let placeholder = format!("Ask the agent about {connection}\u{2026}");
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(
            &composer,
            window,
            move |view, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.dispatch_ai(AiAction::SetAgentDraft(tab, text), cx);
                }
                InputEvent::PressEnter { .. } => view.agent_send(tab, window, cx),
                _ => {}
            },
        );
        AgentUi {
            composer,
            deny: cx.focus_handle(),
            reject: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            seen: 0,
            asked: (None, None),
            _subscription: subscription,
        }
    }

    /// Carries out the agent effects.
    pub(super) fn apply_agent(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::LoadAgentProfiles => self.load_agent_profiles_effect(cx),
            Effect::StartAgent { start, host } => self.start_agent_effect(*start, host, cx),
            _ => {}
        }
    }

    /// Reads `agents.toml` on the blocking pool.
    fn load_agent_profiles_effect(&mut self, cx: &mut Context<Self>) {
        let dir = self.ws.ai().dir().map(|dir| dir.root().to_path_buf());
        let done = self
            .bridge
            .blocking(move || load_agent_profiles(dir.as_deref()));
        cx.spawn(async move |this, cx| {
            let profiles = done.await.unwrap_or_else(|_| AgentProfiles::builtin());
            this.update(cx, |view, cx| {
                view.ws.agent_profiles_loaded(profiles);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Starts the agent on tokio, then follows its host channel until the tab is gone. The
    /// process ends only through the control, which the tab holds.
    fn start_agent_effect(
        &mut self,
        mut start: AgentStart,
        mut host: UnboundedReceiver<AgentMsg>,
        cx: &mut Context<Self>,
    ) {
        let tab = start.tab;
        start.audit = self.audit_log.clone().map(|log| log as Arc<dyn AuditSink>);
        start.cwd = agent_cwd(self.ws.ai().dir());
        let (exit_tx, exit_rx) = oneshot::channel();
        self.agent_views.runs.push(exit_rx);
        let connector = self.bridge.connector();
        let done = self.bridge.spawn(async move {
            match start_agent(start, connector).await {
                Ok(started) => {
                    let crate::ai::agent::AgentStarted { ready, finished } = started;
                    // Quitting waits for the exit, also when nobody adopts the started agent.
                    tokio::spawn(async move {
                        if let Ok(exit) = finished.await {
                            let _ = exit_tx.send(exit);
                        }
                    });
                    Ok(ready)
                }
                Err(failure) => Err(failure),
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let result = done
                .await
                .unwrap_or_else(|_| Err(crate::ai::AgentFailure::plain("the agent task was lost")));
            let alive = this.update(cx, |view, cx| {
                let effect = view.ws.agent_ready(tab, result);
                view.apply(effect, cx);
                view.settle(cx);
            });
            if alive.is_err() {
                return;
            }
            while let Some(first) = host.recv().await {
                let mut batch = vec![first];
                while let Ok(next) = host.try_recv() {
                    batch.push(next);
                }
                let alive = this.update(cx, |view, cx| {
                    for message in batch {
                        let effect = view.ws.agent_message(tab, message);
                        view.apply(effect, cx);
                    }
                    view.settle(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        self.agent_views.drains.insert(tab, task);
    }

    /// Forgets the host-channel tasks of closed tabs and the runs that ended.
    pub(super) fn prune_agents(&mut self) {
        let ws = &self.ws;
        self.agent_views
            .drains
            .retain(|tab, _| ws.agent(*tab).is_some());
        self.agent_views
            .runs
            .retain_mut(|run| matches!(run.try_recv(), Err(TryRecvError::Empty)));
    }

    /// Whether an agent has not finished its teardown.
    pub(super) fn agents_running(&mut self) -> bool {
        self.prune_agents();
        !self.agent_views.runs.is_empty()
    }

    /// Waits for the agents to exit, at most `limit`. Their tabs already closed them.
    pub(super) fn take_agent_runs(&mut self, limit: Duration) -> oneshot::Receiver<()> {
        let runs = std::mem::take(&mut self.agent_views.runs);
        let waits = runs
            .into_iter()
            .map(|run| {
                self.bridge.spawn(async move {
                    let _ = run.await;
                })
            })
            .collect();
        self.bridge.wait_all(waits, limit)
    }
}
