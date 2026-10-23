// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of plugin write approvals (AC6): drains the asks the app's approver sends, keeps the Deny button
//! focused while a dialog is up and routes the answers to the [`WriteQueue`](crate::plugins::WriteQueue).

use std::time::{Duration, Instant};

use gpui::{Context, FocusHandle, Window};
use sqler_config::{KeyChord, Modifiers};
use tokio::sync::mpsc::UnboundedReceiver;

use super::WorkspaceView;
use crate::plugins::{PluginWriteAsk, WriteQueue, WriteStage};

/// What the approval dialog owns on the `GPUI` side.
pub(crate) struct WriteUi {
    /// The Deny wrapper: focused when a dialog opens.
    focus: FocusHandle,
    /// The ask and stage the focus was prepared for.
    focused: Option<(u64, WriteStage)>,
    /// When the ask and stage on screen became visible.
    shown: Option<(u64, WriteStage, Instant)>,
}

/// How long Approve is dead after a dialog appears: a plugin chooses when its dialog shows, so a click meant for
/// the grid or the editor must not land on it.
const ARMING_DELAY: Duration = Duration::from_millis(500);

impl WriteUi {
    pub(crate) fn new(cx: &mut Context<WorkspaceView>) -> Self {
        Self {
            focus: cx.focus_handle(),
            focused: None,
            shown: None,
        }
    }

    /// Whether Approve of `id` at `stage` counts at `now`: the dialog must have been visible for [`ARMING_DELAY`].
    fn armed(&self, id: u64, stage: WriteStage, now: Instant) -> bool {
        is_armed(self.shown, id, stage, now)
    }
}

/// Moves the notice of `queue`, if any, to `notices`.
fn drain_notice(queue: &mut WriteQueue, notices: &mut Vec<String>) {
    if let Some(notice) = queue.take_notice() {
        notices.push(notice);
    }
}

fn is_armed(
    shown: Option<(u64, WriteStage, Instant)>,
    id: u64,
    stage: WriteStage,
    now: Instant,
) -> bool {
    shown.is_some_and(|(i, s, since)| {
        i == id && s == stage && now.saturating_duration_since(since) >= ARMING_DELAY
    })
}

impl WorkspaceView {
    /// Moves the asks that arrive on `inbox` into the queue, one by one, for as long as the window lives.
    pub(super) fn drain_plugin_writes(
        mut inbox: UnboundedReceiver<PluginWriteAsk>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(ask) = inbox.recv().await {
                let alive = this.update(cx, |view, cx| {
                    view.ws.plugin_writes_mut().push(ask);
                    view.drain_write_notice();
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The handle of the Deny wrapper of the approval dialog.
    pub(crate) fn plugin_write_focus(&self) -> &FocusHandle {
        &self.plugin_writes_ui.focus
    }

    /// Whether Approve of the dialog on screen counts yet (the button is drawn disabled until then).
    pub(crate) fn plugin_write_armed(&self) -> bool {
        self.ws.plugin_writes().current().is_some_and(|p| {
            self.plugin_writes_ui
                .armed(p.id, p.request.stage, Instant::now())
        })
    }

    /// Moves the reason of an ask the queue refused without showing it to the notices.
    fn drain_write_notice(&mut self) {
        drain_notice(self.ws.plugin_writes_mut(), &mut self.notices);
    }

    /// Whether an approval dialog is on screen.
    pub(super) fn plugin_write_open(&self) -> bool {
        self.ws.plugin_writes().is_waiting()
    }

    /// Answers ask `id` at `stage`. A stale id does nothing, and neither does an Approve before the dialog is armed.
    pub(crate) fn plugin_write_answer(
        &mut self,
        id: u64,
        stage: WriteStage,
        approve: bool,
        cx: &mut Context<Self>,
    ) {
        if approve && !self.plugin_writes_ui.armed(id, stage, Instant::now()) {
            return;
        }
        self.ws.plugin_writes_mut().answer(id, stage, approve);
        cx.notify();
    }

    /// Escape denies the ask on screen; no key is ever Approve. Returns whether the key was taken.
    pub(super) fn plugin_write_key(&mut self, chord: &KeyChord, cx: &mut Context<Self>) -> bool {
        if !self.plugin_write_open()
            || chord.key() != "escape"
            || chord.modifiers() != Modifiers::default()
        {
            return false;
        }
        self.ws.plugin_writes_mut().deny_current();
        cx.notify();
        true
    }

    /// Drops the asks nobody waits for, and moves the focus to Deny when a new dialog opens (back to the workspace
    /// when the last one closes).
    pub(super) fn sync_plugin_writes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ws.plugin_writes_mut().prune_closed();
        self.drain_write_notice();
        let now = self
            .ws
            .plugin_writes()
            .current()
            .map(|p| (p.id, p.request.stage));
        if now == self.plugin_writes_ui.focused {
            return;
        }
        self.plugin_writes_ui.focused = now;
        self.plugin_writes_ui.shown = now.map(|(id, stage)| (id, stage, Instant::now()));
        if now.is_some() {
            window.focus(&self.plugin_writes_ui.focus, cx);
            // Approve is drawn disabled until armed: draw it again when the delay is over.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ARMING_DELAY).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        } else {
            window.focus(&self.focus, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::PluginWriteRequest;

    fn ask(sql: &str, stage: WriteStage) -> (PluginWriteAsk, tokio::sync::oneshot::Receiver<bool>) {
        let (reply, rx) = tokio::sync::oneshot::channel();
        let request = PluginWriteRequest::new(
            "acme-sync".parse().unwrap(),
            "Acme Sync",
            "Orders",
            sqler_driver_api::sqler_core::DriverId::new("sqlite").unwrap(),
            true,
            sql,
            stage,
        );
        (PluginWriteAsk { request, reply }, rx)
    }

    #[test]
    fn a_queued_ask_denied_by_the_gate_leaves_a_notice() {
        let mut queue = WriteQueue::default();
        let (first, _r1) = ask("update t set a = 1", WriteStage::First);
        let (second, mut r2) = ask("select 1", WriteStage::ProdSecond);
        let id = queue.push(first);
        queue.push(second);
        let mut notices = Vec::new();
        drain_notice(&mut queue, &mut notices);
        assert_eq!(notices.len(), 0);
        // Answering the first brings the second to the front, where the gate refuses it.
        assert!(queue.answer(id, WriteStage::First, false));
        assert_eq!(r2.try_recv(), Ok(false));
        drain_notice(&mut queue, &mut notices);
        assert_eq!(notices.len(), 1);
        drain_notice(&mut queue, &mut notices);
        assert_eq!(notices.len(), 1);
    }

    #[test]
    fn approve_waits_for_the_arming_delay() {
        let t0 = Instant::now();
        let shown = Some((3, WriteStage::First, t0));
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let delay = u64::try_from(ARMING_DELAY.as_millis()).unwrap();
        assert!(!is_armed(shown, 3, WriteStage::First, t0));
        assert!(!is_armed(shown, 3, WriteStage::First, at(delay - 1)));
        assert!(is_armed(shown, 3, WriteStage::First, at(delay)));
        // Another ask or stage is a new dialog and is not armed by this one's time.
        assert!(!is_armed(shown, 4, WriteStage::First, at(delay)));
        assert!(!is_armed(shown, 3, WriteStage::ProdSecond, at(delay)));
        assert!(!is_armed(None, 3, WriteStage::First, at(delay)));
    }
}
