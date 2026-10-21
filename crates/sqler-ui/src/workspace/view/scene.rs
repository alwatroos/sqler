// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Scripted scenes of the real `WorkspaceView` for the screenshot harness
//! (`tests/screenshots.rs`, feature `screenshots`).
//!
//! A scene is a list of steps. Each step goes through the same entry points the UI uses
//! (`run_command`, `dispatch*`, `apply`, `open_editor_on`), never through render code. The
//! harness calls [`ScenePlayer::poll`] between frames until the scene is [`ScenePoll::Ready`].
//! The scenes live next to the views so the items that change a view also change its scene.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Entity, Window};
use gpui_component::Root;
use sqler_config::{
    ConnectionId, ConnectionSpec, ConnectionStore, KeybindingOverrides, MemoryStore,
    ThemePreference,
};
use sqler_driver_api::sqler_core::{DriverId, Environment, ObjectKind};

use super::{Startup, WorkspaceView};
use crate::browser::{BrowserAction, LoadState, TreeNodeId};
use crate::command::CommandId;
use crate::connections::Connector;
use crate::palette::OPEN_PALETTE;
use crate::settings::SettingsModel;
use crate::tabs::{TabId, TabKind};
use crate::workspace::WorkspaceAction;

/// The theme a scene is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneTheme {
    /// The dark theme.
    Dark,
    /// The light theme.
    Light,
}

impl SceneTheme {
    /// Both themes, in file order.
    pub const ALL: [Self; 2] = [Self::Dark, Self::Light];

    /// The name used in file names: `dark` or `light`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    fn preference(self) -> ThemePreference {
        match self {
            Self::Dark => ThemePreference::Dark,
            Self::Light => ThemePreference::Light,
        }
    }
}

/// One scene of the set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scene {
    /// The name used in file names.
    pub name: &'static str,
    /// What the scene lacks until a later item builds it, with the owning item; `None` when the
    /// scene shows what it is named after.
    pub pending: Option<&'static str>,
}

/// Every scene, in file order.
pub const SCENES: &[Scene] = &[
    Scene {
        name: "welcome-empty",
        pending: Some("welcome page (U3)"),
    },
    Scene {
        name: "welcome-recent",
        pending: Some("recent list on the welcome page (U3)"),
    },
    Scene {
        name: "editor-results",
        pending: None,
    },
    Scene {
        name: "prod-editor",
        pending: Some("toolbar and status bar (U3, U4)"),
    },
    Scene {
        name: "tabs-overflow",
        pending: Some("preview and dirty marks (U3)"),
    },
    Scene {
        name: "split-editors",
        pending: Some("editor groups (U3)"),
    },
    Scene {
        name: "object-browser-ddl",
        pending: Some("restyle (U5)"),
    },
    Scene {
        name: "palette-recent",
        pending: Some("\"Recently used\" in the palette (U5)"),
    },
    Scene {
        name: "messages-error",
        pending: Some("Messages panel (U3)"),
    },
    Scene {
        name: "settings",
        pending: Some("restyle (U5)"),
    },
    Scene {
        name: "panels-collapsed",
        pending: Some("collapsible panels (U3)"),
    },
    Scene {
        name: "connection-dialog",
        pending: Some("restyle (U5)"),
    },
];

/// The window size of every scene, in logical pixels.
pub const WINDOW_SIZE: (f32, f32) = (1280., 800.);

/// How long one wait of a scene may last before the scene gives up (or falls back).
pub const SCENE_WAIT: Duration = Duration::from_secs(20);

/// The statement `editor-results` runs, against the table the harness seeds.
const RESULTS_SQL: &str = "select * from orders order by id;";

/// The scene called `name`.
#[must_use]
pub fn find(name: &str) -> Option<&'static Scene> {
    SCENES.iter().find(|scene| scene.name == name)
}

/// What a poll found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenePoll {
    /// The scene is not done; poll again after the executors ran.
    Waiting,
    /// The scene is ready to be drawn and captured.
    Ready {
        /// A remark for the log, for example that a fallback was used.
        note: Option<String>,
    },
    /// The scene cannot be reached.
    Failed(String),
}

/// What a wait needs to see before the next step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wait {
    /// `n` editor tabs are open, each with its session and its editor on screen.
    Editors(usize),
    /// The last run of the active query tab has ended and left results.
    RunDone,
    /// The root of the connection's tree is loaded. Expands every unloaded folder, selects the
    /// table called `name` once it is listed and waits for that.
    Table {
        conn: &'static str,
        name: &'static str,
    },
    /// The active tab shows a generated script.
    Ddl,
}

impl Wait {
    fn label(&self) -> String {
        match self {
            Self::Editors(n) => format!("{n} editor tab(s) with a session"),
            Self::RunDone => "the run to end".to_owned(),
            Self::Table { name, .. } => format!("table {name} in the object browser"),
            Self::Ddl => "the DDL tab".to_owned(),
        }
    }
}

/// One step of a scene.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Opens an editor on the connection called `conn`, starting with `seed`.
    Open { conn: &'static str, seed: String },
    /// Runs a command by ID.
    Command(&'static str),
    /// Selects the sidebar entry called `conn`.
    Select(&'static str),
    /// Closes every tab.
    CloseAll,
    /// Waits.
    Wait(Wait),
}

fn open_step(conn: &'static str, seed: &str) -> Step {
    Step::Open {
        conn,
        seed: seed.to_owned(),
    }
}

fn editors(conn: &'static str, count: usize, first: usize) -> Vec<Step> {
    let mut steps = Vec::new();
    for n in 0..count {
        let table = ["customers", "orders", "big_orders"][n % 3];
        steps.push(open_step(
            conn,
            &format!("select * from {table} order by 1; -- {conn} {}", n + 1),
        ));
        steps.push(Step::Wait(Wait::Editors(first + n + 1)));
    }
    steps
}

fn steps_of(scene: &Scene) -> Vec<Step> {
    match scene.name {
        "editor-results" => vec![
            open_step("dev", RESULTS_SQL),
            Step::Wait(Wait::Editors(1)),
            Step::Command("query:run-all"),
            Step::Wait(Wait::RunDone),
        ],
        "prod-editor" => vec![
            open_step("prod", "select id, name from customers;"),
            Step::Wait(Wait::Editors(1)),
        ],
        "tabs-overflow" => {
            let mut steps = editors("dev", 5, 0);
            steps.extend(editors("staging", 5, 5));
            steps.extend(editors("prod", 4, 10));
            steps
        }
        "split-editors" => editors("dev", 2, 0),
        "object-browser-ddl" => vec![
            open_step("dev", "select * from customers;"),
            Step::Wait(Wait::Editors(1)),
            Step::Select("dev"),
            Step::Wait(Wait::Table {
                conn: "dev",
                name: "customers",
            }),
            Step::Command("object:generate-create"),
            Step::Wait(Wait::Ddl),
        ],
        "palette-recent" => vec![
            open_step("dev", "select * from customers;"),
            Step::Wait(Wait::Editors(1)),
            Step::Command(OPEN_PALETTE),
        ],
        "messages-error" => vec![
            open_step("dev", "select * from missing_table;"),
            Step::Wait(Wait::Editors(1)),
            Step::Command("query:run-all"),
            Step::Wait(Wait::RunDone),
        ],
        "settings" => vec![Step::Command("settings:open")],
        "panels-collapsed" => vec![
            open_step("dev", "select * from customers;"),
            Step::Wait(Wait::Editors(1)),
        ],
        "connection-dialog" => vec![Step::Command("connection:new")],
        // `welcome-empty` and `welcome-recent` show the window as it opens.
        _ => Vec::new(),
    }
}

/// The saved connections of a scene: one in-memory `SQLite` database per environment.
fn store(scene: &Scene) -> Result<ConnectionStore, String> {
    let mut store = ConnectionStore::new();
    if scene.name == "welcome-empty" {
        return Ok(store);
    }
    let driver = DriverId::new("sqlite").map_err(|err| err.to_string())?;
    let specs = [
        ConnectionSpec::new("dev", driver.clone(), ":memory:").with_environment(Environment::Dev),
        ConnectionSpec::new("staging", driver.clone(), ":memory:")
            .with_environment(Environment::Staging),
        ConnectionSpec::new("prod", driver, ":memory:")
            .with_environment(Environment::Prod)
            .with_color("#d14b4b"),
    ];
    for spec in specs {
        store
            .add_connection(spec, None)
            .map_err(|err| err.to_string())?;
    }
    Ok(store)
}

fn startup(
    scene: &Scene,
    theme: SceneTheme,
    connector: Arc<dyn Connector>,
    runtime: tokio::runtime::Handle,
) -> Startup {
    let mut settings = SettingsModel::default();
    settings.set_theme(theme.preference());
    Startup {
        connections: store(scene),
        config_dir: None,
        connector,
        secrets: Arc::new(MemoryStore::new()),
        runtime,
        exit_after_first_frame: false,
        settings,
        keybindings: Ok(KeybindingOverrides::default()),
        history_dir: None,
    }
}

/// Builds the window content of a scene: `Root` around the real `WorkspaceView`, with the
/// steps still to run in the returned player.
pub fn open(
    scene: &Scene,
    theme: SceneTheme,
    connector: Arc<dyn Connector>,
    runtime: tokio::runtime::Handle,
    window: &mut Window,
    cx: &mut App,
) -> (Entity<Root>, ScenePlayer) {
    let startup = startup(scene, theme, connector, runtime);
    let view = cx.new(|cx| WorkspaceView::new(startup, window, cx));
    let root = cx.new(|cx| Root::new(view.clone(), window, cx));
    let player = ScenePlayer {
        view,
        scene: *scene,
        steps: steps_of(scene).into(),
        started: None,
        fallback: false,
    };
    (root, player)
}

/// Runs the steps of a scene, one poll at a time.
pub struct ScenePlayer {
    view: Entity<WorkspaceView>,
    scene: Scene,
    steps: std::collections::VecDeque<Step>,
    /// When the wait at the front began.
    started: Option<Instant>,
    /// Whether the scene gave up its script for the synthetic grid.
    fallback: bool,
}

impl ScenePlayer {
    /// Runs every step that can run now and reports where the scene is.
    pub fn poll(&mut self, window: &mut Window, cx: &mut App) -> ScenePoll {
        while let Some(step) = self.steps.front().cloned() {
            match step {
                Step::Wait(wait) => {
                    if self.view.update(cx, |view, cx| view.wait_met(&wait, cx)) {
                        self.steps.pop_front();
                        self.started = None;
                        continue;
                    }
                    let started = *self.started.get_or_insert_with(Instant::now);
                    if started.elapsed() < SCENE_WAIT {
                        return ScenePoll::Waiting;
                    }
                    if self.scene.name == "editor-results" && !self.fallback {
                        self.fallback = true;
                        self.started = None;
                        self.steps = [Step::CloseAll, Step::Command("dev:open-synthetic-grid")]
                            .into_iter()
                            .collect();
                        continue;
                    }
                    return ScenePoll::Failed(format!(
                        "{}: timed out waiting for {}",
                        self.scene.name,
                        wait.label()
                    ));
                }
                other => {
                    self.steps.pop_front();
                    if let Err(message) = self
                        .view
                        .update(cx, |view, cx| view.run_step(&other, window, cx))
                    {
                        return ScenePoll::Failed(format!("{}: {message}", self.scene.name));
                    }
                }
            }
        }
        ScenePoll::Ready {
            note: self.fallback.then(|| "fallback: synthetic grid".to_owned()),
        }
    }
}

impl WorkspaceView {
    /// The ID of the saved connection called `name`.
    fn scene_conn(&self, name: &str) -> Option<ConnectionId> {
        self.ws
            .sidebar()
            .entries()
            .iter()
            .find(|entry| entry.config().name == name)
            .map(|entry| entry.id().clone())
    }

    fn run_step(
        &mut self,
        step: &Step,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Result<(), String> {
        match step {
            Step::Open { conn, seed } => {
                let id = self
                    .scene_conn(conn)
                    .ok_or_else(|| format!("no connection called {conn}"))?;
                let effect = self.ws.open_editor_on(&id, Some(seed.clone()));
                self.apply(effect, cx);
                self.settle(cx);
            }
            Step::Command(command) => {
                let id = CommandId::new(command).map_err(|err| err.to_string())?;
                self.run_command(&id, window, cx);
            }
            Step::Select(conn) => {
                let index = self
                    .ws
                    .sidebar()
                    .entries()
                    .iter()
                    .position(|entry| entry.config().name == *conn)
                    .ok_or_else(|| format!("no connection called {conn}"))?;
                self.dispatch(WorkspaceAction::SelectConnection(index), cx);
            }
            Step::CloseAll => {
                let tabs: Vec<TabId> = self.ws.tabs().iter().map(crate::tabs::Tab::id).collect();
                for tab in tabs {
                    self.dispatch(WorkspaceAction::CloseTab(tab), cx);
                }
            }
            Step::Wait(_) => {}
        }
        cx.notify();
        Ok(())
    }

    /// Whether `wait` is met. Some waits also move the scene along (they expand tree folders).
    fn wait_met(&mut self, wait: &Wait, cx: &mut gpui::Context<Self>) -> bool {
        match wait {
            Wait::Editors(n) => {
                let tabs: Vec<TabId> = self
                    .ws
                    .tabs()
                    .iter()
                    .filter(|tab| tab.kind() == TabKind::Query)
                    .map(crate::tabs::Tab::id)
                    .collect();
                tabs.len() == *n
                    && tabs
                        .iter()
                        .all(|tab| self.ws.has_session(*tab) && self.editors.contains(*tab))
            }
            Wait::RunDone => {
                let Some(tab) = self.ws_active_query() else {
                    return false;
                };
                self.ws
                    .query_tab(tab)
                    .is_some_and(|q| !q.is_running() && !q.results().is_empty())
            }
            Wait::Table { conn, name } => self.table_found(conn, name, cx),
            Wait::Ddl => self.ws.tabs().active().is_some_and(|tab| {
                self.ws.object_tab(tab.id()).is_some_and(|object| {
                    matches!(object.content, crate::browser::ObjectTabContent::Script(_))
                })
            }),
        }
    }

    /// Expands the folded folders and schemas of the tree and selects the table when it is listed.
    fn table_found(&mut self, conn: &str, name: &str, cx: &mut gpui::Context<Self>) -> bool {
        let Some(id) = self.scene_conn(conn) else {
            return false;
        };
        let Some(tree) = self.ws.browser(&id) else {
            return false;
        };
        let rows = tree.rows().to_vec();
        let found = rows
            .iter()
            .find(|row| row.kind == Some(ObjectKind::Table) && row.label == name);
        if let Some(row) = found {
            let node = row.id;
            self.ws.select_object(&id, node);
            self.settle(cx);
            return true;
        }
        let folders: Vec<TreeNodeId> = rows
            .iter()
            .filter(|row| {
                matches!(row.kind, None | Some(ObjectKind::Schema))
                    && row.expandable
                    && !row.expanded
                    && row.state != LoadState::Loading
            })
            .map(|row| row.id)
            .collect();
        for node in folders {
            self.dispatch_object(&id, node, BrowserAction::Expand, cx);
        }
        false
    }
}
