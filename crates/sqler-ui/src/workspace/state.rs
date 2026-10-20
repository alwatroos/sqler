// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Workspace`: sidebar, tab strip and the sessions behind the tabs.

use std::collections::HashMap;

use sqler_driver_api::Session;
use sqler_driver_api::sqler_core::ConnectionConfig;

use super::action::WorkspaceAction;
use crate::settings::SettingsModel;
use crate::sidebar::SidebarModel;
use crate::tabs::{TabConnection, TabId, TabKind, TabStrip};
use crate::theme::{self, Appearance};

/// Identifies one connect attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

/// What the view has to do after a dispatched action.
#[non_exhaustive]
pub enum Effect {
    /// Nothing.
    None,
    /// Start connecting; report back with [`Workspace::connect_finished`].
    Connect {
        /// The request to report back with.
        request: RequestId,
        /// The connection to open.
        config: ConnectionConfig,
    },
    /// Close this session (its tab is gone).
    CloseSession(Box<dyn Session>),
    /// Quit the application.
    Quit,
    /// The settings changed: save them and re-apply the theme and font size.
    ApplySettings,
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Connect { request, config } => f
                .debug_struct("Connect")
                .field("request", request)
                .field("config", config)
                .finish(),
            Self::CloseSession(_) => f.write_str("CloseSession(..)"),
            Self::Quit => f.write_str("Quit"),
            Self::ApplySettings => f.write_str("ApplySettings"),
        }
    }
}

/// The settings tab sits after the last connection tab; it is not a `TabStrip` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    /// Not open.
    Closed,
    /// Open, but a connection tab is shown.
    Background,
    /// Open and shown.
    Active,
}

/// The workspace state. Holds no UI types.
pub struct Workspace {
    sidebar: SidebarModel,
    tabs: TabStrip,
    sessions: HashMap<TabId, Box<dyn Session>>,
    requests: HashMap<RequestId, usize>,
    next_request: u64,
    settings: SettingsModel,
    settings_tab: SettingsTab,
    system_appearance: Appearance,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("sidebar", &self.sidebar)
            .field("tabs", &self.tabs)
            .field("sessions", &self.sessions.len())
            .field("requests", &self.requests)
            .field("settings_tab", &self.settings_tab)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// Creates a workspace with no tabs.
    #[must_use]
    pub fn new(sidebar: SidebarModel) -> Self {
        Self {
            sidebar,
            tabs: TabStrip::default(),
            sessions: HashMap::new(),
            requests: HashMap::new(),
            next_request: 0,
            settings: SettingsModel::default(),
            settings_tab: SettingsTab::Closed,
            system_appearance: Appearance::Light,
        }
    }

    /// Replaces the settings (the binary loads them at startup).
    #[must_use]
    pub fn with_settings(mut self, settings: SettingsModel) -> Self {
        self.settings = settings;
        self
    }

    /// The settings.
    #[must_use]
    pub fn settings(&self) -> &SettingsModel {
        &self.settings
    }

    /// Saves the settings (a no-op when they are not persistent).
    pub fn save_settings(&mut self) {
        self.settings.save();
    }

    /// The look in use: the theme preference resolved against the OS appearance.
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        theme::resolve(self.settings.settings().theme, self.system_appearance)
    }

    /// Records the OS appearance; `true` if [`appearance`](Self::appearance) changed.
    pub fn set_system_appearance(&mut self, system: Appearance) -> bool {
        let before = self.appearance();
        self.system_appearance = system;
        self.appearance() != before
    }

    /// Whether the settings tab is open.
    #[must_use]
    pub fn settings_tab_open(&self) -> bool {
        self.settings_tab != SettingsTab::Closed
    }

    /// Whether the settings tab is the one shown.
    #[must_use]
    pub fn settings_tab_active(&self) -> bool {
        self.settings_tab == SettingsTab::Active
    }

    /// The sidebar state.
    #[must_use]
    pub fn sidebar(&self) -> &SidebarModel {
        &self.sidebar
    }

    /// The tab strip.
    #[must_use]
    pub fn tabs(&self) -> &TabStrip {
        &self.tabs
    }

    /// Applies an action and returns what the view must do next.
    pub fn dispatch(&mut self, action: WorkspaceAction) -> Effect {
        match action {
            WorkspaceAction::SelectConnection(index) => {
                self.sidebar.select(index);
                Effect::None
            }
            WorkspaceAction::OpenConnection(index) => self.open_connection(index),
            WorkspaceAction::ActivateTab(id) => {
                if self.tabs.activate(id) && self.settings_tab == SettingsTab::Active {
                    self.settings_tab = SettingsTab::Background;
                }
                Effect::None
            }
            WorkspaceAction::CloseTab(id) => self.close_tab(id),
            WorkspaceAction::CloseActiveTab => {
                if self.settings_tab == SettingsTab::Active {
                    self.settings_tab = SettingsTab::Closed;
                    return Effect::None;
                }
                match self.tabs.active().map(crate::tabs::Tab::id) {
                    Some(id) => self.close_tab(id),
                    None => Effect::None,
                }
            }
            WorkspaceAction::NextTab => {
                self.step_tab(true);
                Effect::None
            }
            WorkspaceAction::PreviousTab => {
                self.step_tab(false);
                Effect::None
            }
            WorkspaceAction::Quit => Effect::Quit,
            WorkspaceAction::OpenSettings => {
                self.settings_tab = SettingsTab::Active;
                Effect::None
            }
            WorkspaceAction::CloseSettings => {
                self.settings_tab = SettingsTab::Closed;
                Effect::None
            }
            WorkspaceAction::ToggleThemeMode => {
                let pref = self.settings.settings().theme;
                let next = theme::toggled(pref, self.system_appearance);
                Self::changed(self.settings.set_theme(next))
            }
            WorkspaceAction::SetTheme(pref) => Self::changed(self.settings.set_theme(pref)),
            WorkspaceAction::SetUiFontSize(size) => {
                Self::changed(self.settings.set_ui_font_size(size))
            }
        }
    }

    fn changed(changed: bool) -> Effect {
        if changed {
            Effect::ApplySettings
        } else {
            Effect::None
        }
    }

    /// Moves to the next or previous tab; the settings tab, when open, comes after the last
    /// connection tab.
    fn step_tab(&mut self, forward: bool) {
        let first = self.tabs.iter().next().map(crate::tabs::Tab::id);
        let last = self.tabs.iter().last().map(crate::tabs::Tab::id);
        let active = self.tabs.active().map(crate::tabs::Tab::id);
        match self.settings_tab {
            SettingsTab::Closed => {
                if forward {
                    self.tabs.next();
                } else {
                    self.tabs.previous();
                }
            }
            SettingsTab::Active => {
                // With no connection tabs the settings tab stays active.
                if let Some(id) = if forward { first } else { last } {
                    self.tabs.activate(id);
                    self.settings_tab = SettingsTab::Background;
                }
            }
            SettingsTab::Background => {
                if active == if forward { last } else { first } {
                    self.settings_tab = SettingsTab::Active;
                } else if forward {
                    self.tabs.next();
                } else {
                    self.tabs.previous();
                }
            }
        }
    }

    fn open_connection(&mut self, index: usize) -> Effect {
        if !self.sidebar.begin_connect(index) {
            return Effect::None;
        }
        self.sidebar.select(index);
        let request = RequestId(self.next_request);
        self.next_request += 1;
        self.requests.insert(request, index);
        Effect::Connect {
            request,
            config: self.sidebar.entries()[index].config().clone(),
        }
    }

    fn close_tab(&mut self, id: TabId) -> Effect {
        if !self.tabs.close(id) {
            return Effect::None;
        }
        if self.tabs.is_empty() && self.settings_tab == SettingsTab::Background {
            self.settings_tab = SettingsTab::Active;
        }
        match self.sessions.remove(&id) {
            Some(session) => Effect::CloseSession(session),
            None => Effect::None,
        }
    }

    /// Reports the outcome of a connect attempt.
    ///
    /// On success a query tab bound to the connection is opened, activated and returned; the
    /// session is stored under it. On failure the sidebar entry shows the error. An unknown
    /// request drops the session and returns `None`.
    pub fn connect_finished(
        &mut self,
        request: RequestId,
        result: Result<Box<dyn Session>, String>,
    ) -> Option<TabId> {
        let index = self.requests.remove(&request)?;
        match result {
            Ok(session) => {
                self.sidebar.finish_connect(index, Ok(()));
                let config = self.sidebar.entries()[index].config();
                let id = self.tabs.open(
                    TabConnection {
                        name: config.name.clone(),
                        driver: config.driver.clone(),
                        color: config.color.clone(),
                        environment: config.environment,
                    },
                    TabKind::Query,
                );
                self.sessions.insert(id, session);
                if self.settings_tab == SettingsTab::Active {
                    self.settings_tab = SettingsTab::Background;
                }
                Some(id)
            }
            Err(message) => {
                self.sidebar.finish_connect(index, Err(message));
                None
            }
        }
    }

    /// Whether a session is stored for the tab.
    #[must_use]
    pub fn has_session(&self, id: TabId) -> bool {
        self.sessions.contains_key(&id)
    }

    /// Takes the tab's session out (for example to close it on quit).
    pub fn take_session(&mut self, id: TabId) -> Option<Box<dyn Session>> {
        self.sessions.remove(&id)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::sidebar::ConnectionStatus;
    use crate::workspace::testing::FakeSession;
    use sqler_config::ThemePreference;
    use sqler_driver_api::sqler_core::{DriverId, Environment};

    fn ws() -> Workspace {
        let driver = DriverId::new("sqlite").unwrap();
        Workspace::new(SidebarModel::from_connections(vec![
            ConnectionConfig::new("dev", driver.clone(), ":memory:")
                .with_environment(Environment::Dev)
                .with_color("#00ff00"),
            ConnectionConfig::new("prod", driver, ":memory:").with_environment(Environment::Prod),
        ]))
    }

    fn connect(ws: &mut Workspace, index: usize) -> RequestId {
        match ws.dispatch(WorkspaceAction::OpenConnection(index)) {
            Effect::Connect { request, .. } => request,
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    #[test]
    fn select_sets_selection_and_ignores_out_of_range() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::SelectConnection(1)),
            Effect::None
        ));
        assert_eq!(w.sidebar().selected(), Some(1));
        w.dispatch(WorkspaceAction::SelectConnection(7));
        assert_eq!(w.sidebar().selected(), Some(1));
    }

    #[test]
    fn open_then_ok_opens_active_tab_with_session() {
        let mut w = ws();
        let Effect::Connect { request, config } = w.dispatch(WorkspaceAction::OpenConnection(0))
        else {
            panic!("expected Connect");
        };
        assert_eq!(config.name, "dev");
        assert_eq!(
            *w.sidebar().entries()[0].status(),
            ConnectionStatus::Connecting
        );
        let (session, _) = FakeSession::boxed();
        let tab = w.connect_finished(request, Ok(session)).unwrap();
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tab));
        assert!(w.has_session(tab));
        assert_eq!(*w.sidebar().entries()[0].status(), ConnectionStatus::Idle);
        let header = w.tabs().get(tab).unwrap().header();
        assert_eq!(header.title, "dev");
        assert_eq!(header.badge, Some("DEV"));
    }

    #[test]
    fn err_marks_failed_and_opens_no_tab() {
        let mut w = ws();
        let request = connect(&mut w, 1);
        assert!(w.connect_finished(request, Err("nope".into())).is_none());
        assert!(w.tabs().is_empty());
        assert_eq!(
            *w.sidebar().entries()[1].status(),
            ConnectionStatus::Failed("nope".into())
        );
    }

    #[test]
    fn unknown_request_drops_session() {
        let mut w = ws();
        let request = connect(&mut w, 0);
        let (session, _) = FakeSession::boxed();
        assert!(w.connect_finished(request, Ok(session)).is_some());
        // The same request id again is unknown now.
        let (session, _) = FakeSession::boxed();
        assert!(w.connect_finished(request, Ok(session)).is_none());
        assert_eq!(w.tabs().len(), 1);
    }

    #[test]
    fn several_tabs_per_connection() {
        let mut w = ws();
        for _ in 0..2 {
            let request = connect(&mut w, 0);
            let (session, _) = FakeSession::boxed();
            w.connect_finished(request, Ok(session)).unwrap();
        }
        let titles: Vec<_> = w.tabs().iter().map(crate::tabs::Tab::title).collect();
        assert_eq!(titles, ["dev", "dev (2)"]);
    }

    #[test]
    fn close_tab_returns_session_to_close() {
        let mut w = ws();
        let request = connect(&mut w, 0);
        let (session, closed) = FakeSession::boxed();
        let tab = w.connect_finished(request, Ok(session)).unwrap();
        let Effect::CloseSession(session) = w.dispatch(WorkspaceAction::CloseTab(tab)) else {
            panic!("expected CloseSession");
        };
        assert!(w.tabs().is_empty());
        assert!(!w.has_session(tab));
        assert!(!closed.load(Ordering::SeqCst));
        drop(session);
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseTab(tab)),
            Effect::None
        ));
    }

    #[test]
    fn close_active_tab_and_switching() {
        let mut w = ws();
        let mut tabs = Vec::new();
        for i in [0, 1, 0] {
            let request = connect(&mut w, i);
            let (session, _) = FakeSession::boxed();
            tabs.push(w.connect_finished(request, Ok(session)).unwrap());
        }
        w.dispatch(WorkspaceAction::ActivateTab(tabs[0]));
        w.dispatch(WorkspaceAction::NextTab);
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tabs[1]));
        w.dispatch(WorkspaceAction::PreviousTab);
        w.dispatch(WorkspaceAction::PreviousTab);
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tabs[2]));
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::CloseSession(_)
        ));
        // No right neighbour: the left one becomes active.
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tabs[1]));
        assert_eq!(w.tabs().len(), 2);
    }

    #[test]
    fn close_active_without_tabs_is_noop() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::None
        ));
    }

    #[test]
    fn open_out_of_range_is_noop() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::OpenConnection(5)),
            Effect::None
        ));
    }

    fn open_tabs(w: &mut Workspace, n: usize) -> Vec<TabId> {
        (0..n)
            .map(|_| {
                let request = connect(w, 0);
                let (session, _) = FakeSession::boxed();
                w.connect_finished(request, Ok(session)).unwrap()
            })
            .collect()
    }

    fn active(w: &Workspace) -> Option<TabId> {
        w.tabs().active().map(crate::tabs::Tab::id)
    }

    #[test]
    fn open_settings_activates_and_close_closes() {
        let mut w = ws();
        assert!(!w.settings_tab_open());
        assert!(matches!(
            w.dispatch(WorkspaceAction::OpenSettings),
            Effect::None
        ));
        assert!(w.settings_tab_open() && w.settings_tab_active());
        w.dispatch(WorkspaceAction::CloseSettings);
        assert!(!w.settings_tab_open() && !w.settings_tab_active());
    }

    #[test]
    fn close_active_tab_closes_settings_only_when_active() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 2);
        w.dispatch(WorkspaceAction::OpenSettings);
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::None
        ));
        assert!(!w.settings_tab_open());
        assert_eq!(w.tabs().len(), 2);
        // Background: the connection tab closes and settings stays open.
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::ActivateTab(tabs[1]));
        assert!(w.settings_tab_open() && !w.settings_tab_active());
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::CloseSession(_)
        ));
        assert!(w.settings_tab_open() && !w.settings_tab_active());
        assert_eq!(w.tabs().len(), 1);
    }

    #[test]
    fn activate_tab_and_connect_move_settings_to_background() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::ActivateTab(tabs[0]));
        assert!(w.settings_tab_open() && !w.settings_tab_active());
        w.dispatch(WorkspaceAction::OpenSettings);
        let request = connect(&mut w, 1);
        let (session, _) = FakeSession::boxed();
        let tab = w.connect_finished(request, Ok(session)).unwrap();
        assert!(w.settings_tab_open() && !w.settings_tab_active());
        assert_eq!(active(&w), Some(tab));
    }

    #[test]
    fn activate_unknown_tab_keeps_settings_active() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::CloseTab(tabs[0]));
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::ActivateTab(tabs[0]));
        assert!(w.settings_tab_active());
    }

    #[test]
    fn next_and_previous_cycle_through_settings_tab() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 2);
        w.dispatch(WorkspaceAction::OpenSettings);
        // Active -> next -> first connection tab.
        w.dispatch(WorkspaceAction::NextTab);
        assert!(!w.settings_tab_active());
        assert_eq!(active(&w), Some(tabs[0]));
        w.dispatch(WorkspaceAction::NextTab);
        assert_eq!(active(&w), Some(tabs[1]));
        // From the last connection tab -> settings.
        w.dispatch(WorkspaceAction::NextTab);
        assert!(w.settings_tab_active());
        // Previous mirrors it.
        w.dispatch(WorkspaceAction::PreviousTab);
        assert!(!w.settings_tab_active());
        assert_eq!(active(&w), Some(tabs[1]));
        w.dispatch(WorkspaceAction::PreviousTab);
        assert_eq!(active(&w), Some(tabs[0]));
        w.dispatch(WorkspaceAction::PreviousTab);
        assert!(w.settings_tab_active());
        w.dispatch(WorkspaceAction::PreviousTab);
        assert_eq!(active(&w), Some(tabs[1]));
    }

    #[test]
    fn next_with_settings_closed_wraps_connection_tabs() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 2);
        w.dispatch(WorkspaceAction::NextTab);
        assert_eq!(active(&w), Some(tabs[0]));
        assert!(!w.settings_tab_open());
    }

    #[test]
    fn settings_stays_active_without_connection_tabs() {
        let mut w = ws();
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::NextTab);
        assert!(w.settings_tab_active());
        w.dispatch(WorkspaceAction::PreviousTab);
        assert!(w.settings_tab_active());
    }

    #[test]
    fn closing_last_connection_tab_activates_open_settings() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::ActivateTab(tabs[0]));
        w.dispatch(WorkspaceAction::CloseTab(tabs[0]));
        assert!(w.settings_tab_active());
        // With settings closed nothing is activated.
        let mut w = ws();
        let tabs = open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::CloseTab(tabs[0]));
        assert!(!w.settings_tab_open());
    }

    #[test]
    fn toggle_theme_mode_stores_explicit_opposite() {
        let mut w = ws();
        // System + Light: shows light, toggles to dark.
        assert_eq!(w.appearance(), Appearance::Light);
        assert!(matches!(
            w.dispatch(WorkspaceAction::ToggleThemeMode),
            Effect::ApplySettings
        ));
        assert_eq!(w.settings().settings().theme, ThemePreference::Dark);
        assert_eq!(w.appearance(), Appearance::Dark);
        w.dispatch(WorkspaceAction::ToggleThemeMode);
        assert_eq!(w.settings().settings().theme, ThemePreference::Light);
        // Follow system on a dark OS: toggles to light.
        let mut w = ws();
        w.set_system_appearance(Appearance::Dark);
        w.dispatch(WorkspaceAction::ToggleThemeMode);
        assert_eq!(w.settings().settings().theme, ThemePreference::Light);
        assert_eq!(w.appearance(), Appearance::Light);
    }

    #[test]
    fn set_theme_and_font_size_apply_only_when_changed() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetTheme(ThemePreference::System)),
            Effect::None
        ));
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetTheme(ThemePreference::Dark)),
            Effect::ApplySettings
        ));
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetUiFontSize(14)),
            Effect::None
        ));
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetUiFontSize(49)),
            Effect::None
        ));
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetUiFontSize(20)),
            Effect::ApplySettings
        ));
        assert_eq!(w.settings().settings().ui_font_size, 20);
    }

    #[test]
    fn system_appearance_matters_only_under_system_preference() {
        let mut w = ws();
        assert!(w.set_system_appearance(Appearance::Dark));
        assert_eq!(w.appearance(), Appearance::Dark);
        assert!(!w.set_system_appearance(Appearance::Dark));
        assert!(w.set_system_appearance(Appearance::Light));
        w.dispatch(WorkspaceAction::SetTheme(ThemePreference::Dark));
        assert!(!w.set_system_appearance(Appearance::Light));
        assert!(!w.set_system_appearance(Appearance::Dark));
        w.dispatch(WorkspaceAction::SetTheme(ThemePreference::Light));
        assert!(!w.set_system_appearance(Appearance::Light));
    }

    #[test]
    fn with_settings_replaces_the_model() {
        let mut model = SettingsModel::default();
        model.set_theme(ThemePreference::Dark);
        let w = ws().with_settings(model);
        assert_eq!(w.appearance(), Appearance::Dark);
    }

    #[test]
    fn quit_and_take_session() {
        let mut w = ws();
        assert!(matches!(w.dispatch(WorkspaceAction::Quit), Effect::Quit));
        let request = connect(&mut w, 0);
        let (session, _) = FakeSession::boxed();
        let tab = w.connect_finished(request, Ok(session)).unwrap();
        assert!(w.take_session(tab).is_some());
        assert!(w.take_session(tab).is_none());
    }
}
