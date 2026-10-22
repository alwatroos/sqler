// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the AI session tab (ADR-0053): the composer input and the Deny focus of
//! each open session tab, and the two effects. The session is built and each turn runs on tokio
//! through the bridge (ADR-0007), never on the UI thread; everything the session says comes back
//! through one ordered host channel that is drained into `Workspace::ai_session_message`.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, FocusHandle, ScrollHandle, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use sqler_ai_tools::{ApprovalStage, AuditSink};
use tokio::sync::mpsc::UnboundedReceiver;

use super::WorkspaceView;
use crate::ai::AiAction;
use crate::ai::session::{AiSessionStart, AiTurnJob, HostMsg, build_session};
use crate::tabs::{TabId, TabKind};
use crate::workspace::Effect;

/// What one open AI session tab keeps on the view side.
struct SessionUi {
    composer: Entity<InputState>,
    /// The Deny button's focus: the default focus of an approval card.
    deny: FocusHandle,
    scroll: ScrollHandle,
    /// The transcript length and streamed length last scrolled to.
    seen: (usize, usize),
    /// The approval whose card already took the focus.
    asked: Option<u64>,
    _subscription: Subscription,
}

/// The composer and the Deny focus of every open AI session tab.
#[derive(Default)]
pub(crate) struct AiSessionInputs {
    tabs: HashMap<TabId, SessionUi>,
}

impl WorkspaceView {
    /// The composer input of an AI session tab.
    pub(crate) fn ai_composer(&self, tab: TabId) -> Option<&Entity<InputState>> {
        self.ai_session_inputs.tabs.get(&tab).map(|ui| &ui.composer)
    }

    /// The focus of the Deny button of the approval card of `tab`: an AI session tab's, else an
    /// agent tab's.
    pub(crate) fn ai_deny_focus(&self, tab: TabId) -> Option<&FocusHandle> {
        self.ai_session_inputs
            .tabs
            .get(&tab)
            .map(|ui| &ui.deny)
            .or_else(|| self.agent_deny_focus(tab))
    }

    /// The scroll position of the transcript of `tab`.
    pub(crate) fn ai_scroll(&self, tab: TabId) -> Option<&ScrollHandle> {
        self.ai_session_inputs.tabs.get(&tab).map(|ui| &ui.scroll)
    }

    /// Sends the composer's text: the workspace takes the draft, and the input is cleared when
    /// the message was taken.
    pub(crate) fn ai_send(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch_ai(AiAction::SendMessage(tab), cx);
        let taken = self
            .ws
            .ai_session(tab)
            .is_some_and(|s| s.draft().is_empty());
        if taken && let Some(ui) = self.ai_session_inputs.tabs.get(&tab) {
            ui.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Makes the inputs match the open session tabs: one composer and one Deny focus per tab,
    /// dropped with the tab. A new approval card takes the focus on Deny; a growing transcript
    /// scrolls to its end.
    pub(super) fn sync_ai_session_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open: Vec<TabId> = self
            .ws
            .tabs()
            .iter()
            .filter(|t| t.kind() == TabKind::AiSession)
            .map(crate::tabs::Tab::id)
            .filter(|id| self.ws.ai_session(*id).is_some())
            .collect();
        self.ai_session_inputs
            .tabs
            .retain(|id, _| open.contains(id));
        let active = self.ws.tabs().active().map(crate::tabs::Tab::id);
        for id in open {
            if !self.ai_session_inputs.tabs.contains_key(&id) {
                let name = self
                    .ws
                    .ai_session(id)
                    .map(|s| s.info().name.clone())
                    .unwrap_or_default();
                let ui = Self::new_session_ui(id, &name, window, cx);
                self.ai_session_inputs.tabs.insert(id, ui);
            }
            let Some(session) = self.ws.ai_session(id) else {
                continue;
            };
            let lengths = (session.entries().len(), session.streaming().len());
            let ask = session
                .approvals()
                .current()
                .filter(|a| a.request.stage == ApprovalStage::Confirm)
                .map(|a| a.id);
            let Some(ui) = self.ai_session_inputs.tabs.get_mut(&id) else {
                continue;
            };
            if ui.seen != lengths {
                ui.seen = lengths;
                ui.scroll.scroll_to_bottom();
            }
            if ask != ui.asked {
                ui.asked = ask;
                if ask.is_some() && active == Some(id) {
                    window.focus(&ui.deny, cx);
                }
            }
        }
    }

    fn new_session_ui(
        tab: TabId,
        connection: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> SessionUi {
        let placeholder = format!("Ask about the data in {connection}\u{2026}");
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(
            &composer,
            window,
            move |view, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.dispatch_ai(AiAction::SetSessionDraft(tab, text), cx);
                }
                InputEvent::PressEnter { .. } => view.ai_send(tab, window, cx),
                _ => {}
            },
        );
        SessionUi {
            composer,
            deny: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            seen: (0, 0),
            asked: None,
            _subscription: subscription,
        }
    }

    /// Fills the composer of a session tab (a scene types this way) and sends it.
    #[cfg(feature = "screenshots")]
    pub(super) fn ai_type_and_send(
        &mut self,
        tab: TabId,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ui) = self.ai_session_inputs.tabs.get(&tab) {
            ui.composer
                .update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        }
        self.dispatch_ai(AiAction::SetSessionDraft(tab, text.to_owned()), cx);
        self.ai_send(tab, window, cx);
    }

    /// Carries out the AI session effects.
    pub(super) fn apply_ai_session(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::StartAiSession { start, host } => self.start_ai_session(*start, host, cx),
            Effect::RunAiTurn(job) => self.run_ai_turn(*job, cx),
            _ => {}
        }
    }

    /// Builds the session on tokio and follows its host channel until the tab is gone.
    fn start_ai_session(
        &mut self,
        mut start: AiSessionStart,
        mut host: UnboundedReceiver<HostMsg>,
        cx: &mut Context<Self>,
    ) {
        let tab = start.tab;
        start.audit = self.audit_log.clone().map(|log| log as Arc<dyn AuditSink>);
        let factory = Arc::clone(&self.ai_factory);
        let done = self
            .bridge
            .spawn(build_session(start, factory, self.bridge.connector()));
        cx.spawn(async move |this, cx| {
            let result = done
                .await
                .unwrap_or_else(|_| Err("the session task was lost".to_owned()));
            this.update(cx, |view, cx| {
                let effect = view.ws.ai_session_ready(tab, result);
                view.apply(effect, cx);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Some(first) = host.recv().await {
                let mut batch = vec![first];
                while let Ok(next) = host.try_recv() {
                    batch.push(next);
                }
                let alive = this.update(cx, |view, cx| {
                    for message in batch {
                        let effect = view.ws.ai_session_message(tab, message);
                        view.apply(effect, cx);
                    }
                    view.settle(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Runs one turn on tokio; it reports through the host channel. A lost job frees the tab.
    fn run_ai_turn(&mut self, job: AiTurnJob, cx: &mut Context<Self>) {
        let (tab, turn) = (job.tab, job.turn);
        let done = self.bridge.spawn(job.run());
        cx.spawn(async move |this, cx| {
            if done.await.is_err() {
                this.update(cx, |view, cx| {
                    view.ws.ai_turn_lost(tab, turn);
                    view.settle(cx);
                })
                .ok();
            }
        })
        .detach();
    }
}
