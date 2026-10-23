// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the update check (ADR-0066): the first-frame start, the inbox of the
//! service and the carrying out of its effects.
//!
//! Nothing starts before the first frame has been drawn, and the service is only called for what
//! the model asks for: while the setting is off that is never a check.

use gpui::{Context, Window};
use tokio::sync::mpsc::UnboundedReceiver;

use super::WorkspaceView;
use crate::update::{UpdateCommand, UpdateEffect, UpdateEvent};

impl WorkspaceView {
    /// Schedules the start of the update service for after the first frame was drawn. A frame
    /// callback runs just before the frame it was asked for, so the start waits for the second.
    pub(super) fn schedule_update(window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, _| {
            let weak = weak.clone();
            window.on_next_frame(move |_, cx| {
                weak.update(cx, WorkspaceView::start_update).ok();
            });
        });
    }

    /// Arms the update service with the current setting. Only after the first frame; the scenes
    /// call it after their first draw, as the headless renderer delivers no frame callbacks.
    pub(super) fn start_update(&mut self, cx: &mut Context<Self>) {
        let effect = self.ws.update_first_frame();
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Moves the reports of the service into the workspace, one by one, for as long as the
    /// window lives.
    pub(super) fn drain_update_events(
        mut inbox: UnboundedReceiver<UpdateEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(event) = inbox.recv().await {
                let alive = this.update(cx, |view, cx| {
                    let effect = view.ws.update_event(event);
                    view.apply(effect, cx);
                    view.settle(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Runs an update command (the palette, the status bar and the Settings page all come here).
    pub(crate) fn run_update_command(&mut self, command: UpdateCommand, cx: &mut Context<Self>) {
        let effect = self.ws.dispatch_update(command);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Calls the service or opens the release notes. A build without a service does nothing.
    pub(super) fn apply_update(&mut self, effect: UpdateEffect, cx: &mut Context<Self>) {
        if let UpdateEffect::OpenUrl(url) = &effect {
            cx.open_url(url);
            return;
        }
        let Some(service) = self.update_service.as_ref() else {
            return;
        };
        match effect {
            UpdateEffect::Start(enabled) => service.start(enabled),
            UpdateEffect::SetEnabled(enabled) => service.set_enabled(enabled),
            UpdateEffect::CheckNow => service.check_now(),
            UpdateEffect::Dismiss(version) => service.dismiss(&version),
            UpdateEffect::OpenUrl(_) => {}
        }
    }
}
