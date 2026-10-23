// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What the plugins add to the window (ADR-0060): palette commands, menu commands, panels and status items.
//!
//! The registrations are built from the rows the manager loaded, never from the host. Every id is namespaced
//! (`plugin-<plugin>:<command>` and one prefixed form per menu location and for showing a panel) and checked with
//! [`CommandId::new`]; an id that does not fit the grammar, or that is already taken, is refused with a [`Refusal`]
//! and the rest of the plugin still registers. A plugin that is off, faulted, failed to load or not approved
//! contributes nothing.

mod state;

use std::collections::HashSet;

use sqler_driver_api::sqler_core::ObjectKind;
use sqler_plugin_api::{
    Alignment, CapabilityKind, ContributionId, MenuLocation, PluginId, command_id,
};

pub use state::{PluginUiDone, PluginUiEffect, PluginUiState, Reaction, SyncInput};

use super::backend::{PluginRow, RowState};
use crate::command::{Command, CommandError, CommandId, CommandRegistry};

/// The prefix of the commands that show a plugin panel.
const PANEL_PREFIX: &str = "panel-";
/// The prefix of the object menu commands.
const OBJECT_PREFIX: &str = "object-";
/// The prefix of the editor menu commands.
const EDITOR_PREFIX: &str = "editor-";
/// The prefix of the results menu commands.
const RESULTS_PREFIX: &str = "results-";

/// Whether a command category belongs to the plugin namespaces. The key dispatcher does not warn about a keybinding
/// for such a category at startup (the plugins are not registered yet), and the registry drops these commands before
/// it registers the current ones.
#[must_use]
pub fn is_plugin_category(category: &str) -> bool {
    [
        "plugin-",
        "object-plugin-",
        "editor-plugin-",
        "results-plugin-",
        "panel-plugin-",
    ]
    .iter()
    .any(|prefix| category.starts_with(prefix))
}

/// How a registered command reaches its plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// The palette: no context.
    Palette,
    /// The object menu, for these kinds (empty = every kind).
    Object(Vec<ObjectKind>),
    /// The editor menu.
    Editor,
    /// The results menu.
    Results,
}

/// What a registered command does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run a command of the plugin.
    Run {
        /// The command.
        command: ContributionId,
        /// Where it came from.
        via: Via,
    },
    /// Show a panel of the plugin.
    ShowPanel(ContributionId),
}

/// One command to register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The command id.
    pub id: CommandId,
    /// The palette title.
    pub title: String,
    /// The plugin.
    pub plugin: PluginId,
    /// The plugin's display name.
    pub plugin_name: String,
    /// What running it does.
    pub action: Action,
}

/// A panel a plugin contributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelReg {
    /// The plugin.
    pub plugin: PluginId,
    /// The plugin's display name.
    pub plugin_name: String,
    /// The panel.
    pub panel: ContributionId,
    /// The panel title.
    pub title: String,
    /// The command that shows it.
    pub show: CommandId,
}

/// A status bar item a plugin contributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReg {
    /// The plugin.
    pub plugin: PluginId,
    /// The plugin's display name.
    pub plugin_name: String,
    /// The item.
    pub item: ContributionId,
    /// The tooltip from the manifest.
    pub tooltip: Option<String>,
    /// The side of the bar.
    pub alignment: Alignment,
}

/// A contribution that was not added, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The plugin's display name.
    pub plugin: String,
    /// The id that was refused.
    pub id: String,
    /// Why, in one line.
    pub reason: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: `{}` was not added: {}",
            self.plugin, self.id, self.reason
        )
    }
}

/// Everything the active plugins contribute, in load order and then manifest order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registrations {
    commands: Vec<Registration>,
    panels: Vec<PanelReg>,
    statuses: Vec<StatusReg>,
}

/// Whether a row contributes: on, started or startable.
fn active(row: &PluginRow) -> bool {
    row.enabled && matches!(row.state, RowState::Idle | RowState::Live)
}

/// The prefix a menu location adds to the command id.
fn menu_prefix(location: MenuLocation) -> Option<&'static str> {
    match location {
        MenuLocation::ObjectBrowser => Some(OBJECT_PREFIX),
        MenuLocation::Editor => Some(EDITOR_PREFIX),
        MenuLocation::Results => Some(RESULTS_PREFIX),
        _ => None,
    }
}

/// The menu label of a location.
fn menu_label(location: MenuLocation) -> &'static str {
    match location {
        MenuLocation::ObjectBrowser => "Object",
        MenuLocation::Editor => "Editor",
        _ => "Results",
    }
}

impl Registrations {
    /// The registrations of the rows that are active (on, `Idle` or `Live`). `taken` says whether a command id is
    /// already in use by the application; ids a plugin registers twice are refused too.
    #[must_use]
    pub fn from_rows(
        rows: &[PluginRow],
        taken: &dyn Fn(&CommandId) -> bool,
    ) -> (Self, Vec<Refusal>) {
        Self::from_approved(rows, taken, &|_| true)
    }

    /// Like [`Registrations::from_rows`], and a row also needs `approved` (the grant is `Approved`).
    #[must_use]
    pub fn from_approved(
        rows: &[PluginRow],
        taken: &dyn Fn(&CommandId) -> bool,
        approved: &dyn Fn(&PluginRow) -> bool,
    ) -> (Self, Vec<Refusal>) {
        let mut out = Self::default();
        let mut refusals = Vec::new();
        let mut seen: HashSet<CommandId> = HashSet::new();
        for row in rows.iter().filter(|r| active(r) && approved(r)) {
            out.add_row(row, taken, &mut seen, &mut refusals);
        }
        (out, refusals)
    }

    fn add_row(
        &mut self,
        row: &PluginRow,
        taken: &dyn Fn(&CommandId) -> bool,
        seen: &mut HashSet<CommandId>,
        refusals: &mut Vec<Refusal>,
    ) {
        let manifest = &row.manifest;
        let (plugin, name) = (manifest.id(), manifest.name());
        let contributes = manifest.contributes();
        let mut claim = |raw: String, refusals: &mut Vec<Refusal>| -> Option<CommandId> {
            let refuse = |reason: &str| Refusal {
                plugin: name.to_owned(),
                id: raw.clone(),
                reason: reason.to_owned(),
            };
            let Ok(id) = CommandId::new(&raw) else {
                refusals.push(refuse(
                    "it is not a valid command id (every part must start with a letter)",
                ));
                return None;
            };
            if taken(&id) || !seen.insert(id.clone()) {
                refusals.push(refuse("another command already uses that id"));
                return None;
            }
            Some(id)
        };
        for command in &contributes.commands {
            if let Some(id) = claim(command_id(plugin, &command.id), refusals) {
                self.commands.push(Registration {
                    id,
                    title: format!("{name} (plugin): {}", command.title),
                    plugin: plugin.clone(),
                    plugin_name: name.to_owned(),
                    action: Action::Run {
                        command: command.id.clone(),
                        via: Via::Palette,
                    },
                });
            }
        }
        for entry in &contributes.menus {
            let Some(prefix) = menu_prefix(entry.location) else {
                continue;
            };
            let Some(command) = contributes.commands.iter().find(|c| c.id == entry.command) else {
                continue;
            };
            let raw = format!("{prefix}{}", command_id(plugin, &command.id));
            if let Some(id) = claim(raw, refusals) {
                let via = match entry.location {
                    MenuLocation::ObjectBrowser => Via::Object(entry.object_kinds.clone()),
                    MenuLocation::Editor => Via::Editor,
                    _ => Via::Results,
                };
                self.commands.push(Registration {
                    id,
                    title: format!("{}: {} ({name})", menu_label(entry.location), command.title),
                    plugin: plugin.clone(),
                    plugin_name: name.to_owned(),
                    action: Action::Run {
                        command: command.id.clone(),
                        via,
                    },
                });
            }
        }
        if row.effective.allows(CapabilityKind::UiPanel) {
            for panel in &contributes.panels {
                let raw = format!("{PANEL_PREFIX}{}", command_id(plugin, &panel.id));
                if let Some(show) = claim(raw, refusals) {
                    self.commands.push(Registration {
                        id: show.clone(),
                        title: format!("View: Show {} ({name})", panel.title),
                        plugin: plugin.clone(),
                        plugin_name: name.to_owned(),
                        action: Action::ShowPanel(panel.id.clone()),
                    });
                    self.panels.push(PanelReg {
                        plugin: plugin.clone(),
                        plugin_name: name.to_owned(),
                        panel: panel.id.clone(),
                        title: panel.title.clone(),
                        show,
                    });
                }
            }
        }
        for item in &contributes.status_items {
            self.statuses.push(StatusReg {
                plugin: plugin.clone(),
                plugin_name: name.to_owned(),
                item: item.id.clone(),
                tooltip: item.tooltip.clone(),
                alignment: item.alignment,
            });
        }
    }

    /// The commands to register, in load order and manifest order.
    #[must_use]
    pub fn commands(&self) -> &[Registration] {
        &self.commands
    }

    /// The panels, in load order and manifest order.
    #[must_use]
    pub fn panels(&self) -> &[PanelReg] {
        &self.panels
    }

    /// The status items, in load order and manifest order.
    #[must_use]
    pub fn statuses(&self) -> &[StatusReg] {
        &self.statuses
    }

    /// The registration behind a command id.
    #[must_use]
    pub fn find(&self, id: &CommandId) -> Option<&Registration> {
        self.commands.iter().find(|r| r.id == *id)
    }

    /// Registers every command in `registry`.
    ///
    /// # Errors
    ///
    /// [`CommandError::Duplicate`] when an id is already registered (the caller checks `taken` first).
    pub fn register(&self, registry: &mut CommandRegistry) -> Result<(), CommandError> {
        for r in &self.commands {
            registry.register(Command::new(r.id.as_str(), r.title.clone())?)?;
        }
        Ok(())
    }

    /// Whether nothing is contributed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.panels.is_empty() && self.statuses.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use sqler_plugin_api::{CapabilitySet, Manifest};

    use super::*;
    use crate::plugins::manager::fixture::{row, row_with};

    fn manifest(id: &str, name: &str, body: &str) -> Manifest {
        let text = format!(
            "[plugin]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"1.0.0\"\napi = \"0.1\"\n\
             capabilities = [\"ui.panel\"]\n[entry]\ncomponent = \"bin/plugin.wasm\"\n{body}"
        );
        Manifest::parse(&text).unwrap()
    }

    const FULL: &str = "[[commands]]\nid = \"run\"\ntitle = \"Run it\"\n\
        [[commands]]\nid = \"scan\"\ntitle = \"Scan\"\n\
        [[menus]]\ncommand = \"scan\"\nlocation = \"object-browser\"\nobject-kinds = [\"table\"]\n\
        [[menus]]\ncommand = \"run\"\nlocation = \"editor\"\n\
        [[menus]]\ncommand = \"run\"\nlocation = \"results\"\n\
        [[panels]]\nid = \"main\"\ntitle = \"Main\"\n\
        [[status-items]]\nid = \"env\"\ntooltip = \"Environment\"\nalignment = \"left\"\n";

    fn with_panel(m: Manifest) -> PluginRow {
        row_with(m, |r| {
            r.effective = r.manifest.capabilities().clone();
            r.state = RowState::Live;
        })
    }

    fn none(_: &CommandId) -> bool {
        false
    }

    #[test]
    fn ids_are_namespaced_and_titled() {
        let rows = [with_panel(manifest("acme-sync", "Acme Sync", FULL))];
        let (regs, refused) = Registrations::from_rows(&rows, &none);
        assert!(refused.is_empty(), "{refused:?}");
        let ids: Vec<&str> = regs.commands().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "plugin-acme-sync:run",
                "plugin-acme-sync:scan",
                "object-plugin-acme-sync:scan",
                "editor-plugin-acme-sync:run",
                "results-plugin-acme-sync:run",
                "panel-plugin-acme-sync:main",
            ]
        );
        let titles: Vec<&str> = regs.commands().iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles[0], "Acme Sync (plugin): Run it");
        assert_eq!(titles[2], "Object: Scan (Acme Sync)");
        assert_eq!(titles[3], "Editor: Run it (Acme Sync)");
        assert_eq!(titles[4], "Results: Run it (Acme Sync)");
        assert_eq!(titles[5], "View: Show Main (Acme Sync)");
        assert_eq!(regs.panels().len(), 1);
        assert_eq!(regs.statuses().len(), 1);
        assert_eq!(regs.statuses()[0].alignment, Alignment::Left);
        let mut registry = CommandRegistry::new();
        regs.register(&mut registry).unwrap();
        assert_eq!(registry.len(), 6);
        assert!(registry.iter().all(|c| is_plugin_category(c.category())));
    }

    /// Every built-in command list the window registers, in one registry.
    fn built_in_registry() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        crate::ai::register_commands(&mut r).unwrap();
        crate::browser::register_commands(&mut r).unwrap();
        crate::connections::register_commands(&mut r).unwrap();
        crate::editor::register_commands(&mut r).unwrap();
        crate::export::register_commands(&mut r).unwrap();
        crate::history::register_commands(&mut r).unwrap();
        crate::palette::register_commands(&mut r).unwrap();
        crate::plugins::action::register_commands(&mut r).unwrap();
        crate::query::register_commands(&mut r).unwrap();
        crate::workbench::register_commands(&mut r).unwrap();
        crate::workbench::register_pane_commands(&mut r).unwrap();
        crate::workspace::register_commands(&mut r).unwrap();
        r
    }

    #[test]
    fn no_built_in_category_looks_like_a_plugin_one() {
        let built_in = built_in_registry();
        assert!(built_in.len() > 50, "the lists were registered");
        for command in built_in.iter() {
            assert!(
                !is_plugin_category(command.category()),
                "{} would be dropped as a plugin command",
                command.id().as_str()
            );
        }
    }

    #[test]
    fn a_plugin_title_never_equals_a_built_in_title() {
        let built_in = built_in_registry();
        let titles: HashSet<&str> = built_in.iter().map(Command::title).collect();
        // The worst plugin name: one that makes the old "<name>: <title>" form equal "Query: Run All".
        let hostile = "[[commands]]\nid = \"run\"\ntitle = \"Run All\"\n";
        let rows = [with_panel(manifest("query", "Query", hostile))];
        let (regs, _) = Registrations::from_rows(&rows, &none);
        for reg in regs.commands() {
            assert!(!titles.contains(reg.title.as_str()), "{}", reg.title);
        }
        assert!(titles.iter().all(|t| !t.contains("(plugin)")));
    }

    #[test]
    fn an_object_menu_entry_keeps_its_kinds() {
        let rows = [with_panel(manifest("acme-sync", "Acme Sync", FULL))];
        let (regs, _) = Registrations::from_rows(&rows, &none);
        let id = CommandId::new("object-plugin-acme-sync:scan").unwrap();
        let reg = regs.find(&id).unwrap();
        assert_eq!(
            reg.action,
            Action::Run {
                command: "scan".parse().unwrap(),
                via: Via::Object(vec![ObjectKind::Table])
            }
        );
    }

    #[test]
    fn an_invalid_id_is_refused_and_the_rest_stays() {
        let body = "[[commands]]\nid = \"run-2x\"\ntitle = \"Bad\"\n\
            [[commands]]\nid = \"ok\"\ntitle = \"Good\"\n";
        let rows = [row(manifest("acme-sync", "Acme Sync", body))];
        let (regs, refused) = Registrations::from_rows(&rows, &none);
        assert_eq!(regs.commands().len(), 1);
        assert_eq!(regs.commands()[0].id.as_str(), "plugin-acme-sync:ok");
        assert_eq!(refused.len(), 1);
        let text = refused[0].to_string();
        assert!(text.starts_with("Acme Sync: `plugin-acme-sync:run-2x` was not added: "));
        assert!(!text.contains('\n'));
    }

    #[test]
    fn a_taken_id_is_refused() {
        let body = "[[commands]]\nid = \"run\"\ntitle = \"Run\"\n";
        let rows = [row(manifest("acme-sync", "Acme Sync", body))];
        let taken = |id: &CommandId| id.as_str() == "plugin-acme-sync:run";
        let (regs, refused) = Registrations::from_rows(&rows, &taken);
        assert!(regs.is_empty());
        assert_eq!(refused.len(), 1);
        assert!(refused[0].reason.contains("already"));
    }

    #[test]
    fn the_same_plugin_twice_is_a_duplicate() {
        let body = "[[commands]]\nid = \"run\"\ntitle = \"Run\"\n";
        let one = row(manifest("acme-sync", "Acme Sync", body));
        let (regs, refused) = Registrations::from_rows(&[one.clone(), one], &none);
        assert_eq!(regs.commands().len(), 1);
        assert_eq!(refused.len(), 1);
    }

    #[test]
    fn only_active_rows_contribute() {
        let body = "[[commands]]\nid = \"run\"\ntitle = \"Run\"\n";
        let make = |f: fn(&mut PluginRow)| row_with(manifest("acme-sync", "Acme", body), f);
        for (label, r) in [
            ("off", make(|r| r.enabled = false)),
            ("off state", make(|r| r.state = RowState::Off)),
            (
                "faulted",
                make(|r| r.state = RowState::Faulted { count: 3 }),
            ),
            ("failed", make(|r| r.state = RowState::LoadFailed)),
        ] {
            let (regs, refused) = Registrations::from_rows(&[r], &none);
            assert!(regs.is_empty(), "{label}");
            assert!(refused.is_empty(), "{label}");
        }
        for state in [RowState::Idle, RowState::Live] {
            let r = row_with(manifest("acme-sync", "Acme", body), |r| r.state = state);
            let (regs, _) = Registrations::from_rows(&[r], &none);
            assert_eq!(regs.commands().len(), 1);
        }
        // A denied or undecided grant removes everything.
        let r = row(manifest("acme-sync", "Acme", body));
        let (regs, _) = Registrations::from_approved(&[r], &none, &|_| false);
        assert!(regs.is_empty());
    }

    #[test]
    fn a_panel_needs_the_ui_panel_capability() {
        let body = "[[panels]]\nid = \"main\"\ntitle = \"Main\"\n\
            [[commands]]\nid = \"run\"\ntitle = \"Run\"\n";
        let mut r = row(manifest("acme-sync", "Acme", body));
        r.effective = CapabilitySet::new();
        let (regs, _) = Registrations::from_rows(&[r], &none);
        assert_eq!(regs.panels().len(), 0);
        assert_eq!(regs.commands().len(), 1, "commands need no capability");
    }

    #[test]
    fn load_order_then_manifest_order() {
        let a =
            "[[commands]]\nid = \"b\"\ntitle = \"B\"\n[[commands]]\nid = \"a\"\ntitle = \"A\"\n";
        let rows = [
            row(manifest("zeta-plugin", "Zeta", a)),
            row(manifest("alpha-plugin", "Alpha", a)),
        ];
        let (regs, _) = Registrations::from_rows(&rows, &none);
        let ids: Vec<&str> = regs.commands().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "plugin-zeta-plugin:b",
                "plugin-zeta-plugin:a",
                "plugin-alpha-plugin:b",
                "plugin-alpha-plugin:a"
            ]
        );
    }

    #[test]
    fn plugin_categories_are_recognised() {
        for c in [
            "plugin-acme",
            "object-plugin-acme",
            "editor-plugin-x",
            "results-plugin-x",
            "panel-plugin-x",
        ] {
            assert!(is_plugin_category(c), "{c}");
        }
        for c in [
            "plugins",
            "object",
            "editor",
            "workbench",
            "panel",
            "results",
        ] {
            assert!(!is_plugin_category(c), "{c}");
        }
    }
}
