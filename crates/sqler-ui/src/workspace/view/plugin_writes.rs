// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of plugin write approvals (AC6): drains the asks the app's approver sends, keeps the Deny button
//! focused while a dialog is up and routes the answers to the [`WriteQueue`](crate::plugins::WriteQueue).

use std::time::{Duration, Instant};

use gpui::{Context, FocusHandle, Window};
use sqler_config::{KeyChord, Modifiers};
use tokio::sync::mpsc::UnboundedReceiver;

use super::WorkspaceView;
use crate::plugins::{PluginWriteAsk, WriteStage};

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
                    if let Some(notice) = view.ws.plugin_writes_mut().take_notice() {
                        view.notices.push(notice);
                    }
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
        } else {
            window.focus(&self.focus, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
