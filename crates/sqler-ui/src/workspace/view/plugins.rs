// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the plugin manager (ADR-0060): the first-frame start, the work the manager asks for, the
//! permission dialog (focus, the typed-id field) and the editors of the settings.
//!
//! Every backend call and every read of the grants file runs on the background executor; nothing starts before the
//! first frame has been drawn. The dialog is drawn by [`grant_overlay`]; this host owns its focus handle and its
//! typed-id field, focuses the dialog when it opens and gives the focus back when it closes.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, Focusable as _, Subscription, Window,
};
use gpui_component::WindowExt as _;
use gpui_component::input::{InputEvent, InputState};
use sqler_plugin_api::{PluginId, SettingValue};

use super::WorkspaceView;
use crate::command::CommandId;
use crate::plugins::grant::GrantCommand;
use crate::plugins::grant::view::{GrantHandler, grant_overlay};
use crate::plugins::manager::{PluginDone, PluginEffect};

/// The editor of one text, integer or float setting.
struct SettingInput {
    input: Entity<InputState>,
    /// The text the field was last set to from the stored value.
    shown: String,
    _sub: Subscription,
}

/// What the manager owns on the `GPUI` side.
pub(crate) struct PluginsUi {
    /// The dialog container: focused when a dialog opens, it takes Escape.
    focus: FocusHandle,
    /// The typed-id field of the dialog.
    confirm: Entity<InputState>,
    /// The generation of the dialog the field and the focus were prepared for (0 = none).
    shown: u64,
    /// Setting editors of the selected plugin.
    settings_for: Option<PluginId>,
    settings: HashMap<String, SettingInput>,
    /// Re-reads the grants when the window becomes active.
    _activation: Subscription,
}

impl PluginsUi {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<WorkspaceView>) -> Self {
        let activation = cx.observe_window_activation(window, |view, window, cx| {
            if window.is_window_active() {
                let effect = view.ws.plugins_activated();
                view.apply(effect, cx);
                view.settle(cx);
            }
        });
        Self {
            focus: cx.focus_handle(),
            confirm: cx.new(|cx| InputState::new(window, cx)),
            shown: 0,
            settings_for: None,
            settings: HashMap::new(),
            _activation: activation,
        }
    }
}

/// The text a setting editor shows for `value`.
fn value_text(value: &SettingValue) -> String {
    match value {
        SettingValue::Integer(v) => v.to_string(),
        SettingValue::Float(v) => v.to_string(),
        SettingValue::Text(v) => v.clone(),
        SettingValue::Boolean(v) => v.to_string(),
        _ => String::new(),
    }
}

impl WorkspaceView {
    /// Schedules the start of the plugin manager for after the first frame was drawn. A frame callback runs just
    /// before the frame it was asked for, so the start waits for the second callback.
    pub(super) fn schedule_plugins(window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, _| {
            let weak = weak.clone();
            window.on_next_frame(move |_, cx| {
                weak.update(cx, WorkspaceView::start_plugins).ok();
            });
        });
    }

    /// Starts the plugin manager: read the grants and discover the plugins. Only after the first frame; the scenes
    /// call it after their first draw, as the headless renderer delivers no frame callbacks.
    pub(super) fn start_plugins(&mut self, cx: &mut Context<Self>) {
        let effect = self.ws.plugins_first_frame();
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Carries out work of the manager: blocking effects on the blocking pool, a start on tokio; the result goes back
    /// to the workspace, which says what to do next.
    pub(super) fn apply_plugins(&mut self, effect: PluginEffect, cx: &mut Context<Self>) {
        let Some(backend) = self.ws.plugins().backend().cloned() else {
            return;
        };
        let grants = std::sync::Arc::clone(self.ws.plugins().grants());
        let done = match effect {
            PluginEffect::Start(id) => {
                let started = id.clone();
                let pending = self.bridge.spawn(async move { backend.start(&id).await });
                cx.spawn(async move |this, cx| {
                    let result = pending
                        .await
                        .unwrap_or_else(|_| Err("the plugin start was lost".to_owned()));
                    this.update(cx, |view, cx| {
                        let done = PluginDone::Started {
                            id: started,
                            result,
                        };
                        let effect = view.ws.plugin_finished(done);
                        view.apply(effect, cx);
                        view.settle(cx);
                    })
                    .ok();
                })
                .detach();
                return;
            }
            other => {
                // Plugin drivers come and go with these effects: the connection form is told afterwards. The
                // connector asks the driver sources, which may read files, so this runs on the blocking pool.
                let connector = self.bridge.connector();
                let drivers_may_change = matches!(
                    other,
                    PluginEffect::Load { .. }
                        | PluginEffect::SetEnabled { .. }
                        | PluginEffect::Decide { .. }
                        | PluginEffect::Revoke(_)
                );
                self.bridge.blocking(move || {
                    let done = other.run_blocking(&*backend, &grants);
                    (done, drivers_may_change.then(|| connector.driver_ids()))
                })
            }
        };
        cx.spawn(async move |this, cx| {
            let Ok((done, drivers)) = done.await else {
                return;
            };
            this.update(cx, |view, cx| {
                if let Some(drivers) = drivers {
                    view.ws.set_drivers(drivers);
                }
                let effect = view.ws.plugin_finished(done);
                view.apply(effect, cx);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The answer to the permission dialog: a button, Escape or the typed id.
    pub(crate) fn plugin_answer(&mut self, command: GrantCommand, cx: &mut Context<Self>) {
        let effect = self.ws.plugin_answer(command);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Runs a command of the category `plugins` that `run_command` found by ID. `plugins:approve-grant` is never
    /// mapped: it is not a command anyone can run.
    pub(super) fn run_plugin_command(
        &mut self,
        id: &CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(command) = crate::plugins::action::action_for(id) {
            let effect = self.ws.dispatch_plugin_command(command);
            self.apply(effect, cx);
            self.settle(cx);
            return;
        }
        if crate::plugins::grant::action_for(id) == Some(GrantCommand::Deny) {
            if self.ws.plugins().dialog().is_some() {
                self.plugin_answer(GrantCommand::Deny, cx);
            } else {
                window.push_notification("No permission dialog is open.", cx);
            }
            return;
        }
        // `plugins:approve-grant` and anything else of the category that is not a command to run.
        window.push_notification("This plugin command does not apply now.", cx);
    }

    /// Selects a plugin row.
    pub(crate) fn plugin_select(&mut self, id: &PluginId, cx: &mut Context<Self>) {
        self.ws.plugin_select(id);
        cx.notify();
    }

    /// Flips a boolean setting of a plugin.
    pub(crate) fn plugin_toggle_setting(
        &mut self,
        id: &PluginId,
        key: &str,
        cx: &mut Context<Self>,
    ) {
        let effect = self.ws.plugin_toggle_setting(id, key);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// The editor of a text, integer or float setting of the selected plugin.
    pub(crate) fn plugin_setting_input(
        &self,
        id: &PluginId,
        key: &str,
    ) -> Option<&Entity<InputState>> {
        if self.plugins_ui.settings_for.as_ref() != Some(id) {
            return None;
        }
        self.plugins_ui.settings.get(key).map(|s| &s.input)
    }

    /// Makes the setting editors match the selected plugin and its stored values.
    pub(super) fn sync_plugin_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.ws.plugins().selected_row() else {
            self.plugins_ui.settings.clear();
            self.plugins_ui.settings_for = None;
            return;
        };
        let id = row.manifest.id().clone();
        let wanted: Vec<(String, String)> = row
            .settings
            .iter()
            .filter(|(_, v)| !matches!(v, SettingValue::Boolean(_)))
            .map(|(spec, v)| (spec.key.to_string(), value_text(v)))
            .collect();
        if self.plugins_ui.settings_for.as_ref() != Some(&id) {
            self.plugins_ui.settings.clear();
            self.plugins_ui.settings_for = Some(id.clone());
        }
        self.plugins_ui
            .settings
            .retain(|key, _| wanted.iter().any(|(k, _)| k == key));
        for (key, text) in wanted {
            if let Some(existing) = self.plugins_ui.settings.get_mut(&key) {
                // A value that changed elsewhere replaces the text, unless the user is typing in the field.
                let typing = existing.input.focus_handle(cx).is_focused(window);
                if existing.shown != text && !typing {
                    existing.shown.clone_from(&text);
                    existing
                        .input
                        .update(cx, |state, cx| state.set_value(text, window, cx));
                }
                continue;
            }
            let input = cx.new(|cx| InputState::new(window, cx).default_value(text.clone()));
            let (plugin, setting) = (id.clone(), key.clone());
            let sub = cx.subscribe(&input, move |view, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let typed = input.read(cx).value().to_string();
                    view.commit_plugin_setting(&plugin, &setting, &typed, cx);
                }
            });
            self.plugins_ui.settings.insert(
                key,
                SettingInput {
                    input,
                    shown: text,
                    _sub: sub,
                },
            );
        }
    }

    /// Keeps a typed setting when it differs from the stored value.
    fn commit_plugin_setting(
        &mut self,
        id: &PluginId,
        key: &str,
        typed: &str,
        cx: &mut Context<Self>,
    ) {
        let unchanged = self
            .plugins_ui
            .settings
            .get(key)
            .is_some_and(|s| s.shown == typed);
        if unchanged {
            return;
        }
        let effect = self.ws.plugin_set_setting(id, key, typed);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Prepares the dialog when one opens (an empty field, the focus on the dialog) and hands the focus back when it
    /// closes. A dialog that was rebuilt (after a stale decision) counts as a new one.
    pub(super) fn sync_plugin_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.ws.plugins().dialog().map_or(0, |d| d.generation);
        if generation == self.plugins_ui.shown {
            return;
        }
        let was_open = self.plugins_ui.shown != 0;
        self.plugins_ui.shown = generation;
        if generation != 0 {
            self.plugins_ui
                .confirm
                .update(cx, |state, cx| state.set_value("", window, cx));
            window.focus(&self.plugins_ui.focus, cx);
        } else if was_open && self.plugins_ui.focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
    }

    /// The permission dialog, while one is open.
    pub(super) fn plugin_dialog_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.ws.plugins().dialog()?;
        let weak = cx.weak_entity();
        let on: GrantHandler = Rc::new(move |command, _, cx| {
            weak.update(cx, |view, cx| view.plugin_answer(command, cx))
                .ok();
        });
        Some(grant_overlay(
            &dialog.prompt,
            on,
            &self.plugins_ui.focus,
            &self.plugins_ui.confirm,
            cx,
        ))
    }

    /// Whether the permission dialog is on screen.
    pub(super) fn plugin_dialog_open(&self) -> bool {
        self.ws.plugins().dialog().is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use gpui::{Entity, TestAppContext, VisualTestContext};
    use sqler_ai_api::{AiError, AiProvider};
    use sqler_config::{
        AiConfig, ConnectionStore, KeybindingOverrides, MemoryStore, ProviderConfig, ProviderKind,
        Secret,
    };

    use super::super::{Startup, WorkspaceView};
    use crate::ai::{AiProviderFactory, AiStartup};
    use crate::plugins::PluginsStartup;
    use crate::plugins::backend::{FakeBackend, PluginSnapshot};
    use crate::plugins::grant::{GrantBook, GrantStatus};
    use crate::plugins::manager::fixture::{manifest, row};
    use crate::settings::SettingsModel;
    use crate::theme::Appearance;
    use crate::theme::apply::apply;
    use crate::workspace::testing::FakeConnector;

    struct NoProviders;

    impl AiProviderFactory for NoProviders {
        fn kinds(&self) -> Vec<ProviderKind> {
            Vec::new()
        }

        fn build(
            &self,
            _: &ProviderConfig,
            _: Option<&Secret>,
        ) -> Result<Arc<dyn AiProvider>, AiError> {
            Err(AiError::Unsupported("test".to_owned()))
        }
    }

    struct Rig {
        view: Entity<WorkspaceView>,
        fake: Arc<FakeBackend>,
        grants: Arc<GrantBook>,
        // Keeps the runtime the background work runs on alive.
        _runtime: tokio::runtime::Runtime,
    }

    fn open(cx: &mut TestAppContext) -> (Rig, &mut VisualTestContext) {
        cx.update(crate::workspace::init);
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let handle = runtime.handle().clone();
        let fake = FakeBackend::new(PluginSnapshot {
            rows: vec![row(manifest("acme-sync", &["db.read", "db.write"]))],
            problems: Vec::new(),
        });
        let grants = Arc::new(GrantBook::in_memory());
        let plugins = PluginsStartup::new(Some(fake.clone()), Arc::clone(&grants));
        let startup = Startup {
            connections: Ok(ConnectionStore::new()),
            config_dir: None,
            connector: Arc::new(FakeConnector::new()),
            secrets: Arc::new(MemoryStore::new()),
            runtime: handle,
            exit_after_first_frame: false,
            settings: SettingsModel::default(),
            keybindings: Ok(KeybindingOverrides::default()),
            history_dir: None,
            workbench: crate::workbench::WorkbenchStartup::default(),
            ai: AiStartup {
                config: Ok(AiConfig::default()),
                dir: None,
                factory: Arc::new(NoProviders),
            },
            plugins,
            export_sources: Vec::new(),
        };
        let (view, cx) =
            cx.add_window_view(move |window, cx| WorkspaceView::new(startup, window, cx));
        cx.update(|_, cx| apply(Appearance::Dark, 14, cx));
        (
            Rig {
                view,
                fake,
                grants,
                _runtime: runtime,
            },
            cx,
        )
    }

    /// Draws and lets the background work come back until `done` holds. A hang guard, not a time budget.
    fn settle_until(
        rig: &Rig,
        cx: &mut VisualTestContext,
        what: &str,
        done: impl Fn(&WorkspaceView) -> bool,
    ) {
        for _ in 0..2000 {
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            let ready = rig.view.read_with(cx, |view, _| done(view));
            if ready {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    fn start(rig: &Rig, cx: &mut VisualTestContext) {
        // A frame callback runs before the frame it was asked for: the start is the second one.
        assert_eq!(cx.update(gpui::Window::simulate_next_frame), 1);
        assert_eq!(
            rig.fake.calls.refresh.load(Ordering::SeqCst),
            0,
            "nothing before the first frame was drawn"
        );
        assert_eq!(cx.update(gpui::Window::simulate_next_frame), 1);
        settle_until(rig, cx, "the permission dialog", |v| {
            v.ws.plugins().dialog().is_some()
        });
    }

    #[gpui::test]
    fn nothing_runs_before_the_first_frame_and_the_dialog_takes_the_focus(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        cx.run_until_parked();
        assert_eq!(rig.fake.calls.refresh.load(Ordering::SeqCst), 0);
        assert_eq!(rig.fake.calls.snapshot.load(Ordering::SeqCst), 0);
        start(&rig, cx);
        assert_eq!(rig.fake.calls.refresh.load(Ordering::SeqCst), 1);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let focused = rig.view.read_with(cx, |v, _| v.plugins_ui.focus.clone());
        assert!(cx.update(|window, _| focused.is_focused(window)));
    }

    #[gpui::test]
    fn escape_denies_and_the_host_is_told(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        start(&rig, cx);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        settle_until(&rig, cx, "the denial", |v| {
            v.ws.plugins().dialog().is_none()
        });
        let row = rig
            .view
            .read_with(cx, |v, _| v.ws.plugins().rows()[0].clone());
        let key = row.manifest.grant_key(None).unwrap();
        assert_eq!(rig.grants.status(&key), GrantStatus::Denied);
        settle_until(&rig, cx, "the host call", |_| {
            rig.fake.calls.grants_changed.load(Ordering::SeqCst) == 1
        });
        assert!(
            rig.fake.started.lock().unwrap().is_empty(),
            "a denied plugin is not started"
        );
    }

    #[gpui::test]
    fn escape_denies_from_the_typed_id_field(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        start(&rig, cx);
        let confirm = rig.view.read_with(cx, |v, _| v.plugins_ui.confirm.clone());
        cx.update(|window, cx| {
            window.focus(&gpui::Focusable::focus_handle(&confirm, cx), cx);
            confirm.update(cx, |input, cx| input.set_value("acme-sync", window, cx));
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        settle_until(&rig, cx, "the denial", |v| {
            v.ws.plugins().dialog().is_none()
        });
        let row = rig
            .view
            .read_with(cx, |v, _| v.ws.plugins().rows()[0].clone());
        let key = row.manifest.grant_key(None).unwrap();
        assert_eq!(rig.grants.status(&key), GrantStatus::Denied);
    }

    #[gpui::test]
    fn the_typed_id_and_enter_approve_and_the_plugin_starts(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        start(&rig, cx);
        let confirm = rig.view.read_with(cx, |v, _| v.plugins_ui.confirm.clone());
        cx.update(|window, cx| {
            window.focus(&gpui::Focusable::focus_handle(&confirm, cx), cx);
            confirm.update(cx, |input, cx| input.set_value("acme-syn", window, cx));
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            rig.view
                .read_with(cx, |v, _| v.ws.plugins().dialog().is_some()),
            "a wrong id does nothing"
        );
        cx.update(|window, cx| {
            confirm.update(cx, |input, cx| input.set_value("acme-sync", window, cx));
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        settle_until(&rig, cx, "the plugin to start", |_| {
            !rig.fake.started.lock().unwrap().is_empty()
        });
        let row = rig
            .view
            .read_with(cx, |v, _| v.ws.plugins().rows()[0].clone());
        let key = row.manifest.grant_key(None).unwrap();
        assert!(matches!(rig.grants.status(&key), GrantStatus::Approved(_)));
        assert!(
            rig.view
                .read_with(cx, |v, _| v.ws.plugins().dialog().is_none())
        );
    }
}
