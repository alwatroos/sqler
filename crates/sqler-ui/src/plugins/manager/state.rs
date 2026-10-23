// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `PluginsState`: what the plugin manager knows, and the effects it asks for.
//!
//! Headless and synchronous: a call changes the state and returns the [`PluginEffect`]s to carry out. The view runs
//! them on the background executor (they read and write files) and reports each back with
//! [`PluginsState::finished`]. Every effect starts by re-reading the grants, so a decision or a revoke made by
//! another instance is seen on load, when a dialog opens, on every manager command and when the window is activated.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use sqler_plugin_api::{PluginId, SettingSpec, SettingValue};

use crate::plugins::action::PluginCommand;
use crate::plugins::backend::{
    PluginProblem, PluginRow, PluginSnapshot, PluginsBackend, PluginsStartup, RowState,
};
use crate::plugins::grant::{GrantBook, GrantBookError, GrantCommand, GrantPrompt, GrantStatus};

/// The longest settings text accepted, in bytes.
const SETTING_TEXT_MAX: usize = 4096;

/// What the book says about the key of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowGrant {
    /// The user approved this key.
    Approved,
    /// The user denied this key.
    Denied,
    /// Nothing is decided for this key (the plugin is new, or it changed since the last decision).
    NeedsPrompt,
    /// The store is damaged, newer or unreadable.
    Unavailable(String),
}

/// What the book says about `row`. A row whose key cannot be built is treated as unavailable.
#[must_use]
pub fn row_grant(grants: &GrantBook, row: &PluginRow) -> RowGrant {
    let Ok(key) = row.manifest.grant_key(row.component.as_ref()) else {
        return RowGrant::Unavailable("the plugin is not complete".to_owned());
    };
    match grants.status(&key) {
        GrantStatus::Approved(_) => RowGrant::Approved,
        GrantStatus::Denied => RowGrant::Denied,
        GrantStatus::Unavailable { problem } => RowGrant::Unavailable(problem),
        _ => RowGrant::NeedsPrompt,
    }
}

/// The permission dialog on screen.
#[derive(Debug, Clone)]
pub struct GrantDialog {
    /// The plugin the dialog is about.
    pub id: PluginId,
    /// What the dialog shows.
    pub prompt: GrantPrompt,
    /// An answer is being saved: further answers are ignored.
    pub busy: bool,
    /// Counts the dialogs shown, so the view can tell a new dialog from the same one.
    pub generation: u64,
}

/// Work for the background executor.
#[non_exhaustive]
pub enum PluginEffect {
    /// Re-read the grants, then rescan (`refresh`) or list (`snapshot`) the plugins; `review` opens the dialog of that
    /// plugin when the rows are back.
    Load {
        /// Rescan the directories instead of listing what is known.
        refresh: bool,
        /// A plugin whose dialog to open afterwards.
        review: Option<PluginId>,
    },
    /// Switch a plugin on or off and keep the choice.
    SetEnabled {
        /// The plugin.
        id: PluginId,
        /// The new state.
        on: bool,
    },
    /// Store a setting of a plugin.
    SetSetting {
        /// The plugin.
        id: PluginId,
        /// The setting key.
        key: String,
        /// The value.
        value: SettingValue,
    },
    /// Save the answer to the dialog, then tell the host.
    Decide {
        /// What the dialog showed.
        prompt: Box<GrantPrompt>,
        /// The answer.
        command: GrantCommand,
    },
    /// Take back the approval of a plugin, then tell the host.
    Revoke(PluginId),
    /// Start a plugin (its `init`); the only effect that is a future.
    Start(PluginId),
}

// Ids and counts only: never a setting value.
impl std::fmt::Debug for PluginEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load { refresh, review } => f
                .debug_struct("Load")
                .field("refresh", refresh)
                .field("review", &review.as_ref().map(PluginId::as_str))
                .finish(),
            Self::SetEnabled { id, on } => f
                .debug_struct("SetEnabled")
                .field("id", &id.as_str())
                .field("on", on)
                .finish(),
            Self::SetSetting { id, key, .. } => f
                .debug_struct("SetSetting")
                .field("id", &id.as_str())
                .field("key", key)
                .finish_non_exhaustive(),
            Self::Decide { prompt, command } => f
                .debug_struct("Decide")
                .field("id", &prompt.plugin_id())
                .field("command", command)
                .finish(),
            Self::Revoke(id) => f.debug_tuple("Revoke").field(&id.as_str()).finish(),
            Self::Start(id) => f.debug_tuple("Start").field(&id.as_str()).finish(),
        }
    }
}

/// What a [`PluginEffect`] came to.
#[derive(Debug)]
#[non_exhaustive]
pub enum PluginDone {
    /// The rows are back.
    Loaded {
        /// The plugins and the directories that did not load.
        snapshot: PluginSnapshot,
        /// A plugin whose dialog to open.
        review: Option<PluginId>,
    },
    /// A plugin was switched.
    Enabled {
        /// The plugin.
        id: PluginId,
        /// Whether it could be kept.
        result: Result<(), String>,
    },
    /// A setting was stored.
    Setting {
        /// The plugin.
        id: PluginId,
        /// Whether it could be kept.
        result: Result<(), String>,
    },
    /// The dialog was answered.
    Decided {
        /// The plugin.
        id: PluginId,
        /// The answer.
        command: GrantCommand,
        /// Whether it was saved.
        result: Result<(), GrantBookError>,
    },
    /// An approval was taken back.
    Revoked {
        /// The plugin.
        id: PluginId,
        /// `true` when there was an approval to take back.
        result: Result<bool, GrantBookError>,
    },
    /// A plugin was started.
    Started {
        /// The plugin.
        id: PluginId,
        /// The last error when it did not start.
        result: Result<(), String>,
    },
}

impl PluginEffect {
    /// Runs the effect on the calling thread (call it on the background executor). Every effect re-reads the grants
    /// first. [`PluginEffect::Start`] is a future: run it with [`PluginsBackend::start`] instead; here it reports an
    /// internal error.
    #[must_use]
    pub fn run_blocking(self, backend: &dyn PluginsBackend, grants: &GrantBook) -> PluginDone {
        grants.reload();
        match self {
            Self::Load { refresh, review } => {
                let snapshot = if refresh {
                    backend.refresh()
                } else {
                    backend.snapshot()
                };
                PluginDone::Loaded { snapshot, review }
            }
            Self::SetEnabled { id, on } => {
                let result = backend.set_enabled(&id, on);
                PluginDone::Enabled { id, result }
            }
            Self::SetSetting { id, key, value } => {
                let result = backend.set_setting(&id, &key, value);
                PluginDone::Setting { id, result }
            }
            Self::Decide { prompt, command } => {
                let id = prompt.key().id().clone();
                let result = grants.decide(&prompt, command).map(|_| ());
                if result.is_ok() {
                    backend.grants_changed(&id);
                }
                PluginDone::Decided {
                    id,
                    command,
                    result,
                }
            }
            Self::Revoke(id) => {
                let result = grants.revoke(&id);
                if matches!(result, Ok(true)) {
                    backend.grants_changed(&id);
                }
                PluginDone::Revoked { id, result }
            }
            Self::Start(id) => PluginDone::Started {
                id,
                result: Err("internal error: a start is not a blocking effect".to_owned()),
            },
        }
    }
}

/// Where the manager is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Before the first frame: nothing has run.
    Waiting,
    /// The first load is in flight.
    Loading,
    /// The rows are known.
    Ready,
}

/// The plugin manager's state.
pub struct PluginsState {
    backend: Option<Arc<dyn PluginsBackend>>,
    grants: Arc<GrantBook>,
    snapshot: PluginSnapshot,
    phase: Phase,
    /// How many loads are in flight.
    loads: usize,
    selected: Option<PluginId>,
    queue: VecDeque<PluginId>,
    dialog: Option<GrantDialog>,
    dialog_generation: u64,
    /// Plugins started (or being started) since the last change that could make a start worthwhile.
    tried: HashSet<PluginId>,
    /// Grant keys whose dialog is not opened by itself again (the answer could not be saved).
    declined: HashSet<String>,
    /// The last start error per plugin, for a backend that does not report it.
    errors: HashMap<PluginId, String>,
    notice: Option<String>,
    /// Counts the loads that came back; what the plugins contribute is rebuilt when it changes.
    generation: u64,
}

impl std::fmt::Debug for PluginsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginsState")
            .field("available", &self.backend.is_some())
            .field("phase", &self.phase)
            .field("rows", &self.snapshot.rows.len())
            .field("dialog", &self.dialog.as_ref().map(|d| d.id.as_str()))
            .finish_non_exhaustive()
    }
}

impl Default for PluginsState {
    fn default() -> Self {
        Self::new(&PluginsStartup::unavailable())
    }
}

impl PluginsState {
    /// The state for what the binary handed over. Nothing runs yet.
    #[must_use]
    pub fn new(startup: &PluginsStartup) -> Self {
        Self {
            backend: startup.backend.clone(),
            grants: Arc::clone(&startup.grants),
            snapshot: PluginSnapshot::default(),
            phase: Phase::Waiting,
            loads: 0,
            selected: None,
            queue: VecDeque::new(),
            dialog: None,
            dialog_generation: 0,
            tried: HashSet::new(),
            declined: HashSet::new(),
            errors: HashMap::new(),
            notice: None,
            generation: 0,
        }
    }

    /// Counts the loads that came back (0 before the first). A change means the rows may differ.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Lists the plugins again, so a state change the host made (a fault after a failed call) shows up. Nothing
    /// while the manager has not started or a load is already in flight.
    pub fn reload(&mut self) -> Vec<PluginEffect> {
        if self.backend.is_none() || self.phase == Phase::Waiting || self.loads > 0 {
            return Vec::new();
        }
        vec![self.load(false, None)]
    }

    /// Whether this build has a plugin host.
    #[must_use]
    pub fn available(&self) -> bool {
        self.backend.is_some()
    }

    /// The host, if there is one (the view runs the effects against it).
    #[must_use]
    pub fn backend(&self) -> Option<&Arc<dyn PluginsBackend>> {
        self.backend.as_ref()
    }

    /// The book of grants.
    #[must_use]
    pub fn grants(&self) -> &Arc<GrantBook> {
        &self.grants
    }

    /// Whether the first load has not come back yet.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.available() && self.phase != Phase::Ready
    }

    /// The plugins found, in a stable order.
    #[must_use]
    pub fn rows(&self) -> &[PluginRow] {
        &self.snapshot.rows
    }

    /// The directories that did not load.
    #[must_use]
    pub fn problems(&self) -> &[PluginProblem] {
        &self.snapshot.problems
    }

    /// The selected plugin.
    #[must_use]
    pub fn selected(&self) -> Option<&PluginId> {
        self.selected.as_ref()
    }

    /// The row of the selected plugin.
    #[must_use]
    pub fn selected_row(&self) -> Option<&PluginRow> {
        let id = self.selected.as_ref()?;
        self.row(id)
    }

    fn row(&self, id: &PluginId) -> Option<&PluginRow> {
        self.snapshot.rows.iter().find(|r| r.manifest.id() == id)
    }

    /// What the book says about `row`.
    #[must_use]
    pub fn grant_of(&self, row: &PluginRow) -> RowGrant {
        row_grant(&self.grants, row)
    }

    /// The last start error of a row: the host's, else the one this state saw.
    #[must_use]
    pub fn error_of<'a>(&'a self, row: &'a PluginRow) -> Option<&'a str> {
        row.last_error
            .as_deref()
            .or_else(|| self.errors.get(row.manifest.id()).map(String::as_str))
    }

    /// Whether a plugin waits for a decision or has failed: the activity item shows a dot.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.snapshot.rows.iter().any(|r| {
            matches!(r.state, RowState::Faulted { .. } | RowState::LoadFailed)
                || (r.enabled && self.grant_of(r) == RowGrant::NeedsPrompt)
        })
    }

    /// The dialog on screen.
    #[must_use]
    pub fn dialog(&self) -> Option<&GrantDialog> {
        self.dialog.as_ref()
    }

    /// Takes the message for the user, if there is one.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Selects a plugin (a click on its row).
    pub fn select(&mut self, id: &PluginId) {
        if self.row(id).is_some() {
            self.selected = Some(id.clone());
        }
    }

    fn load(&mut self, refresh: bool, review: Option<PluginId>) -> PluginEffect {
        self.loads += 1;
        PluginEffect::Load { refresh, review }
    }

    /// The first frame was drawn: read the grants and discover the plugins. The only starting point; nothing in the
    /// manager runs before it.
    pub fn first_frame(&mut self) -> Vec<PluginEffect> {
        if self.backend.is_none() || self.phase != Phase::Waiting {
            return Vec::new();
        }
        self.phase = Phase::Loading;
        vec![self.load(true, None)]
    }

    /// The window became active: other instances may have changed the grants.
    pub fn activated(&mut self) -> Vec<PluginEffect> {
        if self.backend.is_none() || self.phase == Phase::Waiting || self.loads > 0 {
            return Vec::new();
        }
        vec![self.load(false, None)]
    }

    /// Runs a manager command.
    pub fn command(&mut self, command: PluginCommand) -> Vec<PluginEffect> {
        if self.backend.is_none() {
            self.notice = Some("This build has no plugin support.".to_owned());
            return Vec::new();
        }
        if self.phase == Phase::Waiting {
            return Vec::new();
        }
        if command == PluginCommand::Refresh {
            self.tried.clear();
            self.declined.clear();
            self.errors.clear();
            return vec![self.load(true, None)];
        }
        let Some(row) = self.selected_row() else {
            self.notice = Some("Select a plugin first.".to_owned());
            return Vec::new();
        };
        let id = row.manifest.id().clone();
        let name = row.manifest.name().to_owned();
        match command {
            PluginCommand::Enable | PluginCommand::Disable => vec![PluginEffect::SetEnabled {
                id,
                on: command == PluginCommand::Enable,
            }],
            PluginCommand::ReviewPermissions => {
                if self.dialog.is_some() {
                    self.notice = Some("Answer the open permission dialog first.".to_owned());
                    return Vec::new();
                }
                vec![self.load(false, Some(id))]
            }
            PluginCommand::Revoke => {
                if self.grant_of_id(&id) != Some(RowGrant::Approved) {
                    self.notice = Some(format!("{name} has no approval to take back."));
                    return Vec::new();
                }
                vec![PluginEffect::Revoke(id)]
            }
            _ => Vec::new(),
        }
    }

    fn grant_of_id(&self, id: &PluginId) -> Option<RowGrant> {
        self.row(id).map(|r| self.grant_of(r))
    }

    /// The user answered the dialog (a button, Escape or the typed id). An answer that is being saved, or that
    /// Approve cannot give, is ignored.
    pub fn answer(&mut self, command: GrantCommand) -> Vec<PluginEffect> {
        let Some(dialog) = self.dialog.as_mut() else {
            return Vec::new();
        };
        if dialog.busy || (command == GrantCommand::Approve && !dialog.prompt.can_approve()) {
            return Vec::new();
        }
        dialog.busy = true;
        vec![PluginEffect::Decide {
            prompt: Box::new(dialog.prompt.clone()),
            command,
        }]
    }

    /// Stores a setting typed as text; a value of the wrong type is refused with a notice.
    pub fn set_setting(&mut self, id: &PluginId, key: &str, text: &str) -> Vec<PluginEffect> {
        let Some(spec) = self.spec(id, key) else {
            return Vec::new();
        };
        match parse_setting(&spec.default, text) {
            Ok(value) => vec![PluginEffect::SetSetting {
                id: id.clone(),
                key: key.to_owned(),
                value,
            }],
            Err(why) => {
                self.notice = Some(format!("{}: {why}", spec.label));
                Vec::new()
            }
        }
    }

    /// Flips a boolean setting.
    pub fn toggle_setting(&mut self, id: &PluginId, key: &str) -> Vec<PluginEffect> {
        let Some(row) = self.row(id) else {
            return Vec::new();
        };
        let current = row
            .settings
            .iter()
            .find(|(spec, _)| spec.key.as_str() == key)
            .map(|(_, v)| v.clone());
        match current {
            Some(SettingValue::Boolean(on)) => vec![PluginEffect::SetSetting {
                id: id.clone(),
                key: key.to_owned(),
                value: SettingValue::Boolean(!on),
            }],
            _ => Vec::new(),
        }
    }

    fn spec(&self, id: &PluginId, key: &str) -> Option<SettingSpec> {
        self.row(id)?
            .settings
            .iter()
            .find(|(spec, _)| spec.key.as_str() == key)
            .map(|(spec, _)| spec.clone())
    }

    /// Takes in what a finished effect came to; returns the follow-up work.
    pub fn finished(&mut self, done: PluginDone) -> Vec<PluginEffect> {
        match done {
            PluginDone::Loaded { snapshot, review } => self.loaded(snapshot, review),
            PluginDone::Enabled { id, result } => {
                self.tried.remove(&id);
                self.errors.remove(&id);
                if let Err(why) = result {
                    self.notice = Some(format!("Could not change the plugin: {why}"));
                }
                vec![self.load(false, None)]
            }
            PluginDone::Setting { result, .. } => {
                if let Err(why) = result {
                    self.notice = Some(format!("Could not keep the setting: {why}"));
                }
                vec![self.load(false, None)]
            }
            PluginDone::Decided { id, result, .. } => self.decided(&id, result),
            PluginDone::Revoked { id, result } => {
                self.tried.remove(&id);
                self.notice = Some(match result {
                    Ok(true) => format!("Permissions of {} taken back.", self.name_of(&id)),
                    Ok(false) => format!("{} had no approval to take back.", self.name_of(&id)),
                    Err(why) => why.to_string(),
                });
                vec![self.load(false, None)]
            }
            PluginDone::Started { id, result } => {
                if let Err(why) = result {
                    self.errors.insert(id, why);
                }
                vec![self.load(false, None)]
            }
        }
    }

    fn name_of(&self, id: &PluginId) -> String {
        self.row(id)
            .map_or_else(|| id.to_string(), |r| r.manifest.name().to_owned())
    }

    fn loaded(&mut self, snapshot: PluginSnapshot, review: Option<PluginId>) -> Vec<PluginEffect> {
        self.loads = self.loads.saturating_sub(1);
        self.phase = Phase::Ready;
        self.generation += 1;
        self.snapshot = snapshot;
        let rows = &self.snapshot.rows;
        self.errors
            .retain(|id, _| rows.iter().any(|r| r.manifest.id() == id));
        if self
            .selected
            .as_ref()
            .is_none_or(|id| self.row(id).is_none())
        {
            self.selected = self.snapshot.rows.first().map(|r| r.manifest.id().clone());
        }
        if let Some(id) = review {
            self.open_dialog(&id, true);
        }
        self.enqueue_prompts();
        self.next_dialog();
        self.starts()
    }

    /// Rows that are on and wait for a decision queue a dialog each; one is shown at a time.
    fn enqueue_prompts(&mut self) {
        let waiting: Vec<PluginId> = self
            .snapshot
            .rows
            .iter()
            .filter(|r| r.enabled && self.grant_of(r) == RowGrant::NeedsPrompt)
            .filter(|r| !self.declined.contains(&row_key_text(r)))
            .map(|r| r.manifest.id().clone())
            .collect();
        self.queue
            .retain(|id| waiting.contains(id) && self.dialog.as_ref().is_none_or(|d| d.id != *id));
        for id in waiting {
            if !self.queue.contains(&id) && self.dialog.as_ref().is_none_or(|d| d.id != id) {
                self.queue.push_back(id);
            }
        }
    }

    fn next_dialog(&mut self) {
        while self.dialog.is_none() {
            let Some(id) = self.queue.pop_front() else {
                return;
            };
            self.open_dialog(&id, false);
        }
    }

    /// Shows the dialog of `id` built from what the book says now. `forced` shows it even when the key is decided
    /// (a review); otherwise a key that was decided meanwhile is skipped.
    fn open_dialog(&mut self, id: &PluginId, forced: bool) {
        let Some(row) = self.row(id) else {
            return;
        };
        let Ok(prompt) = self.grants.prompt(&row.manifest, row.component.as_ref()) else {
            return;
        };
        if !forced && !matches!(prompt.basis(), GrantStatus::NeedsPrompt { .. }) {
            return;
        }
        self.dialog_generation += 1;
        self.dialog = Some(GrantDialog {
            id: id.clone(),
            prompt,
            busy: false,
            generation: self.dialog_generation,
        });
    }

    fn decided(&mut self, id: &PluginId, result: Result<(), GrantBookError>) -> Vec<PluginEffect> {
        match result {
            Ok(()) => {
                self.dialog = None;
                self.tried.remove(id);
                self.errors.remove(id);
                vec![self.load(false, None)]
            }
            Err(GrantBookError::StaleDecision) => {
                // The record changed while the dialog was open: show what is on disk now.
                self.dialog = None;
                self.open_dialog(id, true);
                self.notice = Some(format!(
                    "The permissions of {} changed while the dialog was open. Review them again.",
                    self.name_of(id)
                ));
                if self.dialog.is_none() {
                    return vec![self.load(false, None)];
                }
                Vec::new()
            }
            Err(why) => {
                if let Some(dialog) = self.dialog.take() {
                    self.declined.insert(dialog.prompt.key().to_string());
                }
                self.notice = Some(why.to_string());
                vec![self.load(false, None)]
            }
        }
    }

    /// Plugins that are on, approved and not started yet start once.
    fn starts(&mut self) -> Vec<PluginEffect> {
        let ready: Vec<PluginId> = self
            .snapshot
            .rows
            .iter()
            .filter(|r| r.enabled && r.state == RowState::Idle)
            .filter(|r| self.grant_of(r) == RowGrant::Approved)
            .map(|r| r.manifest.id().clone())
            .filter(|id| !self.tried.contains(id))
            .collect();
        ready
            .into_iter()
            .map(|id| {
                self.tried.insert(id.clone());
                PluginEffect::Start(id)
            })
            .collect()
    }
}

/// The text of the grant key of a row (empty when it cannot be built).
fn row_key_text(row: &PluginRow) -> String {
    row.manifest
        .grant_key(row.component.as_ref())
        .map(|k| k.to_string())
        .unwrap_or_default()
}

/// Parses the text of a setting editor into a value of the type of `default`.
///
/// # Errors
///
/// A one-line reason when the text is not a value of that type.
pub fn parse_setting(default: &SettingValue, text: &str) -> Result<SettingValue, String> {
    match default {
        SettingValue::Boolean(_) => match text.trim() {
            "true" => Ok(SettingValue::Boolean(true)),
            "false" => Ok(SettingValue::Boolean(false)),
            _ => Err("enter true or false".to_owned()),
        },
        SettingValue::Integer(_) => text
            .trim()
            .parse::<i64>()
            .map(SettingValue::Integer)
            .map_err(|_| "enter a whole number".to_owned()),
        SettingValue::Float(_) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(SettingValue::Float)
            .ok_or_else(|| "enter a number".to_owned()),
        SettingValue::Text(_) => {
            if text.len() > SETTING_TEXT_MAX {
                Err(format!("the text is longer than {SETTING_TEXT_MAX} bytes"))
            } else {
                Ok(SettingValue::Text(text.to_owned()))
            }
        }
        _ => Err("this kind of setting cannot be edited here".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::plugins::backend::FakeBackend;
    use crate::plugins::manager::fixture::{manifest, row, row_with};

    struct Rig {
        fake: Arc<FakeBackend>,
        grants: Arc<GrantBook>,
        state: PluginsState,
    }

    fn rig(rows: Vec<PluginRow>) -> Rig {
        let fake = FakeBackend::new(PluginSnapshot {
            rows,
            problems: Vec::new(),
        });
        let grants = Arc::new(GrantBook::in_memory());
        let startup = PluginsStartup::new(Some(fake.clone()), Arc::clone(&grants));
        Rig {
            fake,
            grants,
            state: PluginsState::new(&startup),
        }
    }

    impl Rig {
        /// Runs effects the way the view does, feeding every result back until nothing is left.
        fn pump(&mut self, mut effects: Vec<PluginEffect>) {
            let backend = self.fake.clone();
            while let Some(effect) = effects.pop() {
                let done = match effect {
                    PluginEffect::Start(id) => {
                        let result = futures_lite_block(backend.start(&id));
                        PluginDone::Started { id, result }
                    }
                    other => other.run_blocking(&*backend, &self.grants),
                };
                effects.extend(self.state.finished(done));
            }
        }

        fn load(&mut self) {
            let effects = self.state.first_frame();
            self.pump(effects);
        }
    }

    /// The fake's futures are ready at once.
    fn futures_lite_block<T>(fut: sqler_driver_api::BoxFuture<'static, T>) -> T {
        use std::task::{Context, Poll, Waker};
        let mut fut = fut;
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("the fake future is not ready"),
        }
    }

    fn approve(rig: &Rig, row: &PluginRow) {
        let prompt = rig
            .grants
            .prompt(&row.manifest, row.component.as_ref())
            .unwrap();
        rig.grants.decide(&prompt, GrantCommand::Approve).unwrap();
    }

    #[test]
    fn nothing_runs_before_the_first_frame() {
        let rig = rig(vec![row(manifest("acme-sync", &["db.read"]))]);
        assert_eq!(rig.fake.calls.refresh.load(Ordering::SeqCst), 0);
        assert_eq!(rig.fake.calls.snapshot.load(Ordering::SeqCst), 0);
        assert!(rig.state.loading());
        assert!(rig.state.rows().is_empty());
        assert!(rig.state.dialog().is_none());
    }

    #[test]
    fn reload_lists_the_plugins_again_once_and_counts_the_loads() {
        let mut rig = rig(vec![row(manifest("acme-sync", &["db.read"]))]);
        assert!(rig.state.reload().is_empty(), "not before the first frame");
        assert_eq!(rig.state.generation(), 0);
        let effects = rig.state.first_frame();
        assert!(rig.state.reload().is_empty(), "a load is in flight");
        rig.pump(effects);
        assert_eq!(rig.state.generation(), 1);
        let reload = rig.state.reload();
        assert_eq!(reload.len(), 1);
        assert!(matches!(
            reload[0],
            PluginEffect::Load {
                refresh: false,
                review: None
            }
        ));
        assert!(rig.state.reload().is_empty(), "only one at a time");
        rig.pump(reload);
        assert_eq!(rig.state.generation(), 2);
    }

    #[test]
    fn the_first_frame_refreshes_once() {
        let mut rig = rig(vec![row(manifest("acme-sync", &["db.read"]))]);
        let effects = rig.state.first_frame();
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            PluginEffect::Load {
                refresh: true,
                review: None
            }
        ));
        assert!(rig.state.first_frame().is_empty(), "only once");
        rig.pump(effects);
        assert_eq!(rig.fake.calls.refresh.load(Ordering::SeqCst), 1);
        assert!(!rig.state.loading());
        assert_eq!(rig.state.rows().len(), 1);
        assert_eq!(rig.state.selected().unwrap().as_str(), "acme-sync");
    }

    #[test]
    fn a_build_without_a_host_does_nothing_and_says_so() {
        let mut state = PluginsState::default();
        assert!(!state.available());
        assert!(!state.loading());
        assert!(state.first_frame().is_empty());
        assert!(state.activated().is_empty());
        assert!(state.command(PluginCommand::Refresh).is_empty());
        assert_eq!(
            state.take_notice().as_deref(),
            Some("This build has no plugin support.")
        );
    }

    #[test]
    fn rows_that_need_a_decision_queue_one_dialog_at_a_time() {
        let mut rig = rig(vec![
            row(manifest("acme-sync", &["db.read"])),
            row(manifest("beta-tool", &["db.read", "db.write"])),
        ]);
        rig.load();
        let dialog = rig.state.dialog().unwrap();
        assert_eq!(dialog.id.as_str(), "acme-sync");
        assert!(rig.state.needs_attention());
        // Answering the first opens the second.
        let effects = rig.state.answer(GrantCommand::Deny);
        assert!(rig.state.dialog().unwrap().busy);
        assert!(
            rig.state.answer(GrantCommand::Deny).is_empty(),
            "an answer in flight is ignored"
        );
        rig.pump(effects);
        assert_eq!(rig.state.dialog().unwrap().id.as_str(), "beta-tool");
        let effects = rig.state.answer(GrantCommand::Approve);
        rig.pump(effects);
        assert!(rig.state.dialog().is_none());
        assert_eq!(rig.fake.changed.lock().unwrap().len(), 2);
        // Denied stays denied, approved starts.
        assert_eq!(
            rig.state.grant_of(&rig.state.rows()[0].clone()),
            RowGrant::Denied
        );
        assert_eq!(rig.fake.started.lock().unwrap().as_slice(), ["beta-tool"]);
        assert!(!rig.state.needs_attention());
    }

    #[test]
    fn a_disabled_row_is_neither_asked_nor_started() {
        let mut off = row(manifest("acme-sync", &["db.read"]));
        off.enabled = false;
        off.state = RowState::Off;
        let approved = row(manifest("beta-tool", &["db.read"]));
        let mut disabled_approved = row_with(manifest("gamma-tool", &["db.read"]), |r| {
            r.enabled = false;
            r.state = RowState::Off;
        });
        disabled_approved.enabled = false;
        let mut rig = rig(vec![off, approved.clone(), disabled_approved.clone()]);
        approve(&rig, &approved);
        approve(&rig, &disabled_approved);
        rig.load();
        assert!(rig.state.dialog().is_none());
        assert_eq!(rig.fake.started.lock().unwrap().as_slice(), ["beta-tool"]);
        assert!(!rig.state.needs_attention());
    }

    #[test]
    fn a_stale_decision_rebuilds_and_reshows_the_dialog() {
        let r = row(manifest("acme-sync", &["db.read"]));
        let mut rig = rig(vec![r.clone()]);
        rig.load();
        assert!(rig.state.dialog().is_some());
        // Another instance denied it meanwhile: the basis of the dialog is stale.
        let other = rig.grants.prompt(&r.manifest, None).unwrap();
        rig.grants.decide(&other, GrantCommand::Deny).unwrap();
        let effects = rig.state.answer(GrantCommand::Approve);
        let before = rig.state.dialog().unwrap().generation;
        rig.pump(effects);
        let dialog = rig.state.dialog().expect("the dialog is shown again");
        assert!(dialog.generation > before);
        assert!(!dialog.busy);
        assert!(!dialog.prompt.is_reprompt());
        assert!(
            rig.state
                .take_notice()
                .unwrap()
                .contains("changed while the dialog was open")
        );
        // Nothing was approved by the stale answer.
        assert_eq!(rig.fake.started.lock().unwrap().len(), 0);
    }

    #[test]
    fn a_failed_save_closes_the_dialog_and_does_not_reopen_it_by_itself() {
        let r = row(manifest("acme-sync", &["db.read"]));
        let mut rig = rig(vec![r]);
        rig.load();
        let locked = rig
            .state
            .dialog()
            .unwrap()
            .prompt
            .clone()
            .with_problem("damaged");
        rig.state.dialog.as_mut().unwrap().prompt = locked;
        assert!(rig.state.answer(GrantCommand::Approve).is_empty());
        let effects = rig.state.answer(GrantCommand::Deny);
        rig.pump(effects);
        assert!(rig.state.dialog().is_none());
        assert!(rig.state.take_notice().is_some());
        // A later snapshot does not ask again; Review does.
        let effects = rig.state.activated();
        rig.pump(effects);
        assert!(rig.state.dialog().is_none());
    }

    #[test]
    fn review_permissions_opens_the_dialog_of_a_decided_row() {
        let r = row(manifest("acme-sync", &["db.read"]));
        let mut rig = rig(vec![r.clone()]);
        approve(&rig, &r);
        rig.load();
        assert!(rig.state.dialog().is_none());
        let effects = rig.state.command(PluginCommand::ReviewPermissions);
        rig.pump(effects);
        assert!(rig.state.dialog().is_some());
        // A second review while a dialog is open is refused.
        assert!(
            rig.state
                .command(PluginCommand::ReviewPermissions)
                .is_empty()
        );
        assert!(rig.state.take_notice().is_some());
    }

    #[test]
    fn revoke_calls_grants_changed_and_the_row_asks_again() {
        let r = row(manifest("acme-sync", &["db.read"]));
        let mut rig = rig(vec![r.clone()]);
        approve(&rig, &r);
        rig.load();
        assert_eq!(rig.fake.started.lock().unwrap().len(), 1);
        let effects = rig.state.command(PluginCommand::Revoke);
        assert!(matches!(effects[0], PluginEffect::Revoke(_)));
        rig.pump(effects);
        assert_eq!(rig.fake.changed.lock().unwrap().as_slice(), ["acme-sync"]);
        assert_eq!(rig.fake.calls.grants_changed.load(Ordering::SeqCst), 1);
        // A revoke turns the approval into a denial: nothing to take back a second time.
        assert!(rig.state.command(PluginCommand::Revoke).is_empty());
        assert!(rig.state.take_notice().unwrap().contains("no approval"));
    }

    #[test]
    fn commands_need_a_selection_and_a_started_manager() {
        let mut rig = rig(Vec::new());
        assert!(
            rig.state.command(PluginCommand::Enable).is_empty(),
            "before the first frame"
        );
        rig.load();
        assert!(rig.state.command(PluginCommand::Enable).is_empty());
        assert_eq!(
            rig.state.take_notice().as_deref(),
            Some("Select a plugin first.")
        );
    }

    #[test]
    fn enable_and_disable_go_through_the_backend() {
        let r = row(manifest("acme-sync", &["db.read"]));
        let mut rig = rig(vec![r.clone()]);
        approve(&rig, &r);
        rig.load();
        let effects = rig.state.command(PluginCommand::Disable);
        rig.pump(effects);
        assert_eq!(rig.fake.calls.set_enabled.load(Ordering::SeqCst), 1);
        assert!(!rig.state.rows()[0].enabled);
        let started = rig.fake.started.lock().unwrap().len();
        let effects = rig.state.command(PluginCommand::Enable);
        rig.pump(effects);
        assert!(rig.state.rows()[0].enabled);
        assert_eq!(
            rig.fake.started.lock().unwrap().len(),
            started + 1,
            "enabled again, started again"
        );
    }

    #[test]
    fn a_start_runs_once_and_its_error_is_kept() {
        let r = row(manifest("acme-sync", &["db.read"]));
        let mut rig = rig(vec![r.clone()]);
        approve(&rig, &r);
        *rig.fake.start_error.lock().unwrap() = Some("could not load".to_owned());
        rig.load();
        assert_eq!(rig.fake.started.lock().unwrap().len(), 1);
        let row = rig.state.rows()[0].clone();
        assert_eq!(rig.state.error_of(&row), Some("could not load"));
        // Another snapshot does not start it again.
        let effects = rig.state.activated();
        rig.pump(effects);
        assert_eq!(rig.fake.started.lock().unwrap().len(), 1);
        // A rescan clears the memory and tries again.
        let effects = rig.state.command(PluginCommand::Refresh);
        rig.pump(effects);
        assert_eq!(rig.fake.started.lock().unwrap().len(), 2);
    }

    #[test]
    fn activation_re_reads_through_a_snapshot_only_when_idle() {
        let mut rig = rig(vec![row(manifest("acme-sync", &["db.read"]))]);
        assert!(
            rig.state.activated().is_empty(),
            "not before the first frame"
        );
        let first = rig.state.first_frame();
        assert!(rig.state.activated().is_empty(), "a load is in flight");
        rig.pump(first);
        let again = rig.state.activated();
        assert!(matches!(
            again[0],
            PluginEffect::Load {
                refresh: false,
                review: None
            }
        ));
    }

    #[test]
    fn every_effect_re_reads_the_grants() {
        use sqler_config::ConfigDir;
        let dir = std::env::temp_dir().join(format!("sqler-ui-{}-mgr", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = ConfigDir::new(&dir);
        let r = row(manifest("acme-sync", &["db.read"]));
        let ours = Arc::new(GrantBook::open(cfg.clone()));
        let theirs = GrantBook::open(cfg);
        let prompt = theirs.prompt(&r.manifest, None).unwrap();
        let fake = FakeBackend::new(PluginSnapshot {
            rows: vec![r.clone()],
            problems: Vec::new(),
        });
        let startup = PluginsStartup::new(Some(fake.clone()), Arc::clone(&ours));
        let mut state = PluginsState::new(&startup);
        assert_eq!(state.grant_of(&r), RowGrant::NeedsPrompt);
        // Another instance approves; our book does not know until an effect runs.
        theirs.decide(&prompt, GrantCommand::Approve).unwrap();
        assert_eq!(state.grant_of(&r), RowGrant::NeedsPrompt);
        let effect = state.command(PluginCommand::Refresh);
        assert!(effect.is_empty(), "before the first frame nothing runs");
        let first = state.first_frame();
        let done = first
            .into_iter()
            .next()
            .unwrap()
            .run_blocking(&*fake, &ours);
        assert_eq!(state.grant_of(&r), RowGrant::Approved);
        let follow = state.finished(done);
        assert!(state.dialog().is_none(), "approved elsewhere: no dialog");
        assert!(matches!(follow[0], PluginEffect::Start(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_parse_by_type_and_a_bad_value_is_a_notice() {
        assert_eq!(
            parse_setting(&SettingValue::Integer(1), " 42 "),
            Ok(SettingValue::Integer(42))
        );
        assert!(parse_setting(&SettingValue::Integer(1), "4.2").is_err());
        assert_eq!(
            parse_setting(&SettingValue::Float(1.0), "2.5"),
            Ok(SettingValue::Float(2.5))
        );
        assert!(parse_setting(&SettingValue::Float(1.0), "NaN").is_err());
        assert!(parse_setting(&SettingValue::Float(1.0), "inf").is_err());
        assert_eq!(
            parse_setting(&SettingValue::Boolean(false), "true"),
            Ok(SettingValue::Boolean(true))
        );
        assert!(parse_setting(&SettingValue::Boolean(false), "yes").is_err());
        assert_eq!(
            parse_setting(&SettingValue::Text(String::new()), "x y"),
            Ok(SettingValue::Text("x y".to_owned()))
        );
        assert!(
            parse_setting(
                &SettingValue::Text(String::new()),
                &"x".repeat(SETTING_TEXT_MAX + 1)
            )
            .is_err()
        );
    }

    #[test]
    fn settings_go_to_the_backend_typed() {
        use crate::plugins::manager::fixture::with_settings;
        let r = with_settings(row(manifest("acme-sync", &["db.read"])));
        let id = r.manifest.id().clone();
        let mut rig = rig(vec![r.clone()]);
        approve(&rig, &r);
        rig.load();
        assert!(rig.state.set_setting(&id, "batch", "abc").is_empty());
        assert!(rig.state.take_notice().unwrap().contains("whole number"));
        let effects = rig.state.set_setting(&id, "batch", "250");
        rig.pump(effects);
        let effects = rig.state.toggle_setting(&id, "verbose");
        rig.pump(effects);
        assert_eq!(rig.fake.calls.set_setting.load(Ordering::SeqCst), 2);
        let settings = &rig.state.rows()[0].settings;
        assert_eq!(settings[0].1, SettingValue::Integer(250));
        assert_eq!(settings[1].1, SettingValue::Boolean(true));
        assert!(rig.state.set_setting(&id, "missing", "1").is_empty());
    }

    #[test]
    fn effects_do_not_leak_setting_values_in_debug() {
        let id: PluginId = "acme-sync".parse().unwrap();
        let effect = PluginEffect::SetSetting {
            id,
            key: "token".into(),
            value: SettingValue::Text("hunter2".into()),
        };
        let text = format!("{effect:?}");
        assert!(text.contains("token") && !text.contains("hunter2"));
    }

    #[test]
    fn a_faulted_or_failed_row_needs_attention() {
        let ok = row(manifest("acme-sync", &["db.read"]));
        let faulted = row_with(manifest("beta-tool", &["db.read"]), |r| {
            r.state = RowState::Faulted { count: 3 };
        });
        let mut rig = rig(vec![ok.clone(), faulted]);
        approve(&rig, &ok);
        rig.load();
        assert!(rig.state.needs_attention());
    }

    #[test]
    fn the_approve_id_is_rejected_as_an_override() {
        use crate::command::CommandRegistry;
        use crate::keymap::KeyDispatcher;
        use sqler_config::{CommandId as ConfigCommandId, KeyChord, KeybindingOverrides, Platform};
        let mut registry = CommandRegistry::new();
        crate::plugins::action::register_commands(&mut registry).unwrap();
        let mut ovr = KeybindingOverrides::new();
        ovr.insert(
            ConfigCommandId::new("plugins:approve-grant").unwrap(),
            vec![KeyChord::parse("ctrl+shift+y", Platform::current()).unwrap()],
        );
        let keys = KeyDispatcher::new(Platform::current(), Ok(ovr), &registry);
        assert!(
            keys.notices()
                .iter()
                .any(|n| n.contains("plugins:approve-grant")),
            "{:?}",
            keys.notices()
        );
        let approve = crate::command::CommandId::new("plugins:approve-grant").unwrap();
        assert!(keys.shortcut_display(&approve).is_none());
    }
}
