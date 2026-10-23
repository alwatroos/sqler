// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `PluginUiState`: what the plugins contribute right now and the state of their panels and status items.
//!
//! Headless: a call changes the state and returns the [`PluginUiEffect`]s to carry out against the host; the view
//! runs them and reports each result back with [`PluginUiState::done`]. Nothing here touches the clock: callers pass
//! the time.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use sqler_plugin_api::{CommandContext, ContributionId, PluginId, WidgetEvent, WidgetTree};

use super::{PanelReg, Refusal, Registrations};
use crate::plugins::backend::PanelRender;
use crate::plugins::panel::{PanelSlot, SlotView, Step};
use crate::plugins::status::StatusModel;

type PanelKey = (PluginId, ContributionId);

/// Work against the plugin host.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginUiEffect {
    /// Run a command of a plugin.
    RunCommand {
        /// The plugin.
        plugin: PluginId,
        /// The command.
        command: ContributionId,
        /// Where it was invoked.
        context: CommandContext,
    },
    /// Ask a plugin to draw a panel.
    Render {
        /// The plugin.
        plugin: PluginId,
        /// The panel.
        panel: ContributionId,
    },
    /// Ask a plugin to draw a panel after a wait (the 100 ms gap).
    RenderAfter {
        /// The plugin.
        plugin: PluginId,
        /// The panel.
        panel: ContributionId,
        /// How long to wait.
        delay: Duration,
    },
    /// Send a widget event to a plugin panel.
    Event {
        /// The plugin.
        plugin: PluginId,
        /// The panel.
        panel: ContributionId,
        /// The event.
        event: WidgetEvent,
    },
    /// Ask a plugin for the text of a status item.
    Status {
        /// The plugin.
        plugin: PluginId,
        /// The item.
        item: ContributionId,
    },
}

// Ids only: never the editor selection, a typed value or the columns of a result.
impl std::fmt::Debug for PluginUiEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RunCommand {
                plugin, command, ..
            } => f
                .debug_struct("RunCommand")
                .field("plugin", &plugin.as_str())
                .field("command", &command.as_str())
                .finish_non_exhaustive(),
            Self::Render { plugin, panel } => f
                .debug_struct("Render")
                .field("plugin", &plugin.as_str())
                .field("panel", &panel.as_str())
                .finish(),
            Self::RenderAfter {
                plugin,
                panel,
                delay,
            } => f
                .debug_struct("RenderAfter")
                .field("plugin", &plugin.as_str())
                .field("panel", &panel.as_str())
                .field("delay", delay)
                .finish(),
            Self::Event { plugin, panel, .. } => f
                .debug_struct("Event")
                .field("plugin", &plugin.as_str())
                .field("panel", &panel.as_str())
                .finish_non_exhaustive(),
            Self::Status { plugin, item } => f
                .debug_struct("Status")
                .field("plugin", &plugin.as_str())
                .field("item", &item.as_str())
                .finish(),
        }
    }
}

/// What a [`PluginUiEffect`] came to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginUiDone {
    /// A command ended.
    Command {
        /// The plugin.
        plugin: PluginId,
        /// The command.
        command: ContributionId,
        /// `Err` carries the one-line reason.
        result: Result<(), String>,
    },
    /// A panel was drawn.
    Rendered {
        /// The plugin.
        plugin: PluginId,
        /// The panel.
        panel: ContributionId,
        /// The tree, or why not.
        result: PanelRender,
    },
    /// An event was answered.
    EventReply {
        /// The plugin.
        plugin: PluginId,
        /// The panel.
        panel: ContributionId,
        /// The replacement tree (if any), or why not.
        result: Result<Option<WidgetTree>, String>,
    },
    /// A status text arrived.
    Status {
        /// The plugin.
        plugin: PluginId,
        /// The item.
        item: ContributionId,
        /// The text (`None` hides the item), or why not.
        result: Result<Option<String>, String>,
    },
}

/// What the registrations are built from, after a manager load.
#[derive(Debug, Default)]
pub struct SyncInput {
    /// The manager's generation.
    pub generation: u64,
    /// What the active plugins contribute.
    pub regs: Registrations,
    /// What was refused.
    pub refusals: Vec<Refusal>,
    /// The plugins that are `Live`.
    pub live: HashSet<PluginId>,
    /// The display name of every plugin.
    pub names: HashMap<PluginId, String>,
}

/// What a call of the state asks the window to do besides the effects.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reaction {
    /// Work against the host.
    pub effects: Vec<PluginUiEffect>,
    /// Messages for the user.
    pub notices: Vec<String>,
    /// A call failed: ask the manager to reload, so a fault shows up.
    pub reload: bool,
}

/// What the plugins contribute and how their panels and status items fare.
#[derive(Debug, Default)]
pub struct PluginUiState {
    generation: u64,
    regs: Registrations,
    status: StatusModel,
    slots: HashMap<PanelKey, PanelSlot>,
    selected: Option<PanelKey>,
    refused: HashSet<String>,
    names: HashMap<PluginId, String>,
    notices: Vec<String>,
}

fn effect_of(step: Step, key: &PanelKey) -> Option<PluginUiEffect> {
    let (plugin, panel) = key.clone();
    Some(match step {
        Step::Nothing => return None,
        Step::Render => PluginUiEffect::Render { plugin, panel },
        Step::RenderAfter(delay) => PluginUiEffect::RenderAfter {
            plugin,
            panel,
            delay,
        },
        Step::Event(event) => PluginUiEffect::Event {
            plugin,
            panel,
            event,
        },
    })
}

impl PluginUiState {
    /// The generation of the manager load the registrations were built from.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// What is contributed now.
    #[must_use]
    pub fn registrations(&self) -> &Registrations {
        &self.regs
    }

    /// The status items.
    #[must_use]
    pub fn status(&self) -> &StatusModel {
        &self.status
    }

    /// The panels, in registration order.
    #[must_use]
    pub fn panels(&self) -> &[PanelReg] {
        self.regs.panels()
    }

    /// The panel shown in the bottom panel.
    #[must_use]
    pub fn selected(&self) -> Option<&PanelReg> {
        let key = self.selected.as_ref()?;
        self.regs
            .panels()
            .iter()
            .find(|p| p.plugin == key.0 && p.panel == key.1)
    }

    /// The slot of the selected panel.
    #[must_use]
    pub fn selected_slot(&self) -> Option<&PanelSlot> {
        self.slots.get(self.selected.as_ref()?)
    }

    /// The slot of a panel.
    #[must_use]
    pub fn slot(&self, plugin: &PluginId, panel: &ContributionId) -> Option<&PanelSlot> {
        self.slots.get(&(plugin.clone(), panel.clone()))
    }

    /// Keeps messages for the user until the view shows them.
    pub fn push_notices(&mut self, notices: Vec<String>) {
        self.notices.extend(notices);
    }

    /// Takes the messages for the user.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// The display name of a plugin.
    #[must_use]
    pub fn name_of(&self, plugin: &PluginId) -> String {
        self.names
            .get(plugin)
            .cloned()
            .unwrap_or_else(|| plugin.to_string())
    }

    /// Takes in the registrations after a manager load. Panels that are still contributed keep their slot; the
    /// others are dropped. Refusals are returned once each. Plugins that turned `Live` are asked for their status
    /// texts.
    pub fn sync(&mut self, input: SyncInput) -> Reaction {
        let mut reaction = Reaction::default();
        self.generation = input.generation;
        self.names = input.names;
        self.regs = input.regs;
        for refusal in input.refusals {
            let text = refusal.to_string();
            if self.refused.insert(text.clone()) {
                reaction.notices.push(text);
            }
        }
        let keys: Vec<PanelKey> = self
            .regs
            .panels()
            .iter()
            .map(|p| (p.plugin.clone(), p.panel.clone()))
            .collect();
        self.slots.retain(|k, _| keys.contains(k));
        for key in &keys {
            self.slots.entry(key.clone()).or_default();
        }
        if self.selected.as_ref().is_none_or(|k| !keys.contains(k)) {
            self.selected = keys.first().cloned();
        }
        for (plugin, item) in self.status.sync(self.regs.statuses(), &input.live) {
            reaction
                .effects
                .push(PluginUiEffect::Status { plugin, item });
        }
        reaction
    }

    /// Selects the panel to show; `false` when it is not contributed.
    pub fn select(&mut self, plugin: &PluginId, panel: &ContributionId) -> bool {
        let key = (plugin.clone(), panel.clone());
        if !self.slots.contains_key(&key) {
            return false;
        }
        self.selected = Some(key);
        true
    }

    /// The bottom panel shows the plugin panels (or not): the selected panel is shown, every other is hidden.
    pub fn visibility(&mut self, plugin_view_shown: bool, now: Instant) -> Vec<PluginUiEffect> {
        let selected = self.selected.clone();
        let mut keys: Vec<PanelKey> = self.slots.keys().cloned().collect();
        keys.sort();
        keys.into_iter()
            .filter_map(|key| {
                let shown = plugin_view_shown && selected.as_ref() == Some(&key);
                let step = self.slots.get_mut(&key)?.set_shown(shown, now);
                effect_of(step, &key)
            })
            .collect()
    }

    /// The plugin asks for a panel to be drawn again, or the user asks to reload it.
    pub fn refresh(
        &mut self,
        plugin: &PluginId,
        panel: &ContributionId,
        now: Instant,
    ) -> Vec<PluginUiEffect> {
        let key = (plugin.clone(), panel.clone());
        let Some(slot) = self.slots.get_mut(&key) else {
            return Vec::new();
        };
        let step = slot.refresh(now);
        effect_of(step, &key).into_iter().collect()
    }

    /// The user touched a widget of the selected panel.
    pub fn event(&mut self, event: WidgetEvent, now: Instant) -> Vec<PluginUiEffect> {
        let Some(key) = self.selected.clone() else {
            return Vec::new();
        };
        let Some(slot) = self.slots.get_mut(&key) else {
            return Vec::new();
        };
        let step = slot.event(event, now);
        effect_of(step, &key).into_iter().collect()
    }

    /// A plugin pushed a status text. Ignored when the item is not contributed.
    pub fn set_status(&mut self, plugin: &PluginId, item: &ContributionId, text: &str) -> bool {
        self.status.set(plugin, item, text)
    }

    /// Takes in what an effect came to.
    pub fn done(&mut self, done: PluginUiDone, now: Instant) -> Reaction {
        let mut reaction = Reaction::default();
        match done {
            PluginUiDone::Command {
                plugin,
                command,
                result,
            } => {
                if let Err(why) = result {
                    reaction.notices.push(format!(
                        "{}: the command `{command}` failed: {why}",
                        self.name_of(&plugin)
                    ));
                    reaction.reload = true;
                }
            }
            PluginUiDone::Rendered {
                plugin,
                panel,
                result,
            } => {
                let key = (plugin, panel);
                if let Some(slot) = self.slots.get_mut(&key) {
                    let outcome = slot.rendered(result, now);
                    reaction.reload = outcome.failed;
                    reaction.effects.extend(effect_of(outcome.step, &key));
                }
            }
            PluginUiDone::EventReply {
                plugin,
                panel,
                result,
            } => {
                let key = (plugin, panel);
                if let Some(slot) = self.slots.get_mut(&key) {
                    let outcome = slot.event_replied(result, now);
                    reaction.reload = outcome.failed;
                    reaction.effects.extend(effect_of(outcome.step, &key));
                }
            }
            PluginUiDone::Status {
                plugin,
                item,
                result,
            } => self.status.answered(&plugin, &item, &result),
        }
        reaction
    }

    /// Whether the selected panel is failed, with the reason.
    #[must_use]
    pub fn selected_failure(&self) -> Option<&str> {
        match self.selected_slot()?.view() {
            SlotView::Failed(reason) => Some(reason),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use sqler_plugin_api::{Alignment, Direction, Gap, Stack, Widget, WidgetNode};

    use super::*;
    use crate::plugins::contrib::StatusReg;

    fn pid(s: &str) -> PluginId {
        s.parse().unwrap()
    }

    fn cid(s: &str) -> ContributionId {
        s.parse().unwrap()
    }

    fn panel(plugin: &str, id: &str) -> PanelReg {
        PanelReg {
            plugin: pid(plugin),
            plugin_name: plugin.to_owned(),
            panel: cid(id),
            title: id.to_owned(),
            show: crate::command::CommandId::new(&format!("panel-plugin-{plugin}:{id}")).unwrap(),
        }
    }

    fn input(panels: &[PanelReg], generation: u64) -> SyncInput {
        let mut regs = Registrations::default();
        regs.panels.extend_from_slice(panels);
        SyncInput {
            generation,
            regs,
            ..SyncInput::default()
        }
    }

    fn tree() -> WidgetTree {
        WidgetTree::new(vec![WidgetNode {
            widget: Widget::Stack(Stack {
                direction: Direction::Vertical,
                gap: Gap::None,
            }),
            children: Vec::new(),
        }])
        .unwrap()
    }

    #[test]
    fn the_first_panel_is_selected_and_removed_ones_lose_their_slot() {
        let mut s = PluginUiState::default();
        s.sync(input(
            &[panel("acme-sync", "a"), panel("acme-sync", "b")],
            1,
        ));
        assert_eq!(s.selected().unwrap().panel, cid("a"));
        assert!(s.select(&pid("acme-sync"), &cid("b")));
        assert!(!s.select(&pid("acme-sync"), &cid("zzz")));
        s.sync(input(&[panel("acme-sync", "a")], 2));
        assert_eq!(s.generation(), 2);
        assert_eq!(s.selected().unwrap().panel, cid("a"), "falls back");
        assert!(s.slot(&pid("acme-sync"), &cid("b")).is_none());
        s.sync(input(&[], 3));
        assert!(s.selected().is_none());
    }

    #[test]
    fn only_the_selected_panel_renders_and_only_while_shown() {
        let mut s = PluginUiState::default();
        s.sync(input(
            &[panel("acme-sync", "a"), panel("acme-sync", "b")],
            1,
        ));
        let now = Instant::now();
        assert_eq!(s.visibility(false, now).len(), 0);
        let effects = s.visibility(true, now);
        assert_eq!(
            effects,
            [PluginUiEffect::Render {
                plugin: pid("acme-sync"),
                panel: cid("a")
            }]
        );
        s.select(&pid("acme-sync"), &cid("b"));
        let effects = s.visibility(true, now);
        assert_eq!(effects.len(), 1);
        assert!(matches!(&effects[0], PluginUiEffect::Render { panel, .. } if *panel == cid("b")));
    }

    #[test]
    fn a_failed_render_asks_for_a_reload_and_shows_the_reason() {
        let mut s = PluginUiState::default();
        s.sync(input(&[panel("acme-sync", "a")], 1));
        let now = Instant::now();
        s.visibility(true, now);
        let r = s.done(
            PluginUiDone::Rendered {
                plugin: pid("acme-sync"),
                panel: cid("a"),
                result: Err("the plugin trapped".into()),
            },
            now,
        );
        assert!(r.reload);
        assert_eq!(s.selected_failure(), Some("the plugin trapped"));
        let r = s.done(
            PluginUiDone::Rendered {
                plugin: pid("acme-sync"),
                panel: cid("a"),
                result: Ok(tree()),
            },
            now,
        );
        assert!(!r.reload);
        assert!(s.selected_failure().is_none());
    }

    #[test]
    fn a_failed_command_gives_a_notice_with_the_plugin_name() {
        let mut s = PluginUiState::default();
        let mut i = input(&[], 1);
        i.names.insert(pid("acme-sync"), "Acme Sync".into());
        s.sync(i);
        let r = s.done(
            PluginUiDone::Command {
                plugin: pid("acme-sync"),
                command: cid("run"),
                result: Err("it broke".into()),
            },
            Instant::now(),
        );
        assert_eq!(
            r.notices,
            ["Acme Sync: the command `run` failed: it broke".to_owned()]
        );
        assert!(r.reload);
        let ok = s.done(
            PluginUiDone::Command {
                plugin: pid("acme-sync"),
                command: cid("run"),
                result: Ok(()),
            },
            Instant::now(),
        );
        assert_eq!(ok, Reaction::default());
    }

    #[test]
    fn refusals_are_reported_once() {
        let mut s = PluginUiState::default();
        let refusal = Refusal {
            plugin: "Acme".into(),
            id: "plugin-acme:run-2x".into(),
            reason: "bad".into(),
        };
        let mut i = input(&[], 1);
        i.refusals = vec![refusal.clone()];
        assert_eq!(s.sync(i).notices.len(), 1);
        let mut i = input(&[], 2);
        i.refusals = vec![refusal];
        assert_eq!(s.sync(i).notices.len(), 0);
    }

    #[test]
    fn status_items_are_polled_once_and_events_set_them() {
        let mut s = PluginUiState::default();
        let mut i = input(&[], 1);
        i.regs.statuses.push(StatusReg {
            plugin: pid("acme-sync"),
            plugin_name: "Acme".into(),
            item: cid("env"),
            tooltip: None,
            alignment: Alignment::Right,
        });
        i.live.insert(pid("acme-sync"));
        let r = s.sync(i);
        assert_eq!(
            r.effects,
            [PluginUiEffect::Status {
                plugin: pid("acme-sync"),
                item: cid("env")
            }]
        );
        s.done(
            PluginUiDone::Status {
                plugin: pid("acme-sync"),
                item: cid("env"),
                result: Ok(Some("prod".into())),
            },
            Instant::now(),
        );
        assert_eq!(s.status().shown(Alignment::Right)[0].text, "prod");
        assert!(s.set_status(&pid("acme-sync"), &cid("env"), "dev"));
        assert!(!s.set_status(&pid("acme-sync"), &cid("nope"), "dev"));
    }
}
