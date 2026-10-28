// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the AI session tab (ADR-0053): the composer input and the Deny focus of
//! each open session tab, and the two effects. The session is built and each turn runs on tokio
//! through the bridge (ADR-0007), never on the UI thread; everything the session says comes back
//! through one ordered host channel that is drained into `Workspace::ai_session_message`.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, FocusHandle, ScrollHandle, Subscription, Window};
use gpui_component::input::{InputEvent, TextareaState};
use sqler_ai_tools::ApprovalStage;
use sqler_config::{ChatHistory, ChatHistoryStatus, ConversationId};
use tokio::sync::mpsc::UnboundedReceiver;

use super::WorkspaceView;
use crate::ai::AiAction;
use crate::ai::session::{AiSessionStart, AiTurnJob, HostMsg, build_session};
use crate::tabs::{TabId, TabKind};
use crate::workbench::Region;
use crate::workspace::Effect;

/// What one open AI session tab keeps on the view side.
struct SessionUi {
    composer: Entity<TextareaState>,
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

/// How long quitting waits for the chat history to commit and close.
const CHAT_HISTORY_CLOSE: std::time::Duration = std::time::Duration::from_millis(100);
/// The fewest rows the composer shows, empty or not.
pub(crate) const COMPOSER_MIN_ROWS: usize = 2;
/// The most rows the composer grows to; longer text scrolls inside it.
pub(crate) const COMPOSER_MAX_ROWS: usize = 8;

/// The state of an AI composer: a multi-line field that grows with its text, where a plain Enter
/// emits `PressEnter` without a newline and Shift+Enter inserts one.
pub(crate) fn composer_state(
    placeholder: String,
    window: &mut Window,
    cx: &mut Context<TextareaState>,
) -> TextareaState {
    TextareaState::new(window, cx)
        .auto_grow(COMPOSER_MIN_ROWS, COMPOSER_MAX_ROWS)
        .submit_on_enter(true)
        .placeholder(placeholder)
}

/// Whether `event` sends the message: a plain Enter only. Shift+Enter also emits `PressEnter`
/// (after it inserted the newline) and must not send.
pub(crate) fn composer_sends(event: &InputEvent) -> bool {
    matches!(event, InputEvent::PressEnter { shift: false, .. })
}

impl WorkspaceView {
    /// The composer input of an AI session tab.
    pub(crate) fn ai_composer(&self, tab: TabId) -> Option<&Entity<TextareaState>> {
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
        let send = if tab == TabId::CHAT_PANEL {
            AiAction::SendChat
        } else {
            AiAction::SendMessage(tab)
        };
        self.dispatch_ai(send, cx);
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
        let mut open: Vec<TabId> = self
            .ws
            .tabs()
            .iter()
            .filter(|t| t.kind() == TabKind::AiSession)
            .map(crate::tabs::Tab::id)
            .filter(|id| self.ws.ai_session(*id).is_some())
            .collect();
        // The chat panel has no strip tab; its composer lives as long as the chat does.
        if self.ws.chat().is_some() {
            open.push(TabId::CHAT_PANEL);
        }
        self.ai_session_inputs
            .tabs
            .retain(|id, _| open.contains(id));
        let active = self.ws.tabs().active().map(crate::tabs::Tab::id);
        // A hidden panel must not keep the focus: the keys would go nowhere.
        if !self.ws.layout().chat_visible() && self.chat_focus.contains_focused(window, cx) {
            self.focus_region(Region::Editor, window, cx);
        }
        for id in open {
            if !self.ai_session_inputs.tabs.contains_key(&id) {
                let name = self
                    .ws
                    .ai_session(id)
                    .map(|s| s.info().name.clone())
                    .unwrap_or_default();
                let ui = Self::new_session_ui(id, &name, window, cx);
                // The unsent draft of the last run fills the new chat's composer. The change
                // it causes sets the same draft again, which is harmless.
                let restored = (id == TabId::CHAT_PANEL)
                    .then(|| self.ws.chat().map(|c| c.draft().to_owned()))
                    .flatten()
                    .filter(|d| !d.is_empty());
                if let Some(text) = restored {
                    ui.composer
                        .update(cx, |input, cx| input.set_value(text, window, cx));
                }
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
        let composer = cx.new(|cx| composer_state(placeholder, window, cx));
        let subscription = cx.subscribe_in(
            &composer,
            window,
            move |view, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    let draft = if tab == TabId::CHAT_PANEL {
                        AiAction::SetChatDraft(text)
                    } else {
                        AiAction::SetSessionDraft(tab, text)
                    };
                    view.dispatch_ai(draft, cx);
                }
                e @ InputEvent::PressEnter { .. } if composer_sends(e) => {
                    view.ai_send(tab, window, cx);
                }
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

    /// Fills the composer of a session tab or the chat panel and sets the draft (a scene types
    /// this way).
    #[cfg(feature = "screenshots")]
    pub(super) fn ai_type_draft(
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
        let draft = if tab == TabId::CHAT_PANEL {
            AiAction::SetChatDraft(text.to_owned())
        } else {
            AiAction::SetSessionDraft(tab, text.to_owned())
        };
        self.dispatch_ai(draft, cx);
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
        self.ai_type_draft(tab, text, window, cx);
        self.ai_send(tab, window, cx);
    }

    /// Carries out the AI session effects.
    pub(super) fn apply_ai_session(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::StartAiSession { start, host } => self.start_ai_session(*start, host, cx),
            Effect::RunAiTurn(job) => self.run_ai_turn(*job, cx),
            Effect::ListChats => self.list_chats(cx),
            Effect::LoadChat(id) => self.load_chat(id, cx),
            _ => {}
        }
    }

    /// Reads the saved chats on the background executor (the store blocks) and shows them.
    fn list_chats(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.ws.chat_history().cloned() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { store.list() })
                .await;
            this.update(cx, |view, cx| {
                view.ws.chat_history_listed(result);
                view.push_chat_history_notice();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads one saved chat on the background executor and opens it.
    fn load_chat(&mut self, id: ConversationId, cx: &mut Context<Self>) {
        let Some(store) = self.ws.chat_history().cloned() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let wanted = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { store.load(&wanted) })
                .await;
            this.update(cx, |view, cx| {
                let effect = view.ws.chat_loaded(id, result);
                view.apply(effect, cx);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Reads the list once at start, so a history that was moved aside or is off is announced
    /// at launch and not only when the list is opened.
    pub(super) fn probe_chat_history(store: ChatHistory, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let status = cx
                .background_executor()
                .spawn(async move {
                    let _ = store.list();
                    store.status()
                })
                .await;
            this.update(cx, |view, cx| {
                if let ChatHistoryStatus::Off(text) = status {
                    view.notices.push(text);
                }
                view.push_chat_history_notice();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Moves the store's one-time notice (a corrupt file was moved aside) to the notices.
    fn push_chat_history_notice(&mut self) {
        if let Some(notice) = self.ws.chat_history().and_then(ChatHistory::take_notice) {
            self.notices.push(notice);
        }
    }

    /// The app is quitting: commits and closes the chat history, waiting a moment at most.
    pub(super) fn close_chat_history(&self) {
        if let Some(store) = self.ws.chat_history() {
            store.close(CHAT_HISTORY_CLOSE);
        }
    }

    /// Builds the session on tokio and follows its host channel until the tab is gone.
    fn start_ai_session(
        &mut self,
        start: AiSessionStart,
        mut host: UnboundedReceiver<HostMsg>,
        cx: &mut Context<Self>,
    ) {
        let tab = start.tab;
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
        ParentElement as _, Pixels, Render, Styled as _, TestAppContext, VisualTestContext, Window,
        div, px,
    };
    use gpui_component::button::Button;
    use gpui_component::input::{InputEvent, InputState, Textarea, TextareaState};
    use sqler_config::CHAT_PANEL_SIZE_RANGE;

    use super::{composer_sends, composer_state};
    use crate::ai::session::view::composer_row;

    /// Reproduction (F4/AC1): the old composer was a single-line `InputState`, which cannot hold
    /// a newline, so Shift+Enter could never start a new line.
    #[gpui::test]
    fn the_old_single_line_composer_cannot_hold_a_newline(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            struct Old(Entity<InputState>);
            impl Render for Old {
                fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                    div()
                        .size_full()
                        .child(gpui_component::input::Input::new(&self.0))
                }
            }
            Old(cx.new(|cx| InputState::new(window, cx)))
        });
        let input = state.read_with(cx, |o, _| o.0.clone());
        cx.update(|window, cx| {
            let handle = input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        });
        cx.simulate_input("a");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("b");
        let value = cx.update(|_, cx| input.read(cx).value().to_string());
        assert!(
            !value.contains('\n'),
            "the single-line input held {value:?}"
        );
    }

    /// A composer row of a set width with a send button, counting the sends.
    struct Host {
        state: Entity<TextareaState>,
        width: Pixels,
        _sub: gpui::Subscription,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(self.width).child(
                composer_row(
                    div()
                        .debug_selector(|| "field".to_owned())
                        .child(Textarea::new(&self.state)),
                    div()
                        .debug_selector(|| "send".to_owned())
                        .child(Button::new("send").label("Send")),
                )
                .debug_selector(|| "row".to_owned()),
            )
        }
    }

    fn host(cx: &mut TestAppContext) -> (Entity<Host>, Rc<Cell<usize>>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::editor::view::init_keys(cx);
        });
        let sends = Rc::new(Cell::new(0));
        let counted = Rc::clone(&sends);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| composer_state("Ask".to_owned(), window, cx));
            let sub = cx.subscribe_in(
                &state,
                window,
                move |_: &mut Host, _, event: &InputEvent, _, _| {
                    if composer_sends(event) {
                        counted.set(counted.get() + 1);
                    }
                },
            );
            Host {
                state,
                width: px(400.),
                _sub: sub,
            }
        });
        let state = view.read_with(cx, |h, _| h.state.clone());
        cx.update(|window, cx| {
            let handle = state.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        });
        (view, sends, cx)
    }

    fn value(view: &Entity<Host>, cx: &mut VisualTestContext) -> String {
        let state = view.read_with(cx, |h, _| h.state.clone());
        cx.update(|_, cx| state.read(cx).value().to_string())
    }

    #[gpui::test]
    fn enter_sends_and_inserts_no_newline(cx: &mut TestAppContext) {
        let (view, sends, cx) = host(cx);
        cx.simulate_input("hello");
        cx.simulate_keystrokes("enter");
        assert_eq!(sends.get(), 1);
        assert_eq!(value(&view, cx), "hello");
    }

    #[gpui::test]
    fn shift_enter_inserts_a_newline_and_does_not_send(cx: &mut TestAppContext) {
        let (view, sends, cx) = host(cx);
        cx.simulate_input("a");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("b");
        assert_eq!(sends.get(), 0);
        assert_eq!(value(&view, cx), "a\nb");
        cx.simulate_keystrokes("enter");
        assert_eq!(sends.get(), 1);
        assert_eq!(value(&view, cx), "a\nb");
    }

    #[test]
    fn composer_sends_only_plain_enter() {
        let enter = |shift| InputEvent::PressEnter {
            secondary: false,
            shift,
        };
        assert!(composer_sends(&enter(false)));
        assert!(!composer_sends(&enter(true)));
        assert!(!composer_sends(&InputEvent::Change));
    }

    fn lines(view: &Entity<Host>, cx: &mut VisualTestContext, n: usize) -> Pixels {
        let state = view.read_with(cx, |h, _| h.state.clone());
        let text = vec!["line"; n].join("\n");
        cx.update(|window, cx| state.update(cx, |s, cx| s.set_value(text, window, cx)));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.debug_bounds("field").expect("the field").size.height
    }

    #[gpui::test]
    fn the_composer_grows_then_stops_at_the_max(cx: &mut TestAppContext) {
        let (view, _, cx) = host(cx);
        cx.run_until_parked();
        let h0 = cx.debug_bounds("field").expect("the field").size.height;
        let h4 = lines(&view, cx, 4);
        let h20 = lines(&view, cx, 20);
        let h30 = lines(&view, cx, 30);
        assert!(h0 < h4, "{h0:?} < {h4:?}");
        assert!(h4 < h20, "{h4:?} < {h20:?}");
        assert!((h20 - h30).abs() <= px(1.), "{h20:?} vs {h30:?}");
    }

    #[gpui::test]
    fn the_composer_fits_at_every_chat_width(cx: &mut TestAppContext) {
        let (view, _, cx) = host(cx);
        let tolerance = px(1.);
        for width in [
            *CHAT_PANEL_SIZE_RANGE.start(),
            380.,
            *CHAT_PANEL_SIZE_RANGE.end(),
        ] {
            view.update(cx, |h, cx| {
                h.width = px(width);
                cx.notify();
            });
            lines(&view, cx, 3);
            let row = cx.debug_bounds("row").expect("the row");
            let field = cx.debug_bounds("field").expect("the field");
            let send = cx.debug_bounds("send").expect("the button");
            assert!(
                send.right() <= row.right() + tolerance,
                "{width}: {send:?} in {row:?}"
            );
            assert!(field.size.width > px(0.), "{width}: {field:?}");
            assert!(
                (send.bottom() - row.bottom()).abs() <= tolerance,
                "{width}: {send:?} in {row:?}"
            );
        }
    }
}
