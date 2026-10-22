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
use sqler_ai_api::mock::ScriptedProvider;
use sqler_ai_api::{AiError, AiProvider, Capabilities, ChatEvent, ModelInfo, StopReason, ToolCall};
use sqler_config::{
    AiConfig, ConnectionAi, ConnectionId, ConnectionSpec, ConnectionStore, KeybindingOverrides,
    MemoryStore, ModelChoice, ProviderConfig, ProviderId, ProviderKind, Secret, ThemePreference,
};
use sqler_driver_api::sqler_core::{DriverId, Environment, ObjectKind};

use super::{Startup, WorkspaceView};
use crate::ai::agent::{AgentMsg, PermissionAsk};
use crate::ai::{AiAction, AiProviderFactory, AiStartup};
use crate::browser::{BrowserAction, LoadState, TreeNodeId};
use crate::command::CommandId;
use crate::connections::{ConnectionAction, Connector};
use crate::palette::OPEN_PALETTE;
use crate::plugins::grant::{sample_prompt, view::GrantSceneHost};
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
        pending: None,
    },
    Scene {
        name: "welcome-recent",
        pending: None,
    },
    Scene {
        name: "editor-results",
        pending: None,
    },
    Scene {
        name: "prod-editor",
        pending: None,
    },
    Scene {
        name: "tabs-overflow",
        pending: None,
    },
    Scene {
        name: "split-editors",
        pending: None,
    },
    Scene {
        name: "object-browser-ddl",
        pending: None,
    },
    Scene {
        name: "palette-recent",
        pending: None,
    },
    Scene {
        name: "messages-error",
        pending: None,
    },
    Scene {
        name: "settings",
        pending: None,
    },
    Scene {
        name: "panels-collapsed",
        pending: None,
    },
    Scene {
        name: "connection-dialog",
        pending: None,
    },
    Scene {
        name: "ai-side-view",
        pending: None,
    },
    Scene {
        name: "ai-audit",
        pending: None,
    },
    Scene {
        name: "ai-side-view-empty",
        pending: None,
    },
    Scene {
        name: "ai-settings",
        pending: None,
    },
    Scene {
        name: "ai-provider-editor",
        pending: None,
    },
    Scene {
        name: "connection-dialog-ai",
        pending: None,
    },
    Scene {
        name: "ai-session",
        pending: None,
    },
    Scene {
        name: "ai-session-prod-confirm",
        pending: None,
    },
    Scene {
        name: "agent-tab",
        pending: None,
    },
    Scene {
        name: "agent-permission",
        pending: None,
    },
    Scene {
        name: "ai-generate",
        pending: None,
    },
    Scene {
        name: "ai-explain",
        pending: None,
    },
    Scene {
        name: "ai-fix-diff",
        pending: None,
    },
    Scene {
        name: "plugins-grant",
        pending: None,
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
    /// The active AI session tab is idle and ready for a message.
    AiReady,
    /// The active AI session tab shows an approval card.
    AiApproval,
    /// The production confirmation of the active AI session tab is open.
    AiGate,
    /// The prompt bar of the active tab has its input.
    PromptInput,
    /// The assistant of the active tab has answered: it is neither idle nor running.
    Assist,
}

impl Wait {
    fn label(&self) -> String {
        match self {
            Self::Editors(n) => format!("{n} editor tab(s) with a session"),
            Self::RunDone => "the run to end".to_owned(),
            Self::Table { name, .. } => format!("table {name} in the object browser"),
            Self::Ddl => "the DDL tab".to_owned(),
            Self::AiReady => "the AI session to be ready".to_owned(),
            Self::AiApproval => "the approval card".to_owned(),
            Self::AiGate => "the production confirmation".to_owned(),
            Self::PromptInput => "the AI prompt bar".to_owned(),
            Self::Assist => "the AI answer".to_owned(),
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
    /// Opens the editor of the AI provider with this id.
    EditProvider(&'static str),
    /// Runs a connection action.
    Connection(ConnectionAction),
    /// Scrolls the settings page to its end.
    ScrollSettingsToEnd,
    /// Selects the sidebar entry called `conn`.
    Select(&'static str),
    /// Records a command as just used in the palette.
    Record(&'static str),
    /// Closes every tab.
    CloseAll,
    /// Marks the n-th query tab as edited (a stand-in for typing).
    MarkDirty(usize),
    /// Types a question into the prompt bar and submits it.
    Prompt(&'static str),
    /// Lets `n` more frames pass, so a layout change settles before the capture.
    Pause(u8),
    /// Fills the audit view with the sample entries.
    AuditFixture,
    /// Types a message into the composer of the active AI session tab and sends it.
    AiSend(&'static str),
    /// Approves the statement the active AI session tab waits on.
    AiApprove,
    /// Opens an agent tab on `dev` with a canned conversation (no process runs).
    AgentFixture(AgentScene),
    /// Waits.
    Wait(Wait),
}

/// The canned conversation of an agent scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentScene {
    /// A finished turn: message, thought, tool call, plan.
    Conversation,
    /// The agent waits for a permission.
    Permission,
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

/// Selects `conn`, opens an AI session on it, asks the question and waits for the approval card.
fn ai_session_steps(conn: &'static str) -> Vec<Step> {
    vec![
        Step::Select(conn),
        Step::Command("ai:open-session"),
        Step::Wait(Wait::AiReady),
        Step::AiSend("Which customers spend the most?"),
        Step::Wait(Wait::AiApproval),
    ]
}

#[allow(clippy::too_many_lines)] // one arm per scene
fn steps_of(scene: &Scene) -> Vec<Step> {
    match scene.name {
        "editor-results" => vec![
            open_step("dev", RESULTS_SQL),
            Step::Wait(Wait::Editors(1)),
            Step::Command("query:run-all"),
            Step::Wait(Wait::RunDone),
        ],
        "prod-editor" => vec![
            open_step(
                "prod",
                "select id, name from customers;\nselect count(*) as total from customers;",
            ),
            Step::Wait(Wait::Editors(1)),
            Step::Command("query:run-all"),
            Step::Wait(Wait::RunDone),
        ],
        "tabs-overflow" => {
            let mut steps = editors("dev", 5, 0);
            steps.extend(editors("staging", 5, 5));
            steps.extend(editors("prod", 4, 10));
            steps.extend([
                Step::MarkDirty(1),
                Step::MarkDirty(6),
                Step::MarkDirty(12),
                Step::Select("dev"),
                Step::Wait(Wait::Table {
                    conn: "dev",
                    name: "customers",
                }),
                Step::Command("object:open-data"),
            ]);
            steps
        }
        "split-editors" => {
            let mut steps = editors("dev", 2, 0);
            steps.push(Step::Command("pane:split-right"));
            steps
        }
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
            Step::Record("query:run-all"),
            Step::Record("editor:format-sql"),
            Step::Record("settings:open"),
            Step::Command(OPEN_PALETTE),
        ],
        "messages-error" => vec![
            open_step("dev", "select 1 as ok;\nselect * from missing_table;"),
            Step::Wait(Wait::Editors(1)),
            Step::Command("query:run-all"),
            Step::Wait(Wait::RunDone),
            Step::Command("workbench:show-messages"),
        ],
        "ai-generate" => vec![
            open_step("dev", ""),
            Step::Wait(Wait::Editors(1)),
            Step::Command("ai:generate-sql"),
            Step::Wait(Wait::PromptInput),
            Step::Prompt("total order value per customer"),
            Step::Wait(Wait::Assist),
            Step::Pause(4),
        ],
        "ai-explain" => vec![
            open_step(
                "dev",
                "select c.name, sum(o.total) from customers c join orders o on o.customer_id = c.id group by c.name;",
            ),
            Step::Wait(Wait::Editors(1)),
            Step::Command("ai:explain-query"),
            Step::Wait(Wait::Assist),
        ],
        "ai-fix-diff" => vec![
            open_step("dev", "select id, nme from customers;"),
            Step::Wait(Wait::Editors(1)),
            Step::Command("query:run-all"),
            Step::Wait(Wait::RunDone),
            Step::Command("ai:fix-query"),
            Step::Wait(Wait::Assist),
        ],
        "settings" => vec![Step::Command("settings:open")],
        "panels-collapsed" => vec![
            open_step("dev", "select * from customers;"),
            Step::Wait(Wait::Editors(1)),
            Step::Command("workbench:toggle-side-panel"),
        ],
        "connection-dialog" => vec![Step::Command("connection:new")],
        "agent-tab" => vec![
            Step::Select("dev"),
            Step::AgentFixture(AgentScene::Conversation),
        ],
        "agent-permission" => vec![
            Step::Select("dev"),
            Step::AgentFixture(AgentScene::Permission),
        ],
        "ai-side-view" => vec![
            open_step("dev", "select * from customers;"),
            Step::Wait(Wait::Editors(1)),
            Step::Command("workbench:show-ai"),
        ],
        "ai-audit" => vec![Step::Command("ai:open-audit-log"), Step::AuditFixture],
        "ai-side-view-empty" => vec![Step::Command("workbench:show-ai")],
        "ai-settings" => vec![Step::Command("ai:open-settings")],
        "ai-provider-editor" => vec![
            Step::Command("ai:open-settings"),
            Step::EditProvider("claude"),
            Step::ScrollSettingsToEnd,
        ],
        "connection-dialog-ai" => vec![
            Step::Command("connection:new"),
            Step::Connection(ConnectionAction::SetAiEnabled(true)),
        ],
        "ai-session" => ai_session_steps("dev"),
        "ai-session-prod-confirm" => {
            let mut steps = ai_session_steps("prod");
            steps.extend([Step::AiApprove, Step::Wait(Wait::AiGate)]);
            steps
        }
        "welcome-recent" => {
            let mut steps = editors("dev", 1, 0);
            steps.extend(editors("staging", 1, 1));
            steps.extend(editors("prod", 1, 2));
            steps.push(Step::CloseAll);
            steps
        }
        // `welcome-empty` shows the window as it opens.
        _ => Vec::new(),
    }
}

/// What the agent of an `agent-*` scene said and asked.
fn agent_messages(scene: AgentScene) -> Vec<AgentMsg> {
    use sqler_acp::{
        AgentEvent, PermissionOption, PermissionOptionKind, PermissionRequest, PlanEntry,
        SessionId, SessionUpdate, ToolCallInfo, ToolCallStatus as ToolStatus,
    };
    let session_id = SessionId::new("scene");
    let update = |update: SessionUpdate| {
        AgentMsg::Event(AgentEvent::Update {
            session_id: session_id.clone(),
            update,
        })
    };
    let plan = |done: &[&str], doing: &str, todo: &str| {
        let entry = |text: &str, status: &str| PlanEntry {
            content: text.to_owned(),
            priority: "medium".to_owned(),
            status: status.to_owned(),
        };
        let mut steps: Vec<PlanEntry> = done.iter().map(|t| entry(t, "completed")).collect();
        steps.push(entry(doing, "in_progress"));
        steps.push(entry(todo, "pending"));
        steps
    };
    let mut messages = vec![
        update(SessionUpdate::AgentThought(
            "The user wants the best customers; I need the order totals first.".to_owned(),
        )),
        update(SessionUpdate::Plan(plan(
            &["Read the schema of customers and orders"],
            "Add up the order totals per customer",
            "Summarise the top customers",
        ))),
        update(SessionUpdate::ToolCall(ToolCallInfo {
            id: "call-1".to_owned(),
            title: Some("sqler: list_tables".to_owned()),
            kind: Some("read".to_owned()),
            status: Some(ToolStatus::Completed),
            text: vec!["customers\norders\nbig_orders".to_owned()],
        })),
        update(SessionUpdate::AgentMessage(
            "Two tables matter here: customers and orders. I joined them and summed the totals; "
                .to_owned(),
        )),
        update(SessionUpdate::AgentMessage(
            "the top three customers account for most of the revenue.".to_owned(),
        )),
    ];
    if scene == AgentScene::Permission {
        let (reply, _wait) = tokio::sync::oneshot::channel();
        messages.push(AgentMsg::Permission(PermissionAsk {
            request: PermissionRequest {
                session_id: session_id.clone(),
                tool_call: ToolCallInfo {
                    id: "call-2".to_owned(),
                    title: Some("Write notes/top-customers.md".to_owned()),
                    kind: Some("edit".to_owned()),
                    status: Some(ToolStatus::Pending),
                    text: vec![
                        "# Top customers\n\nWrite the summary to a file in the agent folder."
                            .to_owned(),
                    ],
                },
                options: vec![
                    PermissionOption {
                        id: "allow-once".to_owned(),
                        name: "Allow once".to_owned(),
                        kind: PermissionOptionKind::AllowOnce,
                    },
                    PermissionOption {
                        id: "reject-once".to_owned(),
                        name: "Reject".to_owned(),
                        kind: PermissionOptionKind::RejectOnce,
                    },
                ],
            },
            reply,
        }));
    }
    messages
}

/// The saved connections of a scene: one in-memory `SQLite` database per environment.
fn store(scene: &Scene) -> Result<ConnectionStore, String> {
    let mut store = ConnectionStore::new();
    if scene.name == "welcome-empty" {
        return Ok(store);
    }
    let driver = DriverId::new("sqlite").map_err(|err| err.to_string())?;
    let ai_on = |on: bool| {
        if on {
            ConnectionAi::new(true)
        } else {
            ConnectionAi::default()
        }
    };
    let dev_ai = ai_on(matches!(
        scene.name,
        "ai-side-view"
            | "ai-session"
            | "ai-generate"
            | "ai-explain"
            | "ai-fix-diff"
            | "agent-tab"
            | "agent-permission"
    ));
    let prod_ai = ai_on(scene.name == "ai-session-prod-confirm");
    let specs = [
        ConnectionSpec::new("dev", driver.clone(), ":memory:")
            .with_environment(Environment::Dev)
            .with_ai(dev_ai),
        ConnectionSpec::new("staging", driver.clone(), ":memory:")
            .with_environment(Environment::Staging),
        ConnectionSpec::new("prod", driver, ":memory:")
            .with_environment(Environment::Prod)
            .with_color("#d14b4b")
            .with_ai(prod_ai),
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
        workbench: crate::workbench::WorkbenchStartup::default(),
        ai: ai_startup(scene),
    }
}

/// The model the scenes' mock provider lists.
const SCENE_MODEL: &str = "qwen2.5-coder:7b";

/// A factory whose providers answer from a script: no network, no keychain, no key text. The AI
/// session scenes get a provider that can call tools and follows the session script; the editor
/// AI scenes get one scripted answer.
struct SceneProviders {
    /// The scene being built.
    scene: &'static str,
    /// The scripted answer of the editor AI scenes, pushed once per `build`.
    answer: Option<&'static str>,
}

/// The question's first statement: a read.
const SESSION_READ_SQL: &str = "select c.name, round(sum(o.total), 2) as spent from customers c join orders o on o.customer_id = c.id group by c.name order by spent desc limit 3";
/// The second statement: a write, which waits for the user.
const SESSION_WRITE_SQL: &str = "update customers set country = 'GB (key account)' where id = 2";

impl SceneProviders {
    /// One script step of the session scenes: some text, then a `run_query` call.
    fn script(text: &str, id: &str, conn: &str, sql: &str) -> Vec<Result<ChatEvent, AiError>> {
        let arguments = serde_json::json!({"connection": conn, "sql": sql}).to_string();
        vec![
            Ok(ChatEvent::TextDelta(text.to_owned())),
            Ok(ChatEvent::ToolCall(ToolCall::new(
                id,
                "run_query",
                arguments,
            ))),
            Ok(ChatEvent::Done {
                stop: StopReason::ToolUse,
            }),
        ]
    }
}

impl AiProviderFactory for SceneProviders {
    fn kinds(&self) -> Vec<ProviderKind> {
        crate::ai::ALL_KINDS.to_vec()
    }

    fn build(
        &self,
        _provider: &ProviderConfig,
        _key: Option<&Secret>,
    ) -> Result<Arc<dyn AiProvider>, AiError> {
        let provider = ScriptedProvider::new("mock").with_models(vec![ModelInfo::new(SCENE_MODEL)]);
        if let Some(answer) = self.answer {
            provider.push_script(vec![
                Ok(ChatEvent::TextDelta(answer.to_owned())),
                Ok(ChatEvent::Done {
                    stop: StopReason::EndTurn,
                }),
            ]);
            return Ok(Arc::new(provider));
        }
        if !matches!(self.scene, "ai-session" | "ai-session-prod-confirm") {
            return Ok(Arc::new(provider));
        }
        let conn = if self.scene == "ai-session" {
            "dev"
        } else {
            "prod"
        };
        let provider = provider.with_capabilities(Capabilities::new(true, false, false, None));
        provider.push_script(Self::script(
            "I'll total the orders per customer first.",
            "call-1",
            conn,
            SESSION_READ_SQL,
        ));
        provider.push_script(Self::script(
            "Contoso Ltd spends the most. I can mark it as a key account.",
            "call-2",
            conn,
            SESSION_WRITE_SQL,
        ));
        Ok(Arc::new(provider))
    }
}

/// The answer the scripted provider gives in the scenes that ask the editor AI.
fn scene_answer(scene: &Scene) -> Option<&'static str> {
    match scene.name {
        "ai-generate" => Some(
            "```sql\nselect c.name, sum(o.total) as total_value\nfrom customers c join orders o on o.customer_id = c.id group by c.name;\n```\nSums the order totals per customer.",
        ),
        "ai-explain" => Some(
            "The query joins every order to its customer and adds up the order totals for each customer name.\n\nIt returns one row per customer who has at least one order. Customers without orders are left out because the join is an inner join.",
        ),
        "ai-fix-diff" => Some(
            "```sql\nselect id, name from customers;\n```\nThe column is called name, not nme.",
        ),
        _ => None,
    }
}

/// The AI settings of a scene: none, an Ollama default, or Ollama plus a keyed Anthropic.
fn ai_startup(scene: &Scene) -> AiStartup {
    let mut config = AiConfig::default();
    if let Ok(ollama) = ProviderId::new("ollama") {
        if matches!(
            scene.name,
            "ai-side-view"
                | "agent-tab"
                | "agent-permission"
                | "ai-settings"
                | "ai-provider-editor"
                | "connection-dialog-ai"
                | "ai-session"
                | "ai-session-prod-confirm"
                | "ai-generate"
                | "ai-explain"
                | "ai-fix-diff"
        ) {
            config.providers.push(
                ProviderConfig::new(ollama.clone(), ProviderKind::Ollama)
                    .with_models([SCENE_MODEL]),
            );
            config.models.default = Some(ModelChoice::new(ollama, SCENE_MODEL));
        }
        if let (true, Ok(claude)) = (
            matches!(scene.name, "ai-settings" | "ai-provider-editor"),
            ProviderId::new("claude"),
        ) {
            config.providers.push(
                ProviderConfig::new(claude, ProviderKind::Anthropic)
                    .with_models(["claude-sonnet"])
                    .with_api_key_in_keychain(true),
            );
        }
    }
    AiStartup {
        config: Ok(config),
        dir: None,
        factory: Arc::new(SceneProviders {
            scene: scene.name,
            answer: scene_answer(scene),
        }),
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
    // The scenes have no config folder, so no audit log; the AI scenes use a sink in memory.
    view.update(cx, |view, _| {
        view.ws.ai_audit = Some(crate::ai::AiAudit::in_memory());
    });
    let root = if scene.name == "plugins-grant" {
        // The dialog is drawn by a small host over the real workspace, so `view.rs` is untouched.
        let host = cx.new(|cx| GrantSceneHost::new(view.clone(), sample_prompt(), cx));
        cx.new(|cx| Root::new(host, window, cx))
    } else {
        cx.new(|cx| Root::new(view.clone(), window, cx))
    };
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
                Step::Pause(n) if n > 0 => {
                    self.steps[0] = Step::Pause(n - 1);
                    self.view.update(cx, |_, cx| cx.notify());
                    return ScenePoll::Waiting;
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
            Step::ScrollSettingsToEnd => self.settings_scroll.scroll_to_bottom(),
            Step::EditProvider(id) => {
                let id = ProviderId::new(id).map_err(|err| err.to_string())?;
                self.dispatch_ai(AiAction::EditProvider(id), cx);
            }
            Step::Connection(action) => self.dispatch_connection(action.clone(), cx),
            Step::Record(command) => {
                let id = CommandId::new(command).map_err(|err| err.to_string())?;
                self.palette.record(&id);
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
            Step::MarkDirty(n) => {
                let tab = self
                    .ws
                    .tabs()
                    .iter()
                    .filter(|tab| tab.kind() == TabKind::Query)
                    .map(crate::tabs::Tab::id)
                    .nth(*n)
                    .ok_or_else(|| format!("no query tab number {n}"))?;
                self.editor_changed(tab, cx);
            }
            Step::Prompt(text) => {
                let tab = self.ws_active_query().ok_or("no query tab")?;
                let input = self
                    .assist_input(tab)
                    .cloned()
                    .ok_or("the prompt bar has no input")?;
                input.update(cx, |state, cx| state.set_value(*text, window, cx));
                self.dispatch_ai(AiAction::SetPrompt((*text).to_owned()), cx);
                self.dispatch_ai(AiAction::SubmitPrompt, cx);
            }
            Step::AuditFixture => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
                self.ws
                    .audit_fixture(crate::ai::audit::fixture::entries(now_ms));
                cx.notify();
            }
            Step::AiSend(text) => {
                let (tab, _) = self
                    .ws
                    .active_ai_session()
                    .ok_or("no AI session tab is active")?;
                self.ai_type_and_send(tab, text, window, cx);
            }
            Step::AiApprove => {
                let (tab, session) = self
                    .ws
                    .active_ai_session()
                    .ok_or("no AI session tab is active")?;
                let id = session
                    .approvals()
                    .shown_current(sqler_ai_tools::ApprovalStage::Confirm)
                    .ok_or("no approval card is shown")?;
                self.dispatch_ai(AiAction::ApproveStatement(tab, id), cx);
            }
            Step::AgentFixture(scene) => self.open_agent_fixture(*scene, cx)?,
            Step::Pause(_) | Step::Wait(_) => {}
        }
        cx.notify();
        Ok(())
    }

    /// Opens the agent tab of an `agent-*` scene on `dev` and brings it forward.
    fn open_agent_fixture(
        &mut self,
        scene: AgentScene,
        cx: &mut gpui::Context<Self>,
    ) -> Result<(), String> {
        let conn = self.scene_conn("dev").ok_or("no connection called dev")?;
        let profile = sqler_acp::builtin_profiles()
            .into_iter()
            .next()
            .ok_or("no built-in agent")?;
        let tab = self
            .ws
            .agent_fixture(&conn, profile, agent_messages(scene))
            .ok_or("the agent tab did not open")?;
        self.dispatch(WorkspaceAction::ActivateTab(tab), cx);
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
            Wait::PromptInput => self
                .ws_active_query()
                .is_some_and(|tab| self.assist_input(tab).is_some()),
            Wait::Assist => {
                self.assist_edits.is_empty()
                    && self.ws_active_query().is_some_and(|tab| {
                        self.ws.assist(tab).is_some_and(|a| {
                            !matches!(
                                a.state,
                                crate::ai::AssistState::Idle
                                    | crate::ai::AssistState::Running { .. }
                            )
                        })
                    })
            }
            Wait::Table { conn, name } => self.table_found(conn, name, cx),
            Wait::AiReady => self
                .ws
                .active_ai_session()
                .is_some_and(|(_, s)| matches!(s.phase(), crate::ai::SessionPhase::Ready)),
            Wait::AiApproval => self
                .ws
                .active_ai_session()
                .is_some_and(|(_, s)| s.approvals().current().is_some()),
            Wait::AiGate => self.ws.active_ai_session().is_some_and(|(tab, _)| {
                self.ws
                    .query_tab(tab)
                    .is_some_and(|q| matches!(q.phase(), crate::query::Phase::Confirming(_)))
            }),
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
