// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plugin manager inside the workspace (ADR-0060).
//!
//! State only, like the audit viewer: each call changes [`PluginsState`] and returns the work to carry out as
//! [`Effect::Plugins`]; the view runs it on the background executor and reports with
//! [`Workspace::plugin_finished`]. Nothing here runs before the first frame.

use sqler_plugin_api::PluginId;

use super::state::{Effect, Workspace};
use crate::plugins::grant::GrantCommand;
use crate::plugins::manager::{PluginDone, PluginEffect, PluginsState};
use crate::plugins::{PluginCommand, PluginsStartup};

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
}
