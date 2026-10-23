// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of what plugins contribute (ADR-0060): the palette, menu and panel commands, the status bar texts and
//! the Plugins tab of the bottom panel.
//!
//! The state is headless (`PluginUiState`); this file registers the commands, runs the calls against the host on tokio
//! (`SessionBridge::spawn`, never on the GPUI thread), hands the results back, owns the editors of the text inputs of
//! the panel on screen and draws the panel.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement as _, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Icon, Selectable as _, Sizable as _, WindowExt as _};
use sqler_config::Platform;
use sqler_plugin_api::{CommandContext, ContributionId, PluginId, WidgetEvent, WidgetTree};
use tokio::sync::mpsc::UnboundedReceiver;

use super::WorkspaceView;
use crate::command::CommandId;
use crate::keymap::KeyDispatcher;
use crate::plugins::PluginUiEvent;
use crate::plugins::contrib::{Action, PluginUiDone, PluginUiEffect, Via, is_plugin_category};
use crate::plugins::menu::{editor_context, object_context, results_context};
use crate::plugins::panel::SlotView;
use crate::plugins::widget::diff;
use crate::plugins::widget::view::{Emit, render_tree};
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::empty_state;
use crate::workbench::view::Hints;
use crate::workspace::chrome::ui_scale;

/// The text editor of one text input of the panel on screen.
struct PanelInput {
    input: Entity<InputState>,
    /// The text the field was last set to, or last sent to the plugin.
    shown: String,
    _sub: Subscription,
}

/// What the plugin panel owns on the `GPUI` side: the editors of its text inputs.
#[derive(Default)]
pub(crate) struct PluginUiViews {
    /// The panel the editors belong to, and the tree they were last matched with.
    synced: Option<(PluginId, ContributionId, Arc<WidgetTree>)>,
    inputs: HashMap<String, PanelInput>,
}

/// The command a panel body runs to show (and so reload) its panel.
fn failure_title(title: &str) -> String {
    format!("{title} could not be shown")
}

impl WorkspaceView {
    /// How many plugin panels are registered (the bottom panel shows its tab when there is one).
    pub(crate) fn plugin_panel_count(&self) -> usize {
        self.ws.plugin_ui().panels().len()
    }

    /// Moves the events plugins push into the workspace, one by one, for as long as the window lives.
    pub(super) fn drain_plugin_ui(
        mut inbox: UnboundedReceiver<PluginUiEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(event) = inbox.recv().await {
                let alive = this.update(cx, |view, cx| {
                    let effect = view.ws.plugin_ui_event(event, Instant::now());
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

    /// After a manager load: drops the plugin commands, registers the current ones and rebuilds the key dispatcher
    /// (a keybinding for a plugin command binds once the command exists) and the hints. Also moves the messages of
    /// the contributions to the notices and starts the work the panel on screen needs.
    pub(super) fn sync_plugin_contributions(&mut self, cx: &mut Context<Self>) {
        if self.ws.plugin_ui_stale() {
            self.commands
                .retain(|command| !is_plugin_category(command.category()));
            let commands = &self.commands;
            let effect = self
                .ws
                .sync_plugin_ui(&|id| commands.get(id).is_some(), Instant::now());
            if let Err(err) = self
                .ws
                .plugin_ui()
                .registrations()
                .register(&mut self.commands)
            {
                eprintln!("sqler: could not register the plugin commands: {err}");
            }
            // The notices of the rebuilt dispatcher were shown at startup.
            self.keys = KeyDispatcher::new(
                Platform::current(),
                self.key_overrides.clone(),
                &self.commands,
            );
            self.hints = Hints::build(|id| self.keys.shortcut_display(id));
            self.apply(effect, cx);
        }
        let effect = self.ws.plugin_ui_visibility(Instant::now());
        self.apply(effect, cx);
        let notices = self.ws.take_plugin_ui_notices();
        self.notices.extend(notices);
    }

    /// Runs the command a plugin contributed. `false` when it is not registered (any more).
    pub(super) fn run_plugin_contribution(
        &mut self,
        id: &CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(reg) = self.ws.plugin_ui().registrations().find(id).cloned() else {
            return false;
        };
        let effect = match reg.action {
            Action::ShowPanel(panel) => self.ws.plugin_ui_show(&reg.plugin, &panel, Instant::now()),
            Action::Run { command, via } => match self.plugin_context(&via, cx) {
                Ok(context) => self.ws.plugin_ui_run(&reg.plugin, &command, context),
                Err(notice) => {
                    window.push_notification(notice, cx);
                    return true;
                }
            },
        };
        self.apply(effect, cx);
        self.settle(cx);
        true
    }

    /// The context of a command, read from what is selected. `Err` is the notice for the user.
    fn plugin_context(&self, via: &Via, cx: &App) -> Result<CommandContext, String> {
        match via {
            Via::Palette => Ok(CommandContext::None),
            Via::Object(kinds) => {
                let conn = self.ws.sidebar().selected_id();
                let object = conn.and_then(|id| {
                    let tree = self.ws.browser(id)?;
                    tree.object(tree.selected().unwrap_or_else(|| tree.root()))
                });
                let conn = conn.map(ToString::to_string);
                object_context(conn.as_deref(), object, kinds)
            }
            Via::Editor => {
                let tab = self.ws_active_editor();
                let connection = tab
                    .and_then(|t| self.ws.tab_conns.get(&t))
                    .map(ToString::to_string);
                let buffer = tab.and_then(|t| self.editors.buffer(t, cx));
                let selection = buffer.as_ref().and_then(|b| b.selected_text());
                editor_context(tab.map(|_| (connection.as_deref(), selection)))
            }
            Via::Results => {
                let tab = self.ws_active_grid().or_else(|| self.ws_active_query());
                let connection = tab
                    .and_then(|t| self.ws.tab_conns.get(&t))
                    .map(ToString::to_string);
                let table = self.active_table();
                let columns = table
                    .as_ref()
                    .map(|t| t.read(cx).delegate().grid().model().columns().to_vec());
                results_context(connection.as_deref(), columns.as_deref())
            }
        }
    }

    /// Carries out a call against a plugin on tokio; the result goes back to the workspace.
    pub(super) fn apply_plugin_ui(&mut self, effect: PluginUiEffect, cx: &mut Context<Self>) {
        let Some(backend) = self.ws.plugins().backend().cloned() else {
            return;
        };
        match effect {
            PluginUiEffect::RunCommand {
                plugin,
                command,
                context,
            } => {
                let pending = {
                    let (plugin, command) = (plugin.clone(), command.clone());
                    self.bridge
                        .spawn(async move { backend.run_command(&plugin, &command, context).await })
                };
                Self::await_plugin_call(pending, cx, move |result| PluginUiDone::Command {
                    plugin,
                    command,
                    result,
                });
            }
            PluginUiEffect::Render { plugin, panel } => {
                let pending = {
                    let (plugin, panel) = (plugin.clone(), panel.clone());
                    self.bridge
                        .spawn(async move { backend.render_panel(&plugin, &panel).await })
                };
                Self::await_plugin_call(pending, cx, move |result| PluginUiDone::Rendered {
                    plugin,
                    panel,
                    result,
                });
            }
            PluginUiEffect::RenderAfter {
                plugin,
                panel,
                delay,
            } => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    this.update(cx, |view, cx| {
                        view.apply_plugin_ui(PluginUiEffect::Render { plugin, panel }, cx);
                    })
                    .ok();
                })
                .detach();
            }
            PluginUiEffect::Event {
                plugin,
                panel,
                event,
            } => {
                let pending = {
                    let (plugin, panel) = (plugin.clone(), panel.clone());
                    self.bridge
                        .spawn(async move { backend.panel_event(&plugin, &panel, event).await })
                };
                Self::await_plugin_call(pending, cx, move |result| PluginUiDone::EventReply {
                    plugin,
                    panel,
                    result,
                });
            }
            PluginUiEffect::Status { plugin, item } => {
                let pending = {
                    let (plugin, item) = (plugin.clone(), item.clone());
                    self.bridge
                        .spawn(async move { backend.status(&plugin, &item).await })
                };
                Self::await_plugin_call(pending, cx, move |result| PluginUiDone::Status {
                    plugin,
                    item,
                    result,
                });
            }
        }
    }

    /// Waits for a call on the GPUI executor and reports it to the workspace.
    fn await_plugin_call<T: 'static>(
        pending: tokio::sync::oneshot::Receiver<Result<T, String>>,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<T, String>) -> PluginUiDone + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = pending
                .await
                .unwrap_or_else(|_| Err("the plugin call was lost".to_owned()));
            this.update(cx, |view, cx| {
                let effect = view.ws.plugin_ui_finished(done(result), Instant::now());
                view.apply(effect, cx);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// A widget of the panel on screen was used.
    pub(crate) fn plugin_widget_event(&mut self, event: WidgetEvent, cx: &mut Context<Self>) {
        let effect = self.ws.plugin_ui_widget_event(event, Instant::now());
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// A chip of the bottom panel: show another plugin panel.
    pub(crate) fn plugin_select_panel(
        &mut self,
        plugin: &PluginId,
        panel: &ContributionId,
        cx: &mut Context<Self>,
    ) {
        let effect = self.ws.plugin_ui_select(plugin, panel, Instant::now());
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Makes the text editors match the tree of the panel on screen: new inputs get an editor, gone ones lose it, and a
    /// value the plugin changed replaces the text unless the user is typing in the field.
    pub(super) fn sync_plugin_panel_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.ws.layout().panel_visible()
            && self.ws.layout().panel_view() == crate::workbench::PanelView::PluginPanels;
        let current = if shown {
            self.ws
                .plugin_ui()
                .selected()
                .zip(
                    self.ws
                        .plugin_ui()
                        .selected_slot()
                        .and_then(|s| s.tree_arc()),
                )
                .map(|(reg, tree)| (reg.plugin.clone(), reg.panel.clone(), Arc::clone(tree)))
        } else {
            None
        };
        let Some((plugin, panel, tree)) = current else {
            return;
        };
        let old = match &self.plugin_ui_views.synced {
            // The very tree already matched: nothing to compare, nothing to copy.
            Some((p, q, t)) if *p == plugin && *q == panel && Arc::ptr_eq(t, &tree) => return,
            Some((p, q, t)) if *p == plugin && *q == panel => Some(Arc::clone(t)),
            _ => {
                self.plugin_ui_views.inputs.clear();
                None
            }
        };
        let changes = diff(old.as_deref(), &tree);
        if changes.same {
            return;
        }
        for id in &changes.inputs_removed {
            self.plugin_ui_views.inputs.remove(id);
        }
        for node in tree.nodes() {
            let sqler_plugin_api::Widget::TextInput(spec) = &node.widget else {
                continue;
            };
            if changes.inputs_added.contains(&spec.id) {
                let input = cx.new(|cx| {
                    let state = InputState::new(window, cx).default_value(spec.value.clone());
                    match &spec.placeholder {
                        Some(placeholder) => state.placeholder(placeholder.clone()),
                        None => state,
                    }
                });
                let id = spec.id.clone();
                let sub = cx.subscribe(&input, move |view, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let typed = input.read(cx).value().to_string();
                        view.commit_panel_input(&id, typed, cx);
                    }
                });
                self.plugin_ui_views.inputs.insert(
                    spec.id.clone(),
                    PanelInput {
                        input,
                        shown: spec.value.clone(),
                        _sub: sub,
                    },
                );
            } else if changes.inputs_value_changed.contains(&spec.id)
                && let Some(existing) = self.plugin_ui_views.inputs.get_mut(&spec.id)
            {
                let typing = existing.input.focus_handle(cx).is_focused(window);
                if !typing {
                    existing.shown.clone_from(&spec.value);
                    let value = spec.value.clone();
                    existing
                        .input
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                }
            }
        }
        self.plugin_ui_views.synced = Some((plugin, panel, tree));
    }

    /// Sends the text a user typed when it differs from what the plugin has.
    fn commit_panel_input(&mut self, id: &str, typed: String, cx: &mut Context<Self>) {
        let Some(entry) = self.plugin_ui_views.inputs.get_mut(id) else {
            return;
        };
        if entry.shown == typed {
            return;
        }
        entry.shown.clone_from(&typed);
        let event = WidgetEvent::Changed {
            id: id.to_owned(),
            value: typed,
        };
        self.plugin_widget_event(event, cx);
    }

    /// The Plugins tab of the bottom panel: the panel chips, and the tree, the loading state or the error of the
    /// panel selected.
    #[allow(clippy::too_many_lines)] // one declarative element tree
    pub(super) fn plugin_panel_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.ws.plugin_ui();
        let Some(selected) = state.selected().cloned() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(empty_state(
                    AppIcon::Plug,
                    "No plugin panels",
                    "Plugins that add a panel appear here once they are approved and running.",
                    cx,
                ))
                .into_any_element();
        };
        let mut chips = div()
            .flex_none()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(SPACE[1]))
            .px(px(SPACE[3]))
            .py(px(SPACE[1]));
        let panels = state.panels();
        let show_chips = panels.len() > 1;
        for (i, reg) in panels.iter().enumerate() {
            let (plugin, panel) = (reg.plugin.clone(), reg.panel.clone());
            let chosen = reg.plugin == selected.plugin && reg.panel == selected.panel;
            chips = chips.child(
                Button::new(("plugin-panel-chip", i))
                    .label(format!("{} - {}", reg.plugin_name, reg.title))
                    .ghost()
                    .small()
                    .selected(chosen)
                    .tab_stop(true)
                    .accessibility_label(format!("Show {} ({})", reg.title, reg.plugin_name))
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        view.plugin_select_panel(&plugin, &panel, cx);
                    })),
            );
        }
        let view = state
            .selected_slot()
            .map_or(SlotView::Loading, |s| s.view().clone());
        let content = match view {
            SlotView::Loading => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(SPACE[2]))
                .text_size(px(ui_scale(cx).sm))
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(AppIcon::LoaderCircle).size(px(SPACE[5])))
                .child("Loading the panel…")
                .into_any_element(),
            SlotView::Failed(reason) => {
                let show = selected.show.clone();
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        empty_state(
                            AppIcon::CircleAlert,
                            failure_title(&selected.title),
                            reason,
                            cx,
                        )
                        .child(
                            Button::new("plugin-panel-reload")
                                .label("Reload")
                                .small()
                                .outline()
                                .tab_stop(true)
                                .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                                    view.run_command(&show, window, cx);
                                })),
                        ),
                    )
                    .into_any_element()
            }
            SlotView::Tree(tree) => {
                let weak = cx.weak_entity();
                let emit: Emit =
                    Rc::new(move |event: WidgetEvent, _: &mut Window, cx: &mut App| {
                        weak.update(cx, |view, cx| view.plugin_widget_event(event, cx))
                            .ok();
                    });
                let inputs: HashMap<String, Entity<InputState>> = self
                    .plugin_ui_views
                    .inputs
                    .iter()
                    .map(|(id, entry)| (id.clone(), entry.input.clone()))
                    .collect();
                div()
                    .id("plugin-panel-tree")
                    .size_full()
                    .overflow_y_scroll()
                    .p(px(SPACE[4]))
                    .child(render_tree(&tree, &inputs, &emit, cx))
                    .into_any_element()
            }
        };
        div()
            .id("plugin-panel")
            .size_full()
            .flex()
            .flex_col()
            .child(if show_chips {
                chips.into_any_element()
            } else {
                // One panel: no chips, but its content is still named after the plugin it comes from.
                div()
                    .id("plugin-panel-header")
                    .flex_none()
                    .px(px(SPACE[3]))
                    .py(px(SPACE[1]))
                    .text_size(px(ui_scale(cx).sm))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} - {}", selected.title, selected.plugin_name))
                    .into_any_element()
            })
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
    use sqler_ai_api::{AiError, AiProvider};
    use sqler_config::{
        AiConfig, ConnectionStore, KeybindingOverrides, MemoryStore, ProviderConfig, ProviderKind,
        Secret,
    };
    use sqler_plugin_api::{Alignment, ContributionId, PluginId};

    use super::super::{Startup, WorkspaceView};
    use crate::ai::{AiProviderFactory, AiStartup};
    use crate::command::CommandId;
    use crate::plugins::backend::{FakeBackend, PluginSnapshot, PluginUiEvent, ui_channel};
    use crate::plugins::grant::{GrantBook, GrantCommand};
    use crate::plugins::manager::fixture::{contributing, panel_tree, row};
    use crate::plugins::panel::SlotView;
    use crate::plugins::{PluginRow, PluginsStartup, RowState};
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
        events: crate::plugins::UiSender,
        // Keeps the runtime the background work runs on alive.
        _runtime: tokio::runtime::Runtime,
    }

    fn pid(s: &str) -> PluginId {
        s.parse().unwrap()
    }

    fn cid(s: &str) -> ContributionId {
        s.parse().unwrap()
    }

    /// The approved, running plugin that contributes one of everything.
    fn live_row(book: &GrantBook) -> PluginRow {
        let mut r = row(contributing("acme-sync", "Acme Sync"));
        r.state = RowState::Live;
        let prompt = book
            .prompt(&r.manifest, r.component.as_ref())
            .expect("prompt");
        book.decide(&prompt, GrantCommand::Approve).expect("decide");
        r.effective = r.manifest.capabilities().clone();
        r
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
        let book = Arc::new(GrantBook::in_memory());
        let fake = FakeBackend::new(PluginSnapshot {
            rows: vec![live_row(&book)],
            problems: Vec::new(),
        });
        fake.script_panel("acme-sync", "overview", Ok(panel_tree()));
        fake.script_status("acme-sync", "env", Some("prod".to_owned()));
        let (events, inbox) = ui_channel();
        let plugins = PluginsStartup::new(Some(fake.clone()), book).with_ui_events(inbox);
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
        // `Root` hosts the notifications a failing command or a pushed message shows.
        let shared = std::rc::Rc::new(std::cell::RefCell::new(None));
        let hold = std::rc::Rc::clone(&shared);
        let (_root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| WorkspaceView::new(startup, window, cx));
            *hold.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view = shared.borrow_mut().take().unwrap();
        cx.update(|_, cx| apply(Appearance::Dark, 14, cx));
        (
            Rig {
                view,
                fake,
                events,
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
            if rig.view.read_with(cx, |view, _| done(view)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    /// Starts the manager as the first frame would and waits for the contributions to be registered.
    fn start(rig: &Rig, cx: &mut VisualTestContext) {
        assert_eq!(cx.update(gpui::Window::simulate_next_frame), 1);
        assert_eq!(cx.update(gpui::Window::simulate_next_frame), 1);
        settle_until(rig, cx, "the contributions", |v| {
            v.commands
                .get(&CommandId::new("plugin-acme-sync:sync").unwrap())
                .is_some()
        });
    }

    fn run(rig: &Rig, cx: &mut VisualTestContext, id: &str) {
        let id = CommandId::new(id).unwrap();
        cx.update(|window, cx| {
            rig.view
                .update(cx, |view, cx| view.run_command(&id, window, cx));
        });
    }

    #[gpui::test]
    fn a_palette_command_runs_the_plugin_without_a_context(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        start(&rig, cx);
        run(&rig, cx, "plugin-acme-sync:sync");
        settle_until(&rig, cx, "the call", |_| {
            !rig.fake.commands.lock().unwrap().is_empty()
        });
        let calls = rig.fake.commands.lock().unwrap().clone();
        assert_eq!(
            calls,
            [(
                "acme-sync".to_owned(),
                "sync".to_owned(),
                sqler_plugin_api::CommandContext::None
            )]
        );
    }

    #[gpui::test]
    fn a_failing_command_keeps_the_view_alive_and_lists_the_plugins_again(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        start(&rig, cx);
        rig.fake.fail_commands(Some("it broke".to_owned()));
        let before = rig.fake.calls.snapshot.load(Ordering::SeqCst);
        run(&rig, cx, "plugin-acme-sync:sync");
        settle_until(&rig, cx, "the reload", |_| {
            rig.fake.calls.snapshot.load(Ordering::SeqCst) > before
        });
        // The view still answers.
        assert!(
            rig.view
                .read_with(cx, |v, _| v.ws.plugin_ui().panels().len() == 1)
        );
    }

    #[gpui::test]
    fn the_status_text_arrives_after_the_plugin_is_live_and_events_replace_it(
        cx: &mut TestAppContext,
    ) {
        let (rig, cx) = open(cx);
        start(&rig, cx);
        let text = |v: &WorkspaceView| {
            v.ws.plugin_ui()
                .status()
                .shown(Alignment::Left)
                .first()
                .map(|s| s.text.clone())
        };
        settle_until(&rig, cx, "the status text", |v| text(v).is_some());
        assert_eq!(
            rig.view.read_with(cx, |v, _| text(v)).as_deref(),
            Some("prod")
        );
        rig.events
            .send(PluginUiEvent::SetStatus {
                plugin: pid("acme-sync"),
                item: cid("env"),
                text: "staging".to_owned(),
            })
            .unwrap();
        settle_until(&rig, cx, "the pushed text", |v| {
            text(v).as_deref() == Some("staging")
        });
    }

    #[gpui::test]
    fn a_click_round_trips_and_the_replaced_tree_renders(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        start(&rig, cx);
        run(&rig, cx, "panel-plugin-acme-sync:overview");
        let tree = |v: &WorkspaceView| {
            v.ws.plugin_ui()
                .selected_slot()
                .and_then(|s| s.tree().cloned())
        };
        settle_until(&rig, cx, "the panel", |v| tree(v).is_some());
        assert_eq!(
            rig.view.read_with(cx, |v, _| tree(v)).unwrap(),
            panel_tree()
        );
        // The plugin answers the click with a smaller tree.
        let reply = sqler_plugin_api::WidgetTree::new(vec![sqler_plugin_api::WidgetNode {
            widget: sqler_plugin_api::Widget::Text(sqler_plugin_api::Text {
                content: "Synced.".to_owned(),
                style: sqler_plugin_api::TextStyle::Body,
            }),
            children: Vec::new(),
        }])
        .unwrap();
        rig.fake
            .script_event_reply("acme-sync", "overview", Ok(Some(reply.clone())));
        rig.view.update(cx, |view, cx| {
            view.plugin_widget_event(
                sqler_plugin_api::WidgetEvent::Clicked("sync".to_owned()),
                cx,
            );
        });
        settle_until(&rig, cx, "the replaced tree", |v| {
            tree(v).is_some_and(|t| t == reply)
        });
        let events = rig.fake.events.lock().unwrap().clone();
        assert_eq!(
            events,
            [(
                "acme-sync".to_owned(),
                "overview".to_owned(),
                sqler_plugin_api::WidgetEvent::Clicked("sync".to_owned())
            )]
        );
    }

    #[gpui::test]
    fn a_bad_tree_shows_the_error_state_and_reload_draws_again(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        rig.fake.script_panel(
            "acme-sync",
            "overview",
            Err("The plugin sent a tree that cannot be shown.".to_owned()),
        );
        start(&rig, cx);
        run(&rig, cx, "panel-plugin-acme-sync:overview");
        settle_until(&rig, cx, "the error state", |v| {
            matches!(
                v.ws.plugin_ui().selected_slot().map(|s| s.view().clone()),
                Some(SlotView::Failed(_))
            )
        });
        // The plugin is fixed; "Reload" runs the show command again.
        rig.fake
            .script_panel("acme-sync", "overview", Ok(panel_tree()));
        std::thread::sleep(Duration::from_millis(120));
        run(&rig, cx, "panel-plugin-acme-sync:overview");
        settle_until(&rig, cx, "the tree", |v| {
            v.ws.plugin_ui()
                .selected_slot()
                .is_some_and(|s| s.tree().is_some())
        });
    }

    #[gpui::test]
    fn switching_the_plugin_off_removes_its_command_status_and_panel(cx: &mut TestAppContext) {
        let (rig, cx) = open(cx);
        start(&rig, cx);
        settle_until(&rig, cx, "the status text", |v| {
            !v.ws.plugin_ui().status().shown(Alignment::Left).is_empty()
        });
        let book = Arc::new(GrantBook::in_memory());
        let mut off = live_row(&book);
        off.enabled = false;
        off.state = RowState::Off;
        rig.fake.script(PluginSnapshot {
            rows: vec![off],
            problems: Vec::new(),
        });
        cx.update(|_, cx| {
            rig.view.update(cx, |view, cx| {
                let effect = view.ws.plugins_activated();
                view.apply(effect, cx);
                view.settle(cx);
            });
        });
        settle_until(&rig, cx, "the removal", |v| {
            v.commands
                .get(&CommandId::new("plugin-acme-sync:sync").unwrap())
                .is_none()
        });
        rig.view.read_with(cx, |v, _| {
            assert_eq!(v.ws.plugin_ui().panels().len(), 0);
            assert_eq!(v.ws.plugin_ui().status().shown(Alignment::Left).len(), 0);
            assert!(
                v.commands
                    .iter()
                    .all(|c| !crate::plugins::contrib::is_plugin_category(c.category()))
            );
            assert_eq!(v.plugin_panel_count(), 0);
        });
        // A key still bound to the command does nothing and does not panic.
        run(&rig, cx, "plugin-acme-sync:sync");
        cx.run_until_parked();
        assert!(rig.fake.commands.lock().unwrap().is_empty());
    }
}
