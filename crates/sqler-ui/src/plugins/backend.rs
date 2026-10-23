// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What the UI sees of the plugin host: a trait and plain data, so `sqler-ui` never links the WebAssembly runtime
//! (the `Connector` and `AiProviderFactory` precedent, ADR-0016 and ADR-0050). The binary builds a backend when the
//! `plugins` feature is on; without it [`PluginsStartup::unavailable`] says so.

use std::sync::Arc;

use std::sync::{Mutex, PoisonError};

use sqler_driver_api::BoxFuture;
use sqler_plugin_api::{
    CapabilitySet, CommandContext, CompletionItem, CompletionRequest, ComponentDigest,
    ContributionId, Manifest, PluginId, SettingSpec, SettingValue, WidgetEvent, WidgetTree,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::approval::WriteInbox;
use super::grant::GrantBook;

/// The reason the default contribution methods give.
pub const NO_UI: &str = "This build cannot run plugin contributions.";

/// The result of drawing a panel: the tree, or a one-line reason (a bad tree or a failed call).
pub type PanelRender = Result<WidgetTree, String>;

/// Something a plugin pushed to the window.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginUiEvent {
    /// The plugin asks for a panel to be drawn again.
    RefreshPanel {
        /// The plugin.
        plugin: PluginId,
        /// The panel.
        panel: ContributionId,
    },
    /// The plugin sets the text of a status item.
    SetStatus {
        /// The plugin.
        plugin: PluginId,
        /// The status item.
        item: ContributionId,
        /// The text (already sanitised by the host).
        text: String,
    },
    /// The plugin shows a message.
    Notify {
        /// The plugin.
        plugin: PluginId,
        /// The message (already sanitised by the host).
        message: String,
    },
}

/// The sending side of the plugin UI events (the app's sink holds it).
pub type UiSender = UnboundedSender<PluginUiEvent>;

/// The receiving side, handed to the workspace through `PluginsStartup`. The workspace takes it once.
#[derive(Clone, Default)]
pub struct UiInbox(Arc<Mutex<Option<UnboundedReceiver<PluginUiEvent>>>>);

impl UiInbox {
    /// Takes the receiver. `None` when it was taken already or there never was one.
    #[must_use]
    pub fn take(&self) -> Option<UnboundedReceiver<PluginUiEvent>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    /// Whether a receiver waits to be taken.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
}

impl std::fmt::Debug for UiInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("UiInbox").field(&self.is_set()).finish()
    }
}

/// A new channel: the sender for the app's sink and the inbox for [`PluginsStartup::with_ui_events`].
#[must_use]
pub fn ui_channel() -> (UiSender, UiInbox) {
    let (tx, rx) = unbounded_channel();
    (tx, UiInbox(Arc::new(Mutex::new(Some(rx)))))
}

/// The plugin host as the UI uses it.
///
/// Every method is blocking (file and lock access): call it on the background executor, never on the UI thread.
pub trait PluginsBackend: Send + Sync {
    /// Reloads the grants, discovers the plugins again, registers them and applies the stored preferences.
    fn refresh(&self) -> PluginSnapshot;

    /// Reloads the grants (and refreshes the host's view of the plugins whose grants changed) and returns the rows.
    fn snapshot(&self) -> PluginSnapshot;

    /// Switches a plugin on or off and persists the choice.
    ///
    /// # Errors
    ///
    /// A one-line reason when the choice cannot be kept.
    fn set_enabled(&self, id: &PluginId, on: bool) -> Result<(), String>;

    /// Tells the host the grants of `id` changed (after a decision or a revoke): reload and refresh the plugin.
    fn grants_changed(&self, id: &PluginId);

    /// Stores a setting value of a plugin.
    ///
    /// # Errors
    ///
    /// A one-line reason when the value is refused or cannot be kept.
    fn set_setting(&self, id: &PluginId, key: &str, value: SettingValue) -> Result<(), String>;

    /// Starts a plugin (its `init`). `Err` carries the last error shown in the manager.
    fn start(&self, id: &PluginId) -> BoxFuture<'static, Result<(), String>>;

    /// Runs a command a plugin declares. Never blocks: the work is a future.
    fn run_command(
        &self,
        id: &PluginId,
        command: &ContributionId,
        context: CommandContext,
    ) -> BoxFuture<'static, Result<(), String>> {
        let _ = (id, command, context);
        Box::pin(async { Err(NO_UI.to_owned()) })
    }

    /// Asks a plugin panel for its widget tree. `Err` is a one-line reason.
    fn render_panel(
        &self,
        id: &PluginId,
        panel: &ContributionId,
    ) -> BoxFuture<'static, PanelRender> {
        let _ = (id, panel);
        Box::pin(async { Err(NO_UI.to_owned()) })
    }

    /// Sends a widget event to a plugin panel; the plugin may answer with a replacement tree.
    fn panel_event(
        &self,
        id: &PluginId,
        panel: &ContributionId,
        event: WidgetEvent,
    ) -> BoxFuture<'static, Result<Option<WidgetTree>, String>> {
        let _ = (id, panel, event);
        Box::pin(async { Err(NO_UI.to_owned()) })
    }

    /// Asks a plugin for the text of a status item; `None` hides it.
    fn status(
        &self,
        id: &PluginId,
        item: &ContributionId,
    ) -> BoxFuture<'static, Result<Option<String>, String>> {
        let _ = (id, item);
        Box::pin(async { Err(NO_UI.to_owned()) })
    }

    /// Asks the completion plugins for items: one future per eligible, free plugin, and for each busy
    /// one a future that resolves when its call in flight ends. `only` limits the ask to those plugins.
    ///
    /// May read the grants file: call it off the UI thread.
    fn complete(&self, request: &CompletionRequest, only: Option<&[PluginId]>) -> CompletionCalls {
        let _ = (request, only);
        CompletionCalls::default()
    }
}

/// The calls one completion ask started.
#[derive(Default)]
pub struct CompletionCalls {
    /// One future per plugin that was free.
    pub calls: Vec<BoxFuture<'static, Result<PluginCompletions, String>>>,
    /// For each plugin that was busy with an earlier call: a future that resolves when that call ends,
    /// so the caller can ask it again.
    pub busy: Vec<(PluginId, BoxFuture<'static, ()>)>,
}

/// The completion items one plugin answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCompletions {
    /// The plugin.
    pub plugin: PluginId,
    /// The plugin's display name: one line, at most 64 bytes.
    pub source: String,
    /// The cleaned, capped items.
    pub items: Vec<CompletionItem>,
}

/// What the binary hands over about plugins at startup.
///
/// Only built through [`PluginsStartup::new`] and [`PluginsStartup::unavailable`] (never as a literal), so a field can
/// be added without touching the callers.
#[derive(Clone)]
#[non_exhaustive]
pub struct PluginsStartup {
    /// The host, or `None` when this build has no plugin support.
    pub backend: Option<Arc<dyn PluginsBackend>>,
    /// The book of grants; the dialog decides here and the host reads it.
    pub grants: Arc<GrantBook>,
    /// Where the plugins' write approvals arrive (the app's approver holds the sender). Empty by default.
    pub writes: WriteInbox,
    /// The tools plugins offer to the AI surfaces, or `None` (the default). They ride on the AI audit sink.
    pub ai_tools: Option<Arc<dyn sqler_ai_tools::ExternalTools>>,
    /// Where the plugins' panel refreshes, status texts and messages arrive. Empty by default.
    pub ui_events: UiInbox,
}

impl std::fmt::Debug for PluginsStartup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginsStartup")
            .field("backend", &self.backend.is_some())
            .field("writes", &self.writes.is_set())
            .field("ai_tools", &self.ai_tools.is_some())
            .field("ui_events", &self.ui_events.is_set())
            .finish_non_exhaustive()
    }
}

impl PluginsStartup {
    /// A startup with a host and the book it reads.
    #[must_use]
    pub fn new(backend: Option<Arc<dyn PluginsBackend>>, grants: Arc<GrantBook>) -> Self {
        Self {
            backend,
            grants,
            writes: WriteInbox::default(),
            ai_tools: None,
            ui_events: UiInbox::default(),
        }
    }

    /// Sets the tools plugins offer to the AI surfaces.
    #[must_use]
    pub fn with_ai_tools(mut self, tools: Arc<dyn sqler_ai_tools::ExternalTools>) -> Self {
        self.ai_tools = Some(tools);
        self
    }

    /// Sets the inbox of plugin UI events.
    #[must_use]
    pub fn with_ui_events(mut self, ui_events: UiInbox) -> Self {
        self.ui_events = ui_events;
        self
    }

    /// Sets the inbox of plugin write approvals.
    #[must_use]
    pub fn with_writes(mut self, writes: WriteInbox) -> Self {
        self.writes = writes;
        self
    }

    /// A build without plugin support: no host and an empty in-memory book.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(None, Arc::new(GrantBook::in_memory()))
    }
}

/// The plugins found and the directories that could not be read.
#[derive(Debug, Clone, Default)]
pub struct PluginSnapshot {
    /// One row per discovered plugin, in a stable order.
    pub rows: Vec<PluginRow>,
    /// Directories that did not load.
    pub problems: Vec<PluginProblem>,
}

/// One plugin as the manager shows it.
#[derive(Debug, Clone)]
pub struct PluginRow {
    /// The validated manifest.
    pub manifest: Arc<Manifest>,
    /// The digest of the component (`None` for a declarative plugin or one that failed to load).
    pub component: Option<ComponentDigest>,
    /// Loaded from a `--plugin-dir` path.
    pub dev: bool,
    /// Where the plugin is in its life.
    pub state: RowState,
    /// Whether the user has it switched on.
    pub enabled: bool,
    /// What the plugin may do now (requested capabilities that are approved).
    pub effective: CapabilitySet,
    /// The last error, in one line.
    pub last_error: Option<String>,
    /// Scopes the host refused (missing or escaping directories).
    pub scope_issues: Vec<String>,
    /// The settings the plugin declares with their current values.
    pub settings: Vec<(SettingSpec, SettingValue)>,
}

/// The life of a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RowState {
    /// Known, not started.
    Idle,
    /// Started and running.
    Live,
    /// Switched off by the user.
    Off,
    /// Stopped after trapping or timing out this many times.
    Faulted {
        /// How many faults.
        count: u32,
    },
    /// The component could not be loaded.
    LoadFailed,
}

/// A directory that did not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProblem {
    /// The directory name.
    pub dir: String,
    /// The plugin id, when the manifest got far enough to have one.
    pub id: Option<String>,
    /// Why, in one line.
    pub message: String,
}

/// A scripted backend for tests and scenes: it counts calls and returns what it was given.
#[cfg(any(test, feature = "screenshots"))]
#[allow(unused_imports)] // the manager scenes and tests use them
pub(crate) use fake::{FakeBackend, row as fake_row};

#[cfg(any(test, feature = "screenshots"))]
#[allow(dead_code)] // the manager scenes and tests use the parts the backend tests do not
mod fake {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        Arc, BoxFuture, CapabilitySet, CommandContext, CompletionCalls, CompletionRequest,
        ComponentDigest, ContributionId, Manifest, PanelRender, PluginCompletions, PluginId,
        PluginRow, PluginSnapshot, PluginsBackend, RowState, SettingValue, WidgetEvent, WidgetTree,
    };

    type Key = (String, String);

    /// Counts of each backend call.
    #[derive(Debug, Default)]
    pub(crate) struct Calls {
        pub refresh: AtomicUsize,
        pub snapshot: AtomicUsize,
        pub set_enabled: AtomicUsize,
        pub grants_changed: AtomicUsize,
        pub set_setting: AtomicUsize,
        pub start: AtomicUsize,
    }

    /// The fake: a scripted snapshot, optional failures, and call counters.
    #[derive(Default)]
    pub(crate) struct FakeBackend {
        pub calls: Calls,
        snapshot: Mutex<PluginSnapshot>,
        /// Ids passed to `start`, `grants_changed`, in order.
        pub started: Mutex<Vec<String>>,
        pub changed: Mutex<Vec<String>>,
        /// Result of `start`; `Err` is returned for every start.
        pub start_error: Mutex<Option<String>>,
        /// Result of `set_enabled`/`set_setting` when set.
        pub write_error: Mutex<Option<String>>,
        /// Commands run: (plugin, command, context), in order.
        pub commands: Mutex<Vec<(String, String, CommandContext)>>,
        /// Panel events sent: (plugin, panel, event), in order.
        pub events: Mutex<Vec<(String, String, WidgetEvent)>>,
        /// What `complete` answers, one future per entry (empty: no plugin asked).
        pub completions: Mutex<Vec<PluginCompletions>>,
        /// The requests `complete` saw, in order.
        pub complete_requests: Mutex<Vec<CompletionRequest>>,
        /// What `complete` reports as busy; handed out once.
        pub busy: Mutex<Vec<(PluginId, BoxFuture<'static, ()>)>>,
        command_error: Mutex<Option<String>>,
        panels: Mutex<HashMap<Key, PanelRender>>,
        replies: Mutex<HashMap<Key, Result<Option<WidgetTree>, String>>>,
        statuses: Mutex<HashMap<Key, Option<String>>>,
    }

    impl FakeBackend {
        pub fn new(snapshot: PluginSnapshot) -> Arc<Self> {
            Arc::new(Self {
                snapshot: Mutex::new(snapshot),
                ..Self::default()
            })
        }

        /// Replaces what the next `refresh`/`snapshot` returns.
        #[allow(clippy::unwrap_used)] // test double
        pub fn script(&self, snapshot: PluginSnapshot) {
            *self.snapshot.lock().unwrap() = snapshot;
        }

        #[allow(clippy::unwrap_used)] // test double
        /// What `render_panel` returns for a panel.
        pub fn script_panel(&self, plugin: &str, panel: &str, render: PanelRender) {
            self.panels
                .lock()
                .unwrap()
                .insert((plugin.to_owned(), panel.to_owned()), render);
        }

        #[allow(clippy::unwrap_used)] // test double
        /// What `panel_event` returns for a panel.
        pub fn script_event_reply(
            &self,
            plugin: &str,
            panel: &str,
            reply: Result<Option<WidgetTree>, String>,
        ) {
            self.replies
                .lock()
                .unwrap()
                .insert((plugin.to_owned(), panel.to_owned()), reply);
        }

        #[allow(clippy::unwrap_used)] // test double
        /// What `status` returns for an item.
        pub fn script_status(&self, plugin: &str, item: &str, text: Option<String>) {
            self.statuses
                .lock()
                .unwrap()
                .insert((plugin.to_owned(), item.to_owned()), text);
        }

        #[allow(clippy::unwrap_used)] // test double
        /// Makes every `run_command` fail with this reason (`None` clears it).
        pub fn fail_commands(&self, reason: Option<String>) {
            *self.command_error.lock().unwrap() = reason;
        }

        pub fn count(counter: &AtomicUsize) -> usize {
            counter.load(Ordering::SeqCst)
        }

        #[allow(clippy::unwrap_used)] // test double
        fn rows(&self) -> PluginSnapshot {
            self.snapshot.lock().unwrap().clone()
        }
    }

    #[allow(clippy::unwrap_used)] // test double
    impl PluginsBackend for FakeBackend {
        fn refresh(&self) -> PluginSnapshot {
            self.calls.refresh.fetch_add(1, Ordering::SeqCst);
            self.rows()
        }

        fn snapshot(&self) -> PluginSnapshot {
            self.calls.snapshot.fetch_add(1, Ordering::SeqCst);
            self.rows()
        }

        fn set_enabled(&self, id: &PluginId, on: bool) -> Result<(), String> {
            self.calls.set_enabled.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = self.write_error.lock().unwrap().clone() {
                return Err(e);
            }
            for row in &mut self.snapshot.lock().unwrap().rows {
                if row.manifest.id() == id {
                    row.enabled = on;
                    row.state = if on { RowState::Idle } else { RowState::Off };
                }
            }
            Ok(())
        }

        fn grants_changed(&self, id: &PluginId) {
            self.calls.grants_changed.fetch_add(1, Ordering::SeqCst);
            self.changed.lock().unwrap().push(id.to_string());
        }

        fn set_setting(&self, id: &PluginId, key: &str, value: SettingValue) -> Result<(), String> {
            self.calls.set_setting.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = self.write_error.lock().unwrap().clone() {
                return Err(e);
            }
            for row in &mut self.snapshot.lock().unwrap().rows {
                if row.manifest.id() == id {
                    for (spec, current) in &mut row.settings {
                        if spec.key.as_str() == key {
                            *current = value.clone();
                        }
                    }
                }
            }
            Ok(())
        }

        fn start(&self, id: &PluginId) -> BoxFuture<'static, Result<(), String>> {
            self.calls.start.fetch_add(1, Ordering::SeqCst);
            self.started.lock().unwrap().push(id.to_string());
            let result = match self.start_error.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(()),
            };
            Box::pin(async move { result })
        }

        fn run_command(
            &self,
            id: &PluginId,
            command: &ContributionId,
            context: CommandContext,
        ) -> BoxFuture<'static, Result<(), String>> {
            self.commands
                .lock()
                .unwrap()
                .push((id.to_string(), command.to_string(), context));
            let result = match self.command_error.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(()),
            };
            Box::pin(async move { result })
        }

        fn render_panel(
            &self,
            id: &PluginId,
            panel: &ContributionId,
        ) -> BoxFuture<'static, PanelRender> {
            let key = (id.to_string(), panel.to_string());
            let result = self
                .panels
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Err("No panel was scripted.".to_owned()));
            Box::pin(async move { result })
        }

        fn panel_event(
            &self,
            id: &PluginId,
            panel: &ContributionId,
            event: WidgetEvent,
        ) -> BoxFuture<'static, Result<Option<WidgetTree>, String>> {
            let key = (id.to_string(), panel.to_string());
            self.events
                .lock()
                .unwrap()
                .push((key.0.clone(), key.1.clone(), event));
            let result = self
                .replies
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or(Ok(None));
            Box::pin(async move { result })
        }

        fn status(
            &self,
            id: &PluginId,
            item: &ContributionId,
        ) -> BoxFuture<'static, Result<Option<String>, String>> {
            let key = (id.to_string(), item.to_string());
            let text = self
                .statuses
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or(None);
            Box::pin(async move { Ok(text) })
        }

        fn complete(
            &self,
            request: &CompletionRequest,
            only: Option<&[PluginId]>,
        ) -> CompletionCalls {
            self.complete_requests.lock().unwrap().push(request.clone());
            let busy = std::mem::take(&mut *self.busy.lock().unwrap());
            let calls = self
                .completions
                .lock()
                .unwrap()
                .iter()
                .filter(|a| only.is_none_or(|o| o.contains(&a.plugin)))
                .filter(|a| !busy.iter().any(|(id, _)| *id == a.plugin))
                .cloned()
                .map(|answer| Box::pin(async move { Ok(answer) }) as BoxFuture<'static, _>)
                .collect();
            CompletionCalls { calls, busy }
        }
    }

    /// A row for tests and scenes: idle, enabled, nothing effective.
    pub(crate) fn row(manifest: Manifest, component: Option<ComponentDigest>) -> PluginRow {
        PluginRow {
            manifest: Arc::new(manifest),
            component,
            dev: false,
            state: RowState::Idle,
            enabled: true,
            effective: CapabilitySet::new(),
            last_error: None,
            scope_issues: Vec::new(),
            settings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::grant::fixture_manifest;

    #[test]
    fn unavailable_has_no_backend_and_an_empty_book() {
        let s = PluginsStartup::unavailable();
        assert!(s.backend.is_none());
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let key = m.grant_key(None).unwrap();
        assert_eq!(
            s.grants.status(&key),
            crate::plugins::grant::GrantStatus::NeedsPrompt { previous: None }
        );
        assert!(format!("{s:?}").contains("backend: false"));
    }

    #[test]
    fn the_fake_counts_calls_and_follows_its_script() {
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let id = m.id().clone();
        let fake = FakeBackend::new(PluginSnapshot {
            rows: vec![fake_row(m, None)],
            problems: Vec::new(),
        });
        let startup = PluginsStartup::new(Some(fake.clone()), Arc::new(GrantBook::in_memory()));
        let backend = startup.backend.unwrap();
        assert_eq!(FakeBackend::count(&fake.calls.refresh), 0);
        assert_eq!(backend.refresh().rows.len(), 1);
        assert_eq!(backend.snapshot().rows.len(), 1);
        backend.set_enabled(&id, false).unwrap();
        assert_eq!(backend.snapshot().rows[0].state, RowState::Off);
        backend.grants_changed(&id);
        assert_eq!(FakeBackend::count(&fake.calls.refresh), 1);
        assert_eq!(FakeBackend::count(&fake.calls.snapshot), 2);
        assert_eq!(FakeBackend::count(&fake.calls.set_enabled), 1);
        assert_eq!(FakeBackend::count(&fake.calls.grants_changed), 1);
        assert_eq!(fake.changed.lock().unwrap().as_slice(), ["acme-sync"]);
    }

    /// A backend that implements only the required methods.
    struct Bare;

    impl PluginsBackend for Bare {
        fn refresh(&self) -> PluginSnapshot {
            PluginSnapshot::default()
        }
        fn snapshot(&self) -> PluginSnapshot {
            PluginSnapshot::default()
        }
        fn set_enabled(&self, _: &PluginId, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn grants_changed(&self, _: &PluginId) {}
        fn set_setting(&self, _: &PluginId, _: &str, _: SettingValue) -> Result<(), String> {
            Ok(())
        }
        fn start(&self, _: &PluginId) -> BoxFuture<'static, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn ids() -> (PluginId, ContributionId) {
        ("acme-sync".parse().unwrap(), "main".parse().unwrap())
    }

    fn one_text_tree() -> WidgetTree {
        use sqler_plugin_api::{Text, TextStyle, Widget, WidgetNode};
        WidgetTree::new(vec![WidgetNode {
            widget: Widget::Text(Text {
                content: "hi".to_owned(),
                style: TextStyle::Body,
            }),
            children: Vec::new(),
        }])
        .unwrap()
    }

    #[tokio::test]
    async fn the_contribution_methods_refuse_by_default() {
        let (plugin, item) = ids();
        let b = Bare;
        let no = Err(NO_UI.to_owned());
        assert_eq!(
            b.run_command(&plugin, &item, CommandContext::None).await,
            no
        );
        assert_eq!(
            b.render_panel(&plugin, &item).await,
            no.clone().map(|()| one_text_tree())
        );
        assert_eq!(
            b.panel_event(&plugin, &item, WidgetEvent::Clicked("x".to_owned()))
                .await,
            no.clone().map(|()| None)
        );
        assert_eq!(b.status(&plugin, &item).await, no.map(|()| None));
        let request = CompletionRequest {
            driver: None,
            statement: "select 1".to_owned(),
            cursor: 0,
        };
        assert!(b.complete(&request, None).calls.is_empty());
    }

    #[test]
    fn the_inbox_is_taken_once() {
        let (tx, inbox) = ui_channel();
        assert!(inbox.is_set());
        let (plugin, panel) = ids();
        tx.send(PluginUiEvent::RefreshPanel {
            plugin: plugin.clone(),
            panel: panel.clone(),
        })
        .unwrap();
        let mut rx = inbox.take().unwrap();
        assert!(!inbox.is_set());
        assert!(inbox.take().is_none());
        assert_eq!(
            rx.try_recv().unwrap(),
            PluginUiEvent::RefreshPanel { plugin, panel }
        );
        assert!(format!("{inbox:?}").contains("false"));
    }

    #[test]
    fn the_startup_carries_the_ui_inbox() {
        let plain = PluginsStartup::unavailable();
        assert!(!plain.ui_events.is_set());
        let (_tx, inbox) = ui_channel();
        let s = PluginsStartup::unavailable().with_ui_events(inbox);
        assert!(s.ui_events.is_set());
        assert!(format!("{s:?}").contains("ui_events: true"));
    }

    #[tokio::test]
    async fn the_fake_records_and_replays_contributions() {
        let (plugin, item) = ids();
        let fake = FakeBackend::new(PluginSnapshot::default());
        let tree = one_text_tree();

        fake.script_panel("acme-sync", "main", Ok(tree.clone()));
        assert_eq!(fake.render_panel(&plugin, &item).await, Ok(tree.clone()));
        assert!(
            fake.render_panel(&plugin, &"other".parse().unwrap())
                .await
                .is_err()
        );

        assert_eq!(
            fake.panel_event(&plugin, &item, WidgetEvent::Clicked("go".to_owned()))
                .await,
            Ok(None)
        );
        fake.script_event_reply("acme-sync", "main", Ok(Some(tree.clone())));
        assert_eq!(
            fake.panel_event(&plugin, &item, WidgetEvent::Clicked("go".to_owned()))
                .await,
            Ok(Some(tree))
        );
        assert_eq!(fake.events.lock().unwrap().len(), 2);

        assert_eq!(fake.status(&plugin, &item).await, Ok(None));
        fake.script_status("acme-sync", "main", Some("ok".to_owned()));
        assert_eq!(fake.status(&plugin, &item).await, Ok(Some("ok".to_owned())));

        assert_eq!(
            fake.run_command(&plugin, &item, CommandContext::None).await,
            Ok(())
        );
        fake.fail_commands(Some("boom".to_owned()));
        assert_eq!(
            fake.run_command(&plugin, &item, CommandContext::None).await,
            Err("boom".to_owned())
        );
        let commands = fake.commands.lock().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].0, "acme-sync");
        assert_eq!(commands[0].1, "main");
        assert_eq!(commands[0].2, CommandContext::None);
    }
}
