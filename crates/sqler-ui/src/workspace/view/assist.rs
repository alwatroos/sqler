// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the editor AI actions (ADR-0052): the prompt-bar inputs and the runner of
//! `Effect::RunAssist`. The request runs on tokio through the bridge; the answer comes back to
//! `Workspace::assist_finished`. Nothing here runs SQL; generated text only enters the editor as
//! an `EditorEdit`.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, Focusable as _, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};

use super::WorkspaceView;
use crate::ai::{AiAction, AiProviderFactory, AssistJob, run_assist};
use crate::tabs::TabId;
use crate::workspace::Effect;

/// The text shown when the task behind a request is lost.
const LOST: &str = "The AI request was lost.";

/// The placeholder of the prompt bar.
const PLACEHOLDER: &str = "Describe the query you need…";

/// One `InputState` per open prompt bar.
#[derive(Default)]
pub(crate) struct AssistInputs {
    inputs: HashMap<TabId, (Entity<InputState>, Subscription)>,
}

impl AssistInputs {
    /// The input of the prompt bar of `tab`.
    pub(crate) fn get(&self, tab: TabId) -> Option<&Entity<InputState>> {
        self.inputs.get(&tab).map(|(input, _)| input)
    }
}

impl WorkspaceView {
    /// The input of the prompt bar of `tab`, while the bar is open.
    pub(crate) fn assist_input(&self, tab: TabId) -> Option<&Entity<InputState>> {
        self.assist_inputs.get(tab)
    }

    /// Makes the inputs match the open prompt bars: one is made (and focused) when a bar opens
    /// and dropped when it closes, which gives the focus back to the editor.
    pub(super) fn sync_assist_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open: Vec<TabId> = self
            .ws
            .tabs()
            .iter()
            .map(crate::tabs::Tab::id)
            .filter(|tab| self.ws.assist(*tab).is_some_and(|a| a.prompt.is_some()))
            .collect();
        let closed: Vec<TabId> = self
            .assist_inputs
            .inputs
            .keys()
            .copied()
            .filter(|tab| !open.contains(tab))
            .collect();
        for tab in closed {
            self.assist_inputs.inputs.remove(&tab);
            if self.ws.tabs().active().map(crate::tabs::Tab::id) == Some(tab) {
                self.editors.focus(tab, window, cx);
            }
        }
        for tab in open {
            if self.assist_inputs.inputs.contains_key(&tab) {
                continue;
            }
            let text = self
                .ws
                .assist(tab)
                .and_then(|a| a.prompt.as_ref())
                .map(|bar| bar.text.clone())
                .unwrap_or_default();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(PLACEHOLDER)
                    .default_value(text)
            });
            let sub = cx.subscribe(&input, |view, input, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.dispatch_ai(AiAction::SetPrompt(text), cx);
                }
                InputEvent::PressEnter { .. } => view.dispatch_ai(AiAction::SubmitPrompt, cx),
                _ => {}
            });
            window.focus(&input.read(cx).focus_handle(cx), cx);
            self.assist_inputs.inputs.insert(tab, (input, sub));
        }
    }

    /// Applies the queued editor edits once the frame is done: an edit needs the window, which
    /// the async answer and the button clicks do not have.
    pub(super) fn flush_assist_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.assist_edits.is_empty() {
            return;
        }
        let edits = std::mem::take(&mut self.assist_edits);
        let weak = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            weak.update(cx, |view, cx| {
                for edit in edits {
                    let tab = match &edit {
                        Effect::EditorEdit { tab, .. } => Some(*tab),
                        _ => None,
                    };
                    if let Some(rest) = view.editors.apply(edit, window, cx) {
                        view.apply(rest, cx);
                    }
                    // The text is there to review: the editor takes the focus.
                    if let Some(tab) = tab {
                        view.editors.focus(tab, window, cx);
                    }
                }
                view.settle(cx);
            })
            .ok();
        });
    }

    /// Runs a request on tokio and reports the answer with the editor text at that moment.
    pub(super) fn run_assist_effect(&mut self, mut job: Box<AssistJob>, cx: &mut Context<Self>) {
        job.audit = self
            .audit_log
            .clone()
            .map(|log| log as Arc<dyn sqler_ai_tools::AuditSink>);
        let id = job.id;
        let (cancel, signal) = tokio::sync::oneshot::channel();
        self.assist_cancels.insert(id, cancel);
        let factory: Arc<dyn AiProviderFactory> = Arc::clone(&self.ai_factory);
        let done = self.bridge.spawn(run_assist(*job, factory, signal));
        cx.spawn(async move |this, cx| {
            let result = done.await.unwrap_or_else(|_| Err(LOST.to_owned()));
            this.update(cx, |view, cx| {
                view.assist_cancels.remove(&id);
                let buffer = view
                    .ws
                    .assist_request_tab(id)
                    .and_then(|tab| view.editors.buffer(tab, cx));
                let effect = view.ws.assist_finished(id, result, buffer);
                view.apply(effect, cx);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Fires the cancel signal of request `id`; the runner then drops the provider stream.
    pub(super) fn cancel_assist_effect(&mut self, id: u64) {
        if let Some(cancel) = self.assist_cancels.remove(&id) {
            let _ = cancel.send(());
        }
    }
}
