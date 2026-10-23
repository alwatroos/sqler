// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plugin manager inside the workspace (ADR-0060).
//!
//! State only, like the audit viewer: each call changes [`PluginsState`] and returns the work to carry out as
//! [`Effect::Plugins`]; the view runs it on the background executor and reports with
//! [`Workspace::plugin_finished`]. Nothing here runs before the first frame.

use std::time::Instant;

use sqler_plugin_api::{CommandContext, ContributionId, PluginId, WidgetEvent};

use super::state::{Effect, Workspace};
use crate::command::CommandId;
use crate::plugins::contrib::{
    PluginUiDone, PluginUiEffect, PluginUiState, Reaction, Registrations, SyncInput,
};
use crate::plugins::grant::GrantCommand;
use crate::plugins::manager::{PluginDone, PluginEffect, PluginsState, RowGrant};
use crate::plugins::{PluginCommand, PluginRow, PluginUiEvent, PluginsStartup, RowState};
use crate::workbench::PanelView;

impl Workspace {
    /// Builds the plugin manager from what the binary handed over. Nothing runs yet.
    #[must_use]
    pub fn with_plugins(mut self, startup: &PluginsStartup) -> Self {
        self.plugins = PluginsState::new(startup);
        self
    }

    /// The plugin manager.
    #[must_use]
    pub fn plugins(&self) -> &PluginsState {
        &self.plugins
    }

    /// The manager's effects as one effect; its message for the user goes to the workspace notice.
    fn plugin_effects(&mut self, effects: Vec<PluginEffect>) -> Effect {
        if let Some(notice) = self.plugins.take_notice() {
            self.notice = Some(notice);
        }
        let mut effects: Vec<Effect> = effects.into_iter().map(Effect::Plugins).collect();
        match effects.len() {
            0 => Effect::None,
            1 => effects.remove(0),
            _ => Effect::Batch(effects),
        }
    }

    /// The first frame was drawn: start reading the grants and discovering the plugins.
    pub fn plugins_first_frame(&mut self) -> Effect {
        let effects = self.plugins.first_frame();
        self.plugin_effects(effects)
    }

    /// The window became active: re-read the grants.
    pub fn plugins_activated(&mut self) -> Effect {
        let effects = self.plugins.activated();
        self.plugin_effects(effects)
    }

    /// Runs a manager command.
    pub fn dispatch_plugin_command(&mut self, command: PluginCommand) -> Effect {
        let effects = self.plugins.command(command);
        self.plugin_effects(effects)
    }

    /// The answer to the permission dialog.
    pub fn plugin_answer(&mut self, command: GrantCommand) -> Effect {
        let effects = self.plugins.answer(command);
        self.plugin_effects(effects)
    }

    /// Selects a plugin row.
    pub fn plugin_select(&mut self, id: &PluginId) {
        self.plugins.select(id);
    }

    /// Stores a setting the user typed.
    pub fn plugin_set_setting(&mut self, id: &PluginId, key: &str, text: &str) -> Effect {
        let effects = self.plugins.set_setting(id, key, text);
        self.plugin_effects(effects)
    }

    /// Flips a boolean setting.
    pub fn plugin_toggle_setting(&mut self, id: &PluginId, key: &str) -> Effect {
        let effects = self.plugins.toggle_setting(id, key);
        self.plugin_effects(effects)
    }

    /// Takes the result of an [`Effect::Plugins`] and returns the follow-up work.
    pub fn plugin_finished(&mut self, done: PluginDone) -> Effect {
        let effects = self.plugins.finished(done);
        self.plugin_effects(effects)
    }

    // ---- what the plugins contribute (ADR-0060) ----

    /// What the plugins contribute: commands, panels and status items.
    #[must_use]
    pub fn plugin_ui(&self) -> &PluginUiState {
        &self.plugin_ui
    }

    /// Whether a manager load came back since the contributions were built.
    #[must_use]
    pub fn plugin_ui_stale(&self) -> bool {
        self.plugins.generation() != self.plugin_ui.generation()
    }

    /// Builds the contributions again from the rows of the manager. `taken` says whether the application already
    /// uses a command id (the caller has dropped the plugin commands from its registry first). Returns the work to
    /// start (status texts, a panel that is on screen).
    pub fn sync_plugin_ui(&mut self, taken: &dyn Fn(&CommandId) -> bool, now: Instant) -> Effect {
        let rows = self.plugins.rows();
        let approved = |row: &PluginRow| self.plugins.grant_of(row) == RowGrant::Approved;
        let (regs, refusals) = Registrations::from_approved(rows, taken, &approved);
        let input = SyncInput {
            generation: self.plugins.generation(),
            regs,
            refusals,
            live: rows
                .iter()
                .filter(|r| r.state == RowState::Live)
                .map(|r| r.manifest.id().clone())
                .collect(),
            names: rows
                .iter()
                .map(|r| (r.manifest.id().clone(), r.manifest.name().to_owned()))
                .collect(),
        };
        let reaction = self.plugin_ui.sync(input);
        let mut effects = vec![self.plugin_ui_reaction(reaction)];
        effects.push(self.plugin_ui_visibility(now));
        batch(effects)
    }

    /// The bottom panel shows (or no longer shows) the plugin panels; the selected panel renders while shown.
    pub fn plugin_ui_visibility(&mut self, now: Instant) -> Effect {
        let shown =
            self.layout.panel_visible() && self.layout.panel_view() == PanelView::PluginPanels;
        let effects = self.plugin_ui.visibility(shown, now);
        batch(effects.into_iter().map(Effect::PluginUi).collect())
    }

    /// Runs a command of a plugin with a context the caller built.
    pub fn plugin_ui_run(
        &mut self,
        plugin: &PluginId,
        command: &ContributionId,
        context: CommandContext,
    ) -> Effect {
        Effect::PluginUi(PluginUiEffect::RunCommand {
            plugin: plugin.clone(),
            command: command.clone(),
            context,
        })
    }

    /// Shows a plugin panel in the bottom panel. A panel that failed is asked for again.
    pub fn plugin_ui_show(
        &mut self,
        plugin: &PluginId,
        panel: &ContributionId,
        now: Instant,
    ) -> Effect {
        if !self.plugin_ui.select(plugin, panel) {
            self.notice = Some("That plugin panel is not available any more.".to_owned());
            return Effect::None;
        }
        self.layout.reveal_panel(PanelView::PluginPanels);
        let mut effects = self.plugin_ui.visibility(true, now);
        if effects.is_empty() && self.plugin_ui.selected_failure().is_some() {
            effects = self.plugin_ui.refresh(plugin, panel, now);
        }
        batch(effects.into_iter().map(Effect::PluginUi).collect())
    }

    /// Selects the panel to show (a chip of the bottom panel).
    pub fn plugin_ui_select(
        &mut self,
        plugin: &PluginId,
        panel: &ContributionId,
        now: Instant,
    ) -> Effect {
        if !self.plugin_ui.select(plugin, panel) {
            return Effect::None;
        }
        self.plugin_ui_visibility(now)
    }

    /// The user touched a widget of the selected panel.
    pub fn plugin_ui_widget_event(&mut self, event: WidgetEvent, now: Instant) -> Effect {
        let effects = self.plugin_ui.event(event, now);
        batch(effects.into_iter().map(Effect::PluginUi).collect())
    }

    /// Something a plugin pushed.
    pub fn plugin_ui_event(&mut self, event: PluginUiEvent, now: Instant) -> Effect {
        match event {
            PluginUiEvent::RefreshPanel { plugin, panel } => {
                let effects = self.plugin_ui.refresh(&plugin, &panel, now);
                batch(effects.into_iter().map(Effect::PluginUi).collect())
            }
            PluginUiEvent::SetStatus { plugin, item, text } => {
                self.plugin_ui.set_status(&plugin, &item, &text);
                Effect::None
            }
            PluginUiEvent::Notify { plugin, message } => {
                let live = self
                    .plugins
                    .rows()
                    .iter()
                    .any(|r| r.manifest.id() == &plugin && r.state == RowState::Live);
                if live {
                    let text = format!("{}: {message}", self.plugin_ui.name_of(&plugin));
                    self.plugin_ui.push_notices(vec![text]);
                }
                Effect::None
            }
        }
    }

    /// Takes the result of an [`Effect::PluginUi`].
    pub fn plugin_ui_finished(&mut self, done: PluginUiDone, now: Instant) -> Effect {
        let reaction = self.plugin_ui.done(done, now);
        self.plugin_ui_reaction(reaction)
    }

    /// Takes the messages the plugins' contributions have for the user.
    pub fn take_plugin_ui_notices(&mut self) -> Vec<String> {
        self.plugin_ui.take_notices()
    }

    /// Turns what the state asked for into effects; a failed call also asks the manager to list the plugins again.
    fn plugin_ui_reaction(&mut self, reaction: Reaction) -> Effect {
        self.plugin_ui.push_notices(reaction.notices);
        let mut effects: Vec<Effect> = reaction.effects.into_iter().map(Effect::PluginUi).collect();
        if reaction.reload {
            let reload = self.plugins.reload();
            effects.extend(reload.into_iter().map(Effect::Plugins));
        }
        batch(effects)
    }
}

/// One effect out of many.
fn batch(mut effects: Vec<Effect>) -> Effect {
    effects.retain(|e| !matches!(e, Effect::None));
    match effects.len() {
        0 => Effect::None,
        1 => effects.remove(0),
        _ => Effect::Batch(effects),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::plugins::backend::{FakeBackend, PluginSnapshot};
    use crate::plugins::grant::GrantBook;
    use crate::plugins::manager::fixture::{manifest, row};
    use crate::sidebar::SidebarModel;

    fn sidebar() -> SidebarModel {
        SidebarModel::from_store(sqler_config::ConnectionStore::new(), None)
    }

    fn workspace(rows: Vec<crate::plugins::PluginRow>) -> (Workspace, Arc<FakeBackend>) {
        let fake = FakeBackend::new(PluginSnapshot {
            rows,
            problems: Vec::new(),
        });
        let startup = PluginsStartup::new(Some(fake.clone()), Arc::new(GrantBook::in_memory()));
        (Workspace::new(sidebar()).with_plugins(&startup), fake)
    }

    #[test]
    fn a_workspace_without_plugins_has_an_unavailable_manager() {
        let mut ws = Workspace::new(sidebar());
        assert!(!ws.plugins().available());
        assert!(matches!(ws.plugins_first_frame(), Effect::None));
    }

    #[test]
    fn the_first_frame_is_the_only_start_and_effects_are_redacted() {
        let (mut ws, fake) = workspace(vec![row(manifest("acme-sync", &["db.read"]))]);
        assert_eq!(fake.calls.refresh.load(Ordering::SeqCst), 0);
        let Effect::Plugins(inner) = ws.plugins_first_frame() else {
            panic!("expected a plugins effect");
        };
        assert!(format!("{inner:?}").contains("Load"));
        assert!(matches!(ws.plugins_first_frame(), Effect::None));
        // Planning the work ran nothing.
        assert_eq!(fake.calls.refresh.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_manager_notice_reaches_the_workspace() {
        let (mut ws, _) = workspace(Vec::new());
        assert!(matches!(ws.plugins_first_frame(), Effect::Plugins(_)));
        let follow = ws.plugin_finished(PluginDone::Loaded {
            snapshot: PluginSnapshot::default(),
            review: None,
        });
        assert!(matches!(follow, Effect::None));
        assert!(matches!(
            ws.dispatch_plugin_command(PluginCommand::Enable),
            Effect::None
        ));
        assert_eq!(ws.take_notice().as_deref(), Some("Select a plugin first."));
    }

    // ---- contributions ----

    use crate::plugins::PluginRow;
    use crate::plugins::grant::GrantCommand;
    use crate::plugins::manager::fixture::{contributing, named_with};

    /// A workspace whose manager has loaded `rows`; `decide` records the answer to give each row.
    fn loaded(rows: Vec<PluginRow>, approve: &[&str]) -> Workspace {
        let book = Arc::new(GrantBook::in_memory());
        for r in &rows {
            if approve.contains(&r.manifest.id().as_str()) {
                let prompt = book.prompt(&r.manifest, r.component.as_ref()).unwrap();
                book.decide(&prompt, GrantCommand::Approve).unwrap();
            }
        }
        let fake = FakeBackend::new(PluginSnapshot {
            rows,
            problems: Vec::new(),
        });
        let startup = PluginsStartup::new(Some(fake.clone()), Arc::clone(&book));
        let mut ws = Workspace::new(sidebar()).with_plugins(&startup);
        let Effect::Plugins(load) = ws.plugins_first_frame() else {
            panic!("expected a load");
        };
        let done = load.run_blocking(&*fake, &book);
        assert!(matches!(
            ws.plugin_finished(done),
            Effect::None | Effect::Plugins(_)
        ));
        ws
    }

    fn live(mut r: PluginRow, with_caps: bool) -> PluginRow {
        r.state = RowState::Live;
        if with_caps {
            r.effective = r.manifest.capabilities().clone();
        }
        r
    }

    fn ids(ws: &Workspace) -> Vec<String> {
        ws.plugin_ui()
            .registrations()
            .commands()
            .iter()
            .map(|r| r.id.to_string())
            .collect()
    }

    #[test]
    fn only_approved_active_plugins_contribute_and_a_sync_is_idempotent() {
        let acme = live(row(contributing("acme-sync", "Acme Sync")), true);
        let denied = live(row(contributing("denied-one", "Denied")), true);
        let mut ws = loaded(vec![acme, denied], &["acme-sync"]);
        assert!(ws.plugin_ui_stale());
        let effect = ws.sync_plugin_ui(&|_| false, Instant::now());
        // The status item of the live plugin is asked for.
        assert!(format!("{effect:?}").contains("Status"));
        assert!(!ws.plugin_ui_stale());
        let ids = ids(&ws);
        assert!(ids.contains(&"plugin-acme-sync:sync".to_owned()));
        assert!(ids.contains(&"panel-plugin-acme-sync:overview".to_owned()));
        assert!(ids.iter().all(|id| !id.contains("denied-one")), "{ids:?}");
        assert_eq!(ws.plugin_ui().panels().len(), 1);
    }

    #[test]
    fn a_plugin_without_the_panel_capability_has_no_panel() {
        let acme = live(row(contributing("acme-sync", "Acme Sync")), false);
        let mut ws = loaded(vec![acme], &["acme-sync"]);
        ws.sync_plugin_ui(&|_| false, Instant::now());
        assert_eq!(ws.plugin_ui().panels().len(), 0);
        assert!(ids(&ws).contains(&"plugin-acme-sync:sync".to_owned()));
    }

    #[test]
    fn a_refused_contribution_is_reported_once() {
        let bad = live(
            row(named_with(
                "acme-sync",
                "Acme Sync",
                &["db.read"],
                "[[commands]]\nid = \"run-2x\"\ntitle = \"Bad\"\n\
                 [[commands]]\nid = \"fine\"\ntitle = \"Fine\"\n",
            )),
            true,
        );
        let mut ws = loaded(vec![bad], &["acme-sync"]);
        ws.sync_plugin_ui(&|_| false, Instant::now());
        let notices = ws.take_plugin_ui_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].starts_with("Acme Sync: `plugin-acme-sync:run-2x` was not added"));
        assert_eq!(ids(&ws), ["plugin-acme-sync:fine"]);
        // A second load reports nothing new.
        ws.plugin_finished(PluginDone::Loaded {
            snapshot: PluginSnapshot {
                rows: ws.plugins().rows().to_vec(),
                problems: Vec::new(),
            },
            review: None,
        });
        ws.sync_plugin_ui(&|_| false, Instant::now());
        assert_eq!(ws.take_plugin_ui_notices().len(), 0);
    }

    #[test]
    fn a_taken_id_is_refused() {
        let acme = live(row(contributing("acme-sync", "Acme Sync")), true);
        let mut ws = loaded(vec![acme], &["acme-sync"]);
        ws.sync_plugin_ui(&|id| id.as_str() == "plugin-acme-sync:sync", Instant::now());
        assert!(!ids(&ws).contains(&"plugin-acme-sync:sync".to_owned()));
        assert_eq!(ws.take_plugin_ui_notices().len(), 1);
    }

    #[test]
    fn showing_a_panel_reveals_the_bottom_panel_and_renders_it() {
        let acme = live(row(contributing("acme-sync", "Acme Sync")), true);
        let mut ws = loaded(vec![acme], &["acme-sync"]);
        ws.sync_plugin_ui(&|_| false, Instant::now());
        assert!(!ws.layout().panel_visible());
        let effect = ws.plugin_ui_show(
            &"acme-sync".parse().unwrap(),
            &"overview".parse().unwrap(),
            Instant::now(),
        );
        assert!(ws.layout().panel_visible());
        assert_eq!(ws.layout().panel_view(), PanelView::PluginPanels);
        assert!(matches!(
            effect,
            Effect::PluginUi(PluginUiEffect::Render { .. })
        ));
        // A panel that is not contributed leaves a notice.
        let effect = ws.plugin_ui_show(
            &"acme-sync".parse().unwrap(),
            &"nope".parse().unwrap(),
            Instant::now(),
        );
        assert!(matches!(effect, Effect::None));
        assert!(ws.take_notice().is_some());
    }

    #[test]
    fn a_failed_command_reloads_the_plugins_and_names_the_plugin() {
        let acme = live(row(contributing("acme-sync", "Acme Sync")), true);
        let mut ws = loaded(vec![acme], &["acme-sync"]);
        ws.sync_plugin_ui(&|_| false, Instant::now());
        let effect = ws.plugin_ui_finished(
            PluginUiDone::Command {
                plugin: "acme-sync".parse().unwrap(),
                command: "sync".parse().unwrap(),
                result: Err("it broke".to_owned()),
            },
            Instant::now(),
        );
        assert!(matches!(effect, Effect::Plugins(_)));
        let notices = ws.take_plugin_ui_notices();
        assert_eq!(notices, ["Acme Sync: the command `sync` failed: it broke"]);
    }

    #[test]
    fn a_message_from_a_plugin_that_is_not_live_is_dropped() {
        let idle = row(contributing("acme-sync", "Acme Sync"));
        let live_one = live(row(contributing("live-one", "Live One")), true);
        let mut ws = loaded(vec![idle, live_one], &["acme-sync", "live-one"]);
        ws.sync_plugin_ui(&|_| false, Instant::now());
        let now = Instant::now();
        ws.plugin_ui_event(
            PluginUiEvent::Notify {
                plugin: "acme-sync".parse().unwrap(),
                message: "hello".to_owned(),
            },
            now,
        );
        assert_eq!(ws.take_plugin_ui_notices().len(), 0);
        ws.plugin_ui_event(
            PluginUiEvent::Notify {
                plugin: "live-one".parse().unwrap(),
                message: "hello".to_owned(),
            },
            now,
        );
        assert_eq!(ws.take_plugin_ui_notices(), ["Live One: hello"]);
    }
}
