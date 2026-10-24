// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The cross-surface guard suite (T17, AC5): the same scenarios run through the in-app session
//! (an `ExplorationSession` with a scripted model), the MCP surface (`McpServer` over HTTP with a
//! bearer client) and an ACP agent (`start_agent` with the mock agent). Every surface gets the
//! same tool set over a `SQLite` file, the same limits and the same user. The normalised
//! [`Outcome`] of a call must be identical on all three.
//!
//! The mock agent cannot call MCP, so on the ACP leg the same bearer client sends the calls to the
//! endpoint the agent was told about: the URL and the token come from the agent's own
//! `descriptor` reply (spec assumption A8). Approvals reach the host as `AgentMsg::Ask`.
//!
//! Nothing here asserts a duration. Waits are channel waits with a hang guard. Every agent is
//! closed at the end of its test and the process is checked to be gone.
//!
//! The mock agent is found through `SQLER_TEST_MOCK_AGENT` or next to the test binary; a missing
//! binary fails the test (build it with `cargo build -p sqler-acp --bins`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::missing_panics_doc,
    clippy::similar_names
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sqler_acp::{AgentEvent, AgentProfile, ExitInfo, LaunchOptions, SessionUpdate, StopReason};
use sqler_ai_api::mock::ScriptedProvider;
use sqler_ai_api::{AiError, Capabilities, ChatEvent, StopReason as ModelStop, ToolCall, Usage};
use sqler_ai_tools::session::{
    ExplorationSession, SessionConfig, ToolCallStatus, TranscriptEntry, TurnOutcome,
};
use sqler_ai_tools::{
    ApprovalReason, ApprovalStage, AuditEntry, AuditLog, AuditOptions, AuditSink, ConnectionInfo,
    Decision, ExternalApproval, ExternalContext, ExternalTool, ExternalTools, MemoryAudit, Surface,
    ToolError, ToolSet, unwrap_untrusted,
};
use sqler_config::{AiConfig, ConnectionSpec};
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Environment, Query};
use sqler_driver_api::{BoxFuture, DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_mcp::{AuthToken, McpServer, ServerOptions};
use sqler_sql::{Dialect, StatementClass};
use sqler_ui::ai::agent::{
    AgentAbort, AgentMsg, AgentStart, PermissionAsk, PermissionQueue, start_agent,
};
use sqler_ui::ai::session::TabSource;
use sqler_ui::ai::{AiAudit, ApprovalAsk, ApprovalQueue, ChannelApprover, HostMsg, open_prod_gate};
use sqler_ui::browser::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt, SecretSource};
use sqler_ui::query::{Phase, QueryTab};
use sqler_ui::tabs::{TabConnection, TabId, TabKind, TabStrip};
use tokio::net::TcpStream;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// The hang guard of every wait. It is never asserted against.
const HANG: Duration = Duration::from_secs(30);
static SEQ: AtomicUsize = AtomicUsize::new(0);

const READ: &str = "select id, name from users order by id";
const WRITE: &str = "update users set name='zed' where id=1";
const DELETE: &str = "delete from users where id=2";
const PROD_WRITE: &str = "update users set name='p' where id=1";
// A recursive CTE would need approval (the classifier cannot tell it from a write), so the limits
// are driven by plain selects: a 9 row cross join, and an 18 way cross join (3^18 rows to count,
// seconds of work) that outlasts the 1 s limit. The `SQLite` driver cancels only between rows, so
// the statement itself runs on after the call returned; the runtime is abandoned, not awaited.
const TO_NINE: &str = "select a.id, b.id from users a cross join users b";
const ENDLESS: &str = "select count(*) from users u0 cross join users u1 cross join users u2 cross join users u3 cross join users u4 cross join users u5 cross join users u6 cross join users u7 cross join users u8 cross join users u9 cross join users u10 cross join users u11 cross join users u12 cross join users u13 cross join users u14 cross join users u15 cross join users u16 cross join users u17";
const SECRETS: &str = "select id, password from users order by id";

/// The three surfaces, in the order the outcomes are compared.
const SURFACES: [Surface; 3] = [Surface::Session, Surface::Mcp, Surface::Agent];

// ---------------------------------------------------------------------------------------------
// The environment: files, the connector, the mock agent.
// ---------------------------------------------------------------------------------------------

/// A folder that is removed with the guard: the database, the audit file and the agent's cwd.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "sqler-guard-suite-{}-{n}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn db(&self) -> String {
        self.0.join("db.sqlite").display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The path of the mock agent. A missing binary fails the test: it never skips.
fn mock_agent() -> String {
    if let Ok(path) = std::env::var("SQLER_TEST_MOCK_AGENT") {
        return path;
    }
    let exe = std::env::current_exe().unwrap();
    let name = format!("sqler-acp-mock-agent{}", std::env::consts::EXE_SUFFIX);
    let path = exe.parent().and_then(Path::parent).unwrap().join(name);
    assert!(
        path.exists(),
        "the mock agent is missing at {}: run `cargo build -p sqler-acp --bins`",
        path.display()
    );
    path.display().to_string()
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    // `tasklist` lists the process only while it exists; matching the image name too keeps a
    // reused pid of another program from counting.
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_ascii_lowercase()
                .lines()
                .any(|l| l.contains("sqler-acp-mock-agent") && l.contains(&format!("\"{pid}\"")))
        })
}

#[cfg(not(any(unix, windows)))]
fn alive(_pid: u32) -> bool {
    false
}

/// Runs a future under the hang guard.
async fn guard<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(HANG, fut)
        .await
        .unwrap_or_else(|_| panic!("hang guard: {what}"))
}

fn driver() -> DriverId {
    DriverId::new(DRIVER_ID).unwrap()
}

async fn open_sqlite(path: &str) -> Box<dyn Session> {
    let mut registry = DriverRegistry::new();
    registry.register(Box::new(SqliteDriver::new())).unwrap();
    let config = ConnectionConfig::new("t", driver(), path);
    registry.connect(&config).await.unwrap()
}

/// Runs `sql` on a session of its own and returns the rows as text cells.
async fn run_sql(path: &str, sql: &str) -> Vec<Vec<String>> {
    let mut session = open_sqlite(path).await;
    let q = Query::new(sql);
    let mut stream = session.execute(&q).await.unwrap();
    let result = collect(&mut *stream).await.unwrap();
    let rows = result
        .rows()
        .iter()
        .map(|r| r.0.iter().map(|c| format!("{c:?}")).collect())
        .collect();
    drop(stream);
    session.close().await.unwrap();
    rows
}

/// `users(id, name, password)` with three rows whose passwords are canaries.
async fn seed(path: &str) {
    for sql in [
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, password TEXT)",
        "INSERT INTO users VALUES (1,'ann','pw-canary-1'),(2,'bob','pw-canary-2'),(3,'cy','pw-canary-3')",
    ] {
        run_sql(path, sql).await;
    }
}

/// The table as it is now, read on a session of its own.
async fn table(path: &str) -> Vec<Vec<String>> {
    run_sql(path, "select id, name, password from users order by id").await
}

/// Opens real `SQLite` sessions on the target of the request.
struct Lite;

impl Connector for Lite {
    fn driver_ids(&self) -> Vec<DriverId> {
        vec![driver()]
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
        Box::pin(async move { Ok(open_sqlite(&request.spec.target).await) })
    }

    fn trust_host_key(
        &self,
        _key: HostKeyPrompt,
    ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// The SQL the plugin tool asks to run.
const NOTE: &str = "insert into users values (9, 'note', 'n')";
/// The model name of the plugin tool.
const NOTE_TOOL: &str = "fake__write_note";

/// A plugin tool that asks the surface to approve one `INSERT` and runs it only when approved.
struct NotePlugin {
    db: String,
    env: Environment,
}

impl ExternalTools for NotePlugin {
    fn tools(&self) -> Vec<ExternalTool> {
        vec![ExternalTool::new(
            NOTE_TOOL,
            "Adds a note row.",
            json!({"type": "object", "properties": {}}),
        )]
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        _arguments: Value,
        ctx: ExternalContext,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            if name != NOTE_TOOL {
                return Err(ToolError::UnknownTool(name.to_owned()));
            }
            let connection = ctx.use_connection(self.env.as_str())?;
            let mut stages = vec![ApprovalStage::Confirm];
            if self.env == Environment::Prod {
                stages.push(ApprovalStage::ProdSecond);
            }
            for stage in stages {
                let request = ExternalApproval::new(
                    connection.id.clone(),
                    NOTE,
                    Vec::new(),
                    StatementClass::Write,
                    ApprovalReason::Write,
                    stage,
                );
                if ctx.approve(request).await != Decision::Approve {
                    return Ok(json!({"inserted": false}));
                }
            }
            run_sql(&self.db, NOTE).await;
            Ok(json!({"inserted": true}))
        })
    }
}

/// The connection of a scenario: `dev` or `prod`, over the database file.
fn connection(env: Environment, path: &str) -> (ConnectionInfo, ConnectRequest) {
    let id = env.as_str();
    let info = ConnectionInfo::new(id, id, driver(), Some(env));
    let spec = ConnectionSpec::new(id, driver(), path).with_environment(env);
    let request = ConnectRequest {
        spec,
        db_password: SecretSource::None,
        ssh_secret: SecretSource::None,
    };
    (info, request)
}

/// The time limit of every scenario but the time-limit one. The limit covers the whole call, the
/// wait for the user's approval included, so it must not be the thing a slow runner trips over; it
/// stays below `HANG`.
const CALM_LIMIT_SECS: u32 = 25;
/// The smallest time limit the config can hold (whole seconds): the time-limit scenario only.
const SHORT_LIMIT_SECS: u32 = 1;

/// The limits of the suite: 5 rows and a time limit of `secs`.
fn config(secs: u32) -> AiConfig {
    let mut config = AiConfig::default();
    config.limits.max_rows = 5;
    config.limits.timeout_secs = secs;
    config
}

fn a_tab() -> TabId {
    TabStrip::default().open(
        TabConnection {
            name: "dev".into(),
            driver: driver(),
            color: None,
            environment: None,
        },
        TabKind::Agent,
    )
}

fn mock_profile() -> AgentProfile {
    AgentProfile {
        id: "mock".into(),
        name: "Mock Agent".into(),
        command: mock_agent(),
        args: Vec::new(),
        env: Vec::new(),
        inherit_env: Vec::new(),
        install_hint: None,
    }
}

// ---------------------------------------------------------------------------------------------
// The stand-in MCP client.
// ---------------------------------------------------------------------------------------------

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

/// A bearer client of the endpoint: what an agent's MCP client would do.
#[derive(Clone)]
struct McpClient {
    addr: SocketAddr,
    auth: String,
}

impl McpClient {
    async fn post(&self, session: Option<&str>, body: &Value) -> Reply {
        let stream = TcpStream::connect(self.addr).await.unwrap();
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.unwrap();
        tokio::spawn(connection);
        let mut builder = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", self.addr.to_string())
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .header("authorization", &self.auth);
        if let Some(session) = session {
            builder = builder.header("mcp-session-id", session);
        }
        let request = builder
            .body(Full::new(Bytes::from(body.to_string())))
            .unwrap();
        let response = guard("a response", sender.send_request(request))
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let body = guard("a finished body", body.collect())
            .await
            .unwrap()
            .to_bytes();
        Reply {
            status: parts.status,
            headers: parts.headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    /// The JSON-RPC response with this id, whether the body is JSON or an event stream.
    fn response_to(reply: &Reply, id: i64) -> Value {
        let text = reply.body.trim();
        let mut all = Vec::new();
        if let Ok(value) = serde_json::from_str::<Value>(text) {
            all.push(value);
        }
        all.extend(
            text.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok()),
        );
        all.into_iter()
            .find(|m| m["id"] == id)
            .unwrap_or_else(|| panic!("no response {id} in {:?}", reply.body))
    }

    /// Initializes a session and returns its id.
    async fn session(&self) -> String {
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "guard-suite", "version": "0"}}});
        let reply = self.post(None, &init).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let session = reply
            .headers
            .get("mcp-session-id")
            .expect("a session id")
            .to_str()
            .unwrap()
            .to_owned();
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let reply = self.post(Some(&session), &note).await;
        assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.body);
        session
    }

    /// Calls `run_query` on a session of its own and returns the JSON-RPC `result`.
    async fn run_query(&self, connection: &str, sql: &str) -> Value {
        self.call_tool("run_query", json!({"connection": connection, "sql": sql}))
            .await
    }

    /// Calls a tool on a session of its own and returns the JSON-RPC `result`.
    async fn call_tool(&self, name: &str, arguments: Value) -> Value {
        let session = self.session().await;
        let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        let reply = self.post(Some(&session), &body).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        Self::response_to(&reply, 2)["result"].clone()
    }

    /// The names `tools/list` returns, in order.
    async fn tool_names(&self) -> Vec<String> {
        let session = self.session().await;
        let body = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {}});
        let reply = self.post(Some(&session), &body).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        Self::response_to(&reply, 3)["result"]["tools"]
            .as_array()
            .expect("a tool list")
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// The user: one answerer for every surface.
// ---------------------------------------------------------------------------------------------

/// Plays the user. Every ask goes into an [`ApprovalQueue`] and is answered through `answer_id`.
/// For `ProdSecond` it first opens the production gate on a [`QueryTab`] and checks that the gate
/// shows exactly the SQL of the ask.
struct User {
    queue: ApprovalQueue,
    decisions: VecDeque<Decision>,
    sql: String,
    asked: Rc<RefCell<Vec<ApprovalStage>>>,
    gates: Rc<RefCell<usize>>,
    /// The dialect of the connection the gate is opened for.
    dialect: Option<Dialect>,
}

impl User {
    fn new(sql: &str, decisions: &[Decision]) -> Self {
        Self {
            dialect: Dialect::from_driver_id(driver().as_str()),
            queue: ApprovalQueue::default(),
            decisions: decisions.iter().copied().collect(),
            sql: sql.to_owned(),
            asked: Rc::default(),
            gates: Rc::default(),
        }
    }

    /// Answers asks forever; the caller races it against the call.
    async fn serve(&mut self, asks: &mut UnboundedReceiver<ApprovalAsk>) -> Infallible {
        loop {
            let Some(ask) = asks.recv().await else {
                std::future::pending::<()>().await;
                continue;
            };
            let id = self.queue.push(ask);
            let (stage, sql) = {
                let current = self.queue.current().expect("an ask");
                assert_eq!(current.id, id);
                (current.request.stage, current.request.sql.clone())
            };
            assert_eq!(sql, self.sql, "the ask carries the exact SQL");
            let mut gate = QueryTab::new();
            if stage == ApprovalStage::ProdSecond {
                open_prod_gate(&mut gate, self.dialect, &sql).expect("the gate opens");
                let Phase::Confirming(list) = gate.phase() else {
                    panic!("the gate is not confirming: {:?}", gate.phase());
                };
                assert_eq!(list.len(), 1);
                assert_eq!(list[0].sql, sql, "the gate shows the exact SQL");
                *self.gates.borrow_mut() += 1;
            }
            self.asked.borrow_mut().push(stage);
            let decision = self.decisions.pop_front().expect("an unscripted ask");
            assert!(self.queue.answer_id(id, stage, decision), "answered once");
            // A second answer to the same ask changes nothing.
            assert!(!self.queue.answer_id(id, stage, Decision::Approve));
            gate.dismiss();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The normalised outcome.
// ---------------------------------------------------------------------------------------------

/// One statement of a call as the audit entry records it.
#[derive(Debug, Clone, PartialEq)]
struct AuditedStatement {
    sql: String,
    class: String,
    approval: String,
    ran: bool,
    rows: Option<u64>,
    truncated: Option<String>,
    failed: bool,
}

/// What a call looked like, with nothing that differs by surface.
#[derive(Debug, Clone, PartialEq)]
struct Outcome {
    asked: Vec<ApprovalStage>,
    is_error: bool,
    error_code: Option<String>,
    /// `row_count` of each statement in the result.
    row_counts: Vec<u64>,
    /// `truncated` of each statement in the result.
    truncation: Vec<Option<String>>,
    /// The rows of each statement in the result (already redacted).
    rows: Vec<Value>,
    audit_tool: String,
    audit_connection: Option<String>,
    audit_arguments: Value,
    audit_approval: String,
    audit_result: String,
    audit_error_code: Option<String>,
    audit_rows: u64,
    audit_statements: Vec<AuditedStatement>,
    redaction: (Vec<String>, u32, bool, u32),
    table: Vec<Vec<String>>,
}

/// A result as the surface gave it.
struct Raw {
    is_error: bool,
    /// The result body with the untrusted envelope removed.
    body: Value,
    /// The text as the model or agent got it.
    text: String,
}

impl Raw {
    fn from_mcp(result: &Value) -> Self {
        let text = result["content"][0]["text"]
            .as_str()
            .expect("a text content")
            .to_owned();
        let inner = unwrap_untrusted(&text).unwrap_or(&text);
        Self {
            is_error: result["isError"].as_bool().unwrap_or(false),
            body: serde_json::from_str(inner).unwrap_or(Value::Null),
            text,
        }
    }

    fn code(&self) -> Option<String> {
        self.body["error"]["code"].as_str().map(str::to_owned)
    }

    fn statements(&self) -> &[Value] {
        self.body["statements"]
            .as_array()
            .map_or(&[], Vec::as_slice)
    }
}

fn outcome(
    raw: &Raw,
    code: Option<String>,
    entry: &AuditEntry,
    user: &User,
    table: Vec<Vec<String>>,
) -> Outcome {
    Outcome {
        asked: user.asked.borrow().clone(),
        is_error: raw.is_error,
        error_code: code,
        row_counts: raw
            .statements()
            .iter()
            .map(|s| s["row_count"].as_u64().unwrap_or(0))
            .collect(),
        truncation: raw
            .statements()
            .iter()
            .map(|s| s["truncated"].as_str().map(str::to_owned))
            .collect(),
        rows: raw.statements().iter().map(|s| s["rows"].clone()).collect(),
        audit_tool: entry.tool.clone(),
        audit_connection: entry.connection.as_ref().map(|c| c.id.clone()),
        audit_arguments: entry.arguments.clone(),
        audit_approval: entry.approval.as_str().to_owned(),
        audit_result: entry.outcome.as_str().to_owned(),
        audit_error_code: entry.error.as_ref().map(|e| e.code.clone()),
        audit_rows: entry.rows,
        audit_statements: entry
            .statements
            .iter()
            .map(|s| AuditedStatement {
                sql: s.sql.clone(),
                class: s.class.clone(),
                approval: s.approval.as_str().to_owned(),
                ran: s.ran,
                rows: s.rows,
                truncated: s.truncated.clone(),
                failed: s.failed,
            })
            .collect(),
        redaction: (
            entry.redaction.columns.clone(),
            entry.redaction.tainted_statements,
            entry.redaction.rows_withheld,
            entry.redaction.errors_withheld,
        ),
        table,
    }
}

// ---------------------------------------------------------------------------------------------
// The three surfaces.
// ---------------------------------------------------------------------------------------------

/// The in-app session: an `ExplorationSession` with a scripted model that issues `run_query`.
struct SessionRig {
    session: ExplorationSession,
    provider: Arc<ScriptedProvider>,
    source: Arc<TabSource>,
    calls: usize,
    /// The names the tool set lists to the model.
    listed: Vec<String>,
    /// Keeps the host channel open: a closed channel would deny every ask.
    _host: UnboundedReceiver<HostMsg>,
}

impl SessionRig {
    fn new(
        info: &ConnectionInfo,
        request: ConnectRequest,
        audit: Arc<dyn AuditSink>,
        asks: UnboundedSender<ApprovalAsk>,
        secs: u32,
        plugin: Arc<dyn ExternalTools>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut host) = unbounded_channel::<HostMsg>();
        // Host messages other than asks are dropped; asks go to the user.
        let (keep_tx, keep_rx) = unbounded_channel::<HostMsg>();
        let forward = tokio::spawn(async move {
            while let Some(msg) = host.recv().await {
                match msg {
                    HostMsg::Ask(ask) => {
                        let _ = asks.send(ask);
                    }
                    other => {
                        let _ = keep_tx.send(other);
                    }
                }
            }
        });
        let source = Arc::new(TabSource::new(info.clone(), request, Arc::new(Lite)));
        let approver = Arc::new(ChannelApprover::new(tx, HostMsg::Ask));
        let tools = AiAudit::new(audit)
            .with_external_tools(Some(plugin))
            .attach(
                ToolSet::configured(source.clone(), &config(secs)).with_approver(approver),
                Surface::Session,
            );
        let listed = tools.definitions().into_iter().map(|d| d.name).collect();
        let provider = Arc::new(
            ScriptedProvider::new("mock")
                .with_capabilities(Capabilities::new(true, false, false, None)),
        );
        let session = ExplorationSession::new(
            provider.clone(),
            tools,
            info.clone(),
            None,
            SessionConfig::new("model"),
        )
        .unwrap();
        (
            Self {
                session,
                provider,
                source,
                calls: 0,
                listed,
                _host: keep_rx,
            },
            forward,
        )
    }

    async fn run_query(&mut self, info: &ConnectionInfo, sql: &str) -> (Raw, Option<String>) {
        self.call_tool("run_query", json!({"connection": info.id, "sql": sql}))
            .await
    }

    async fn call_tool(&mut self, tool: &str, arguments: Value) -> (Raw, Option<String>) {
        self.calls += 1;
        let id = format!("call-{}", self.calls);
        let args = arguments.to_string();
        let steps: [Vec<Result<ChatEvent, AiError>>; 2] = [
            vec![
                Ok(ChatEvent::ToolCall(ToolCall::new(&id, tool, args))),
                Ok(ChatEvent::Usage(Usage::new(1, 1))),
                Ok(ChatEvent::Done {
                    stop: ModelStop::ToolUse,
                }),
            ],
            vec![
                Ok(ChatEvent::TextDelta("done".to_owned())),
                Ok(ChatEvent::Done {
                    stop: ModelStop::EndTurn,
                }),
            ],
        ];
        for step in steps {
            self.provider.push_script(step);
        }
        let turn = self.session.send("go").await;
        assert!(matches!(turn, TurnOutcome::Completed), "{turn:?}");
        let entry = self
            .session
            .transcript()
            .entries()
            .iter()
            .find_map(|e| match e {
                TranscriptEntry::ToolCall(c) if c.call_id == id => Some(c.clone()),
                _ => None,
            })
            .expect("the tool call is in the transcript");
        let (is_error, code) = match &entry.status {
            ToolCallStatus::Ok => (false, None),
            ToolCallStatus::Error { code, .. } => (true, Some(code.clone())),
            other => panic!("the call did not run: {other:?}"),
        };
        let text = entry.result.to_string();
        (
            Raw {
                is_error,
                body: entry.result,
                text,
            },
            code,
        )
    }
}

/// The MCP server over HTTP, in this process.
struct McpRig {
    client: McpClient,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl McpRig {
    async fn new(
        info: &ConnectionInfo,
        request: ConnectRequest,
        audit: Arc<dyn AuditSink>,
        asks: UnboundedSender<ApprovalAsk>,
        secs: u32,
        plugin: Arc<dyn ExternalTools>,
    ) -> Self {
        let source = Arc::new(TabSource::new(info.clone(), request, Arc::new(Lite)));
        let approver = Arc::new(ChannelApprover::new(asks, std::convert::identity));
        let tools = AiAudit::new(audit.clone())
            .with_external_tools(Some(plugin))
            .external(ToolSet::configured(source, &config(secs)).with_approver(approver));
        let server = McpServer::new(tools, audit, ServerOptions::default());
        let token = AuthToken::generate().unwrap();
        let auth = format!("Bearer {}", token.expose());
        let http = server
            .bind_http("127.0.0.1:0".parse().unwrap(), token)
            .await
            .unwrap();
        let addr = http.local_addr();
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = http
                .run(async move {
                    let _ = stopped.await;
                })
                .await;
        });
        Self {
            client: McpClient { addr, auth },
            stop: Some(stop),
            task: Some(task),
        }
    }

    async fn close(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            guard("the MCP server stops", task).await.unwrap();
        }
    }
}

/// What the pump of an agent's host channel passes on.
enum Pumped {
    Text(String),
    Permission(PermissionAsk),
    Done(Result<StopReason, String>),
}

/// An audit sink that records in memory and in a file, so the token can be searched for.
struct Tee {
    memory: Arc<MemoryAudit>,
    file: AuditLog,
}

impl AuditSink for Tee {
    fn record(&self, entry: &AuditEntry) {
        self.memory.record(entry);
        self.file.record(entry);
    }
}

/// A started mock agent and the endpoint it was told about.
struct AcpRig {
    control: sqler_ui::ai::agent::AgentControl,
    finished: Option<oneshot::Receiver<ExitInfo>>,
    pid: Option<u32>,
    pumped: UnboundedReceiver<Pumped>,
    pump: Option<JoinHandle<()>>,
    client: McpClient,
    /// The token as the descriptor carries it (`Bearer ...`).
    auth: String,
    url: String,
    audit_file: PathBuf,
    /// The `Debug` text of everything the host saw.
    log: Arc<Mutex<String>>,
    turn: u64,
}

impl AcpRig {
    async fn new(
        info: &ConnectionInfo,
        request: ConnectRequest,
        audit: Arc<dyn AuditSink>,
        audit_file: PathBuf,
        cwd: PathBuf,
        asks: UnboundedSender<ApprovalAsk>,
        secs: u32,
        plugin: Arc<dyn ExternalTools>,
    ) -> Self {
        let (tx, mut host) = unbounded_channel::<AgentMsg>();
        let launch = LaunchOptions {
            cancel_grace: Duration::from_millis(200),
            ..LaunchOptions::default()
        };
        let start = AgentStart {
            tab: a_tab(),
            profile: mock_profile(),
            info: info.clone(),
            request,
            config: config(secs),
            audit: AiAudit::new(audit).with_external_tools(Some(plugin)),
            cwd,
            launch,
            tx,
            abort: AgentAbort::never(),
        };
        let log = Arc::new(Mutex::new(format!("{start:?}\n")));
        let started = guard("the agent starts", start_agent(start, Arc::new(Lite)))
            .await
            .unwrap_or_else(|failure| panic!("the agent did not start: {failure:?}"));
        writeln!(
            log.lock().unwrap_or_else(PoisonError::into_inner),
            "{started:?}\n{:?}",
            started.ready
        )
        .unwrap();
        let (out, pumped) = unbounded_channel::<Pumped>();
        let seen = Arc::clone(&log);
        let pump = tokio::spawn(async move {
            while let Some(msg) = host.recv().await {
                writeln!(
                    seen.lock().unwrap_or_else(PoisonError::into_inner),
                    "{msg:?}"
                )
                .unwrap();
                match msg {
                    AgentMsg::Ask(ask) => {
                        let _ = asks.send(ask);
                    }
                    AgentMsg::Permission(ask) => {
                        let _ = out.send(Pumped::Permission(ask));
                    }
                    AgentMsg::PromptDone { result, .. } => {
                        let _ = out.send(Pumped::Done(result));
                    }
                    AgentMsg::Event(AgentEvent::Update {
                        update: SessionUpdate::AgentMessage(text),
                        ..
                    }) => {
                        let _ = out.send(Pumped::Text(text));
                    }
                    AgentMsg::Event(_) => {}
                }
            }
        });
        let mut rig = Self {
            pid: started.ready.control.pid(),
            control: started.ready.control,
            finished: Some(started.finished),
            pumped,
            pump: Some(pump),
            client: McpClient {
                addr: "127.0.0.1:1".parse().unwrap(),
                auth: String::new(),
            },
            auth: String::new(),
            url: started.ready.endpoint_url,
            audit_file,
            log,
            turn: 0,
        };
        // The URL and the token are the ones the agent got in `session/new`.
        let (texts, done) = rig.prompt("descriptor").await;
        assert_eq!(done, Ok(StopReason::EndTurn));
        let servers: Value = serde_json::from_str(texts.last().expect("a descriptor")).unwrap();
        let server = &servers[0];
        assert_eq!(server["name"], "sqler");
        let url = server["url"].as_str().unwrap();
        assert_eq!(url, rig.url);
        let header = server["headers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "Authorization")
            .expect("the Authorization header");
        header["value"].as_str().unwrap().clone_into(&mut rig.auth);
        let hostport = url
            .strip_prefix("http://")
            .and_then(|rest| rest.strip_suffix("/mcp"))
            .expect("an http URL ending in /mcp");
        rig.client = McpClient {
            addr: hostport.parse().unwrap(),
            auth: rig.auth.clone(),
        };
        rig
    }

    async fn next(&mut self) -> Pumped {
        guard("the agent said something", self.pumped.recv())
            .await
            .expect("the host channel ended")
    }

    /// Sends a prompt and waits for its end; returns what the agent said.
    async fn prompt(&mut self, text: &str) -> (Vec<String>, Result<StopReason, String>) {
        self.turn += 1;
        self.control.prompt(self.turn, text.to_owned());
        let mut said = Vec::new();
        loop {
            match self.next().await {
                Pumped::Text(t) => said.push(t),
                Pumped::Done(result) => return (said, result),
                Pumped::Permission(_) => panic!("an unexpected permission request"),
            }
        }
    }

    /// Closes the agent through its control; checks it was reaped and the host channel ended.
    async fn close(&mut self) {
        self.control.close();
        let finished = self.finished.take().expect("not closed twice");
        guard("the agent was torn down", finished)
            .await
            .expect("finished resolves");
        if let Some(pid) = self.pid {
            assert!(!alive(pid), "the agent process {pid} was reaped");
        }
        if let Some(pump) = self.pump.take() {
            guard("the host channel ends", pump).await.unwrap();
        }
        // The endpoint is gone with the agent.
        assert!(TcpStream::connect(self.client.addr).await.is_err());
    }

    fn token_canary(&self) -> &str {
        self.auth.strip_prefix("Bearer ").expect("a bearer token")
    }

    fn log(&self) -> String {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// One surface with its own database, audit and asks.
enum Inner {
    Session(Box<SessionRig>, JoinHandle<()>),
    Mcp(McpRig),
    Acp(Box<AcpRig>),
}

struct Rig {
    surface: Surface,
    info: ConnectionInfo,
    inner: Inner,
    asks: UnboundedReceiver<ApprovalAsk>,
    audit: Arc<MemoryAudit>,
    db: String,
    calls: usize,
    env: Environment,
    /// The time limit of a tool call, in seconds.
    secs: u32,
    /// The database, the audit file and the agent folder.
    scratch: Scratch,
}

impl Rig {
    async fn new(surface: Surface, env: Environment) -> Self {
        Self::with_time_limit(surface, env, CALM_LIMIT_SECS).await
    }

    /// A rig whose tool calls stop after `secs` seconds.
    async fn with_time_limit(surface: Surface, env: Environment, secs: u32) -> Self {
        let scratch = Scratch::new(surface.as_str());
        seed(&scratch.db()).await;
        let dialect = Dialect::from_driver_id(driver().as_str());
        Self::build(surface, env, scratch, secs, dialect).await
    }

    /// A rig whose connection declares `dialect`, as a plugin driver does through its manifest.
    async fn with_dialect(surface: Surface, env: Environment, dialect: Dialect) -> Self {
        let scratch = Scratch::new(surface.as_str());
        seed(&scratch.db()).await;
        Self::build(surface, env, scratch, CALM_LIMIT_SECS, Some(dialect)).await
    }

    /// A rig on the database in `scratch`.
    async fn build(
        surface: Surface,
        env: Environment,
        scratch: Scratch,
        secs: u32,
        dialect: Option<Dialect>,
    ) -> Self {
        let db = scratch.db();
        let (info, request) = connection(env, &db);
        let info = info.with_dialect(dialect);
        let (asks_tx, asks) = unbounded_channel();
        let audit = Arc::new(MemoryAudit::new());
        let plugin: Arc<dyn ExternalTools> = Arc::new(NotePlugin {
            db: db.clone(),
            env,
        });
        let inner = match surface {
            Surface::Session => {
                let (rig, forward) =
                    SessionRig::new(&info, request, audit.clone(), asks_tx, secs, plugin);
                Inner::Session(Box::new(rig), forward)
            }
            Surface::Mcp => {
                Inner::Mcp(McpRig::new(&info, request, audit.clone(), asks_tx, secs, plugin).await)
            }
            Surface::Agent => {
                let file = scratch.0.join("ai-audit.jsonl");
                let sink = Arc::new(Tee {
                    memory: audit.clone(),
                    file: AuditLog::open_at(&file, AuditOptions::default()),
                });
                let cwd = scratch.0.join("agent-cwd");
                Inner::Acp(Box::new(
                    AcpRig::new(&info, request, sink, file, cwd, asks_tx, secs, plugin).await,
                ))
            }
            other => panic!("not a surface of the suite: {other:?}"),
        };
        Self {
            surface,
            info,
            inner,
            asks,
            audit,
            db,
            calls: 0,
            env,
            secs,
            scratch,
        }
    }

    /// Closes this rig and starts a new one (a new agent or server, new sessions, a new audit
    /// log) on the same database file.
    async fn restart(self) -> Self {
        let (surface, env, secs) = (self.surface, self.env, self.secs);
        let dialect = self.info.dialect;
        let scratch = self.close().await;
        Self::build(surface, env, scratch, secs, dialect).await
    }

    /// Calls `run_query` with the user's scripted answers and normalises what happened.
    async fn call(&mut self, sql: &str, decisions: &[Decision]) -> Outcome {
        let args = json!({"connection": self.info.id, "sql": sql});
        self.call_tool("run_query", args, sql, decisions).await
    }

    /// Calls `tool` with the user's scripted answers; every ask must carry `sql`.
    async fn call_tool(
        &mut self,
        tool: &str,
        args: Value,
        sql: &str,
        decisions: &[Decision],
    ) -> Outcome {
        let mut user = User::new(sql, decisions);
        user.dialect = self.info.dialect;
        let (raw, code) = {
            let Self { inner, asks, .. } = self;
            let call = async {
                match inner {
                    Inner::Session(rig, _) => rig.call_tool(tool, args).await,
                    Inner::Mcp(rig) => {
                        let raw = Raw::from_mcp(&rig.client.call_tool(tool, args).await);
                        let code = raw.code();
                        (raw, code)
                    }
                    Inner::Acp(rig) => {
                        let raw = Raw::from_mcp(&rig.client.call_tool(tool, args).await);
                        let code = raw.code();
                        (raw, code)
                    }
                }
            };
            tokio::select! {
                result = guard("the call finished", call) => result,
                never = user.serve(asks) => match never {},
            }
        };
        assert!(
            user.decisions.is_empty(),
            "every scripted answer was used: {}",
            raw.text
        );
        assert!(user.queue.is_empty(), "no ask is left waiting");
        assert!(
            self.asks.try_recv().is_err(),
            "no ask is left in the channel"
        );
        assert_eq!(
            *user.gates.borrow(),
            user.asked
                .borrow()
                .iter()
                .filter(|s| **s == ApprovalStage::ProdSecond)
                .count(),
            "the gate showed every second confirmation"
        );
        // S8: one audit entry per call, on this surface.
        self.calls += 1;
        let entries = self.audit.entries();
        assert_eq!(entries.len(), self.calls, "one audit entry per call");
        let entry = entries.last().expect("an entry");
        assert_eq!(entry.surface, self.surface, "the audit surface");
        let table = table(&self.db).await;
        outcome(&raw, code, entry, &user, table)
    }

    /// The tool names the surface lists, in order.
    async fn listed(&self) -> Vec<String> {
        match &self.inner {
            Inner::Session(rig, _) => rig.listed.clone(),
            Inner::Mcp(rig) => rig.client.tool_names().await,
            Inner::Acp(rig) => rig.client.tool_names().await,
        }
    }

    /// The text of the last result, for the checks on what the model or agent saw.
    async fn call_raw_text(&mut self, sql: &str) -> String {
        match &mut self.inner {
            Inner::Session(rig, _) => rig.run_query(&self.info, sql).await.0.text,
            Inner::Mcp(rig) => Raw::from_mcp(&rig.client.run_query(&self.info.id, sql).await).text,
            Inner::Acp(rig) => Raw::from_mcp(&rig.client.run_query(&self.info.id, sql).await).text,
        }
    }

    async fn close(mut self) -> Scratch {
        match &mut self.inner {
            Inner::Session(rig, forward) => {
                // Nothing is lent out: the idle session is closed.
                if let Some(session) = rig.source.shut() {
                    let _ = session.close().await;
                }
                forward.abort();
            }
            Inner::Mcp(rig) => rig.close().await,
            Inner::Acp(rig) => {
                rig.close().await;
                let token = rig.token_canary().to_owned();
                let file = std::fs::read_to_string(&rig.audit_file).unwrap_or_default();
                assert!(!file.contains(&token), "the token is in the audit file");
                assert!(!rig.log().contains(&token), "the token is in a Debug text");
            }
        }
        self.scratch
    }
}

// ---------------------------------------------------------------------------------------------
// Scenarios.
// ---------------------------------------------------------------------------------------------

const APPROVE: Decision = Decision::Approve;
const DENY: Decision = Decision::Deny;

/// A scenario: the connection, the SQL and what the user answers.
#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    env: Environment,
    sql: &'static str,
    user: &'static [Decision],
    /// The time limit of the call in seconds.
    secs: u32,
}

const S1: Scenario = Scenario {
    name: "S1 read ok",
    env: Environment::Dev,
    sql: READ,
    user: &[],
    secs: CALM_LIMIT_SECS,
};
const S2: Scenario = Scenario {
    name: "S2 write needs approval",
    env: Environment::Dev,
    sql: WRITE,
    user: &[APPROVE],
    secs: CALM_LIMIT_SECS,
};
const S3: Scenario = Scenario {
    name: "S3 deny",
    env: Environment::Dev,
    sql: DELETE,
    user: &[DENY],
    secs: CALM_LIMIT_SECS,
};
const S4: Scenario = Scenario {
    name: "S4 prod double confirm",
    env: Environment::Prod,
    sql: PROD_WRITE,
    user: &[APPROVE, APPROVE],
    secs: CALM_LIMIT_SECS,
};
const S4B: Scenario = Scenario {
    name: "S4b prod second refused",
    env: Environment::Prod,
    sql: PROD_WRITE,
    user: &[APPROVE, DENY],
    secs: CALM_LIMIT_SECS,
};
const S5: Scenario = Scenario {
    name: "S5 row limit",
    env: Environment::Dev,
    sql: TO_NINE,
    user: &[],
    secs: CALM_LIMIT_SECS,
};
const S6: Scenario = Scenario {
    name: "S6 time limit",
    env: Environment::Dev,
    sql: ENDLESS,
    user: &[],
    secs: SHORT_LIMIT_SECS,
};
const S7: Scenario = Scenario {
    name: "S7 redaction",
    env: Environment::Dev,
    sql: SECRETS,
    user: &[],
    secs: CALM_LIMIT_SECS,
};
const ALL: [Scenario; 8] = [S1, S2, S3, S4, S4B, S5, S6, S7];

/// Runs the scenario on every surface (a fresh database and agent each) and returns the
/// outcomes, in [`SURFACES`] order. The rigs are closed at the end.
async fn on_all(scn: Scenario) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    for surface in SURFACES {
        let mut rig = Rig::with_time_limit(surface, scn.env, scn.secs).await;
        let out = rig.call(scn.sql, scn.user).await;
        rig.close().await;
        outcomes.push(out);
    }
    outcomes
}

/// Every surface gave the same normalised outcome; returns it.
fn identical(scn: Scenario, outcomes: &[Outcome]) -> Outcome {
    let first = outcomes[0].clone();
    for (surface, out) in SURFACES.iter().zip(outcomes) {
        assert_eq!(
            out, &first,
            "{}: {surface:?} differs from the session",
            scn.name
        );
    }
    first
}

fn runtime() -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn run<T>(fut: impl Future<Output = T>) -> T {
    let rt = runtime();
    let out = rt.block_on(fut);
    // A statement the driver cannot interrupt may still run in a worker thread; do not wait for it.
    rt.shutdown_background();
    out
}

fn text(cell: &str) -> &str {
    cell.strip_prefix("Text(\"")
        .and_then(|rest| rest.strip_suffix("\")"))
        .unwrap_or(cell)
}

fn names(out: &Outcome) -> Vec<&str> {
    out.table.iter().map(|row| text(&row[1])).collect()
}

fn statement_sql(out: &Outcome) -> Vec<&str> {
    out.audit_statements
        .iter()
        .map(|s| s.sql.as_str())
        .collect()
}

#[test]
fn s1_a_read_runs_without_asking() {
    let out = identical(S1, &run(on_all(S1)));
    assert_eq!(out.asked, []);
    assert!(!out.is_error);
    assert_eq!(out.row_counts, [3]);
    assert_eq!(out.truncation, [None]);
    assert_eq!(out.audit_approval, "not_needed");
    assert_eq!(out.audit_result, "ok");
    assert_eq!(statement_sql(&out), [READ]);
    assert!(out.audit_statements[0].ran);
    assert_eq!(out.audit_statements[0].class, "read");
    assert_eq!(names(&out), ["ann", "bob", "cy"]);
}

#[test]
fn s2_a_write_runs_after_one_approval() {
    let out = identical(S2, &run(on_all(S2)));
    assert_eq!(out.asked, [ApprovalStage::Confirm]);
    assert!(!out.is_error);
    assert_eq!(out.audit_approval, "approved");
    assert_eq!(out.audit_result, "ok");
    assert!(out.audit_statements[0].ran);
    assert_eq!(out.audit_statements[0].approval, "approved");
    assert_eq!(out.audit_statements[0].class, "write");
    assert_eq!(names(&out), ["zed", "bob", "cy"]);
}

#[test]
fn s3_a_denied_write_does_not_run() {
    let out = identical(S3, &run(on_all(S3)));
    assert_eq!(out.asked, [ApprovalStage::Confirm]);
    assert!(out.is_error);
    assert_eq!(out.error_code.as_deref(), Some("denied"));
    assert_eq!(out.audit_approval, "denied");
    assert_eq!(out.audit_result, "error");
    assert!(out.audit_statements.iter().all(|s| !s.ran));
    assert_eq!(names(&out), ["ann", "bob", "cy"]);
}

#[test]
fn s4_a_prod_write_needs_two_confirmations() {
    let out = identical(S4, &run(on_all(S4)));
    assert_eq!(
        out.asked,
        [ApprovalStage::Confirm, ApprovalStage::ProdSecond]
    );
    assert!(!out.is_error);
    assert_eq!(out.audit_approval, "approved");
    assert_eq!(out.audit_result, "ok");
    assert!(out.audit_statements[0].ran);
    assert_eq!(names(&out), ["p", "bob", "cy"]);
}

#[test]
fn s4b_a_refused_second_confirmation_stops_the_write() {
    let out = identical(S4B, &run(on_all(S4B)));
    assert_eq!(
        out.asked,
        [ApprovalStage::Confirm, ApprovalStage::ProdSecond]
    );
    assert!(out.is_error);
    assert_eq!(out.error_code.as_deref(), Some("denied"));
    assert_eq!(out.audit_approval, "denied");
    assert!(out.audit_statements.iter().all(|s| !s.ran));
    assert_eq!(names(&out), ["ann", "bob", "cy"]);
}

#[test]
fn s5_the_row_limit_truncates_without_failing() {
    let out = identical(S5, &run(on_all(S5)));
    assert_eq!(out.asked, []);
    assert!(!out.is_error);
    assert_eq!(out.row_counts, [5]);
    assert_eq!(out.truncation, [Some("row_limit".to_owned())]);
    assert_eq!(
        out.audit_statements[0].truncated.as_deref(),
        Some("row_limit")
    );
    assert_eq!(out.audit_statements[0].rows, Some(5));
}

#[test]
fn s6_the_time_limit_stops_the_query_and_the_database_stays_usable() {
    let (outcomes, again) = run(async {
        let mut outcomes = Vec::new();
        let mut again = Vec::new();
        for surface in SURFACES {
            let mut rig = Rig::with_time_limit(surface, Environment::Dev, S6.secs).await;
            outcomes.push(rig.call(S6.sql, S6.user).await);
            // The `SQLite` driver stops a statement only between rows, so the session of the
            // call may still be busy. A new server or agent on the same file reads normally.
            let mut rig = rig.restart().await;
            again.push(rig.call(S1.sql, S1.user).await);
            rig.close().await;
        }
        (outcomes, again)
    });
    let out = identical(S6, &outcomes);
    assert_eq!(out.asked, []);
    assert!(
        out.audit_statements[0].ran,
        "the statement ran before it was stopped"
    );
    assert_eq!(
        out.audit_statements[0].truncated.as_deref(),
        Some("time_limit"),
        "{out:?}"
    );
    assert_eq!(out.truncation, [Some("time_limit".to_owned())]);
    let next = identical(S1, &again);
    assert!(!next.is_error);
    assert_eq!(next.row_counts, [3], "{next:?}");
    assert_eq!(next.audit_approval, "not_needed");
}

#[test]
fn s7_redacted_columns_never_leave_the_tool_set() {
    let out = identical(S7, &run(on_all(S7)));
    assert_eq!(out.asked, []);
    assert!(!out.is_error);
    // The SQL names a listed word, so the whole statement is tainted: every value is hidden.
    assert_eq!(out.redaction.1, 1, "{out:?}");
    assert!(!out.redaction.0.is_empty(), "{out:?}");
    assert_eq!(out.row_counts, [3]);
    assert!(
        out.rows[0]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .all(|cell| cell == "<redacted>")
    );
    let shown = out.rows.iter().map(Value::to_string).collect::<String>();
    assert!(!shown.contains("pw-canary"), "{shown}");
    // What the model or agent got, byte for byte, holds no canary either.
    let texts = run(async {
        let mut texts = Vec::new();
        for surface in SURFACES {
            let mut rig = Rig::new(surface, Environment::Dev).await;
            texts.push(rig.call_raw_text(S7.sql).await);
            rig.close().await;
        }
        texts
    });
    for (surface, text) in SURFACES.iter().zip(&texts) {
        assert!(
            !text.contains("pw-canary"),
            "{surface:?} leaked a password: {text}"
        );
    }
}

#[test]
fn s8_every_call_leaves_one_equal_audit_entry() {
    run(async {
        for scn in ALL {
            // `on_all` asserts one entry per call and the surface of each entry; the normalised
            // entries (everything but the surface, id, time and size) must be equal.
            let outcomes = on_all(scn).await;
            let first = &outcomes[0];
            for out in &outcomes[1..] {
                assert_eq!(out.audit_tool, first.audit_tool, "{}", scn.name);
                assert_eq!(out.audit_connection, first.audit_connection, "{}", scn.name);
                assert_eq!(out.audit_arguments, first.audit_arguments, "{}", scn.name);
                assert_eq!(out.audit_approval, first.audit_approval, "{}", scn.name);
                assert_eq!(out.audit_result, first.audit_result, "{}", scn.name);
                assert_eq!(out.audit_statements, first.audit_statements, "{}", scn.name);
                assert_eq!(out.redaction, first.redaction, "{}", scn.name);
            }
            assert_eq!(first.audit_tool, "run_query");
        }
    });
}

// ---------------------------------------------------------------------------------------------
// A connection that declares the T-SQL dialect (a plugin driver such as `sqlserver`, ADR-0073).
// The database is still the SQLite file: the statements below are only asked about, or denied.
// ---------------------------------------------------------------------------------------------

/// A read in every dialect.
const TSQL_READ: &str = "select id, name from users order by id";
/// A locking hint: the T-SQL classifier files it as a write (a plain `nolock` stays a read).
const TSQL_HINT: &str = "select id from users with (updlock)";
/// A stored procedure call: never a read.
const TSQL_EXEC: &str = "exec sp_who";

/// Runs one call on every surface on a connection with the T-SQL dialect.
async fn tsql_on_all(env: Environment, sql: &str, user: &[Decision]) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    for surface in SURFACES {
        let mut rig = Rig::with_dialect(surface, env, Dialect::TSql).await;
        outcomes.push(rig.call(sql, user).await);
        rig.close().await;
    }
    outcomes
}

#[test]
fn t1_a_tsql_read_runs_without_asking() {
    let outcomes = run(tsql_on_all(Environment::Prod, TSQL_READ, &[]));
    let out = same_everywhere(&outcomes);
    assert_eq!(out.asked, []);
    assert!(!out.is_error);
    assert_eq!(out.audit_approval, "not_needed");
    assert_eq!(out.audit_statements[0].class, "read");
    assert_eq!(names(&out), ["ann", "bob", "cy"]);
}

#[test]
fn t2_a_tsql_write_needs_approval_and_a_denial_stops_it() {
    for sql in [TSQL_HINT, TSQL_EXEC] {
        let out = same_everywhere(&run(tsql_on_all(Environment::Dev, sql, &[DENY])));
        assert_eq!(out.asked, [ApprovalStage::Confirm], "{sql}");
        assert!(out.is_error, "{sql}");
        assert_eq!(out.error_code.as_deref(), Some("denied"), "{sql}");
        assert!(out.audit_statements.iter().all(|s| !s.ran), "{sql}");
        assert_eq!(names(&out), ["ann", "bob", "cy"], "{sql}");
    }
}

#[test]
fn t3_a_tsql_statement_on_prod_asks_twice_and_the_gate_shows_it() {
    for sql in [TSQL_HINT, TSQL_EXEC] {
        let user = [APPROVE, DENY];
        let out = same_everywhere(&run(tsql_on_all(Environment::Prod, sql, &user)));
        assert_eq!(
            out.asked,
            [ApprovalStage::Confirm, ApprovalStage::ProdSecond],
            "{sql}"
        );
        assert_eq!(out.error_code.as_deref(), Some("denied"), "{sql}");
        assert!(out.audit_statements.iter().all(|s| !s.ran), "{sql}");
        assert_eq!(names(&out), ["ann", "bob", "cy"], "{sql}");
    }
}

// ---------------------------------------------------------------------------------------------
// Plugin tools (ADR-0062): the same guards on all three surfaces.
// ---------------------------------------------------------------------------------------------

/// Runs the plugin tool on every surface and returns the outcomes in [`SURFACES`] order.
async fn note_on_all(env: Environment, user: &'static [Decision]) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    for surface in SURFACES {
        let mut rig = Rig::new(surface, env).await;
        outcomes.push(rig.call_tool(NOTE_TOOL, json!({}), NOTE, user).await);
        rig.close().await;
    }
    outcomes
}

fn same_everywhere(outcomes: &[Outcome]) -> Outcome {
    let first = outcomes[0].clone();
    for (surface, out) in SURFACES.iter().zip(outcomes) {
        assert_eq!(out, &first, "{surface:?} differs from the session");
    }
    first
}

#[test]
fn x1_every_surface_lists_the_plugin_tool_after_the_built_ins() {
    let listed = run(async {
        let mut listed = Vec::new();
        for surface in SURFACES {
            let rig = Rig::new(surface, Environment::Dev).await;
            listed.push(rig.listed().await);
            rig.close().await;
        }
        listed
    });
    for (surface, names) in SURFACES.iter().zip(&listed) {
        assert_eq!(
            names.last().map(String::as_str),
            Some(NOTE_TOOL),
            "{surface:?}"
        );
        assert!(names.iter().any(|n| n == "run_query"), "{surface:?}");
        assert_eq!(
            names.iter().filter(|n| *n == NOTE_TOOL).count(),
            1,
            "{surface:?}"
        );
    }
}

#[test]
fn x2_an_approved_plugin_write_runs_and_is_audited() {
    let out = same_everywhere(&run(note_on_all(Environment::Dev, &[APPROVE])));
    assert_eq!(out.asked, [ApprovalStage::Confirm]);
    assert!(!out.is_error, "{out:?}");
    assert_eq!(out.audit_tool, NOTE_TOOL);
    assert_eq!(out.audit_approval, "approved");
    assert_eq!(out.audit_result, "ok");
    assert_eq!(statement_sql(&out), [NOTE]);
    assert!(out.audit_statements[0].ran);
    assert_eq!(names(&out), ["ann", "bob", "cy", "note"]);
}

#[test]
fn x3_a_denied_plugin_write_does_not_run_and_is_audited() {
    let out = same_everywhere(&run(note_on_all(Environment::Dev, &[DENY])));
    assert_eq!(out.asked, [ApprovalStage::Confirm]);
    assert!(out.is_error);
    assert_eq!(out.error_code.as_deref(), Some("denied"));
    assert_eq!(out.audit_tool, NOTE_TOOL);
    assert_eq!(out.audit_approval, "denied");
    assert_eq!(out.audit_result, "error");
    assert!(out.audit_statements.iter().all(|s| !s.ran));
    assert_eq!(names(&out), ["ann", "bob", "cy"]);
}

#[test]
fn x4_a_prod_plugin_write_needs_two_confirmations() {
    let out = same_everywhere(&run(note_on_all(Environment::Prod, &[APPROVE, APPROVE])));
    assert_eq!(
        out.asked,
        [ApprovalStage::Confirm, ApprovalStage::ProdSecond]
    );
    assert!(!out.is_error, "{out:?}");
    assert_eq!(out.audit_approval, "approved");
    assert_eq!(names(&out), ["ann", "bob", "cy", "note"]);
}

// ---------------------------------------------------------------------------------------------
// ACP only.
// ---------------------------------------------------------------------------------------------

async fn acp_rig() -> Rig {
    let rig = Rig::new(Surface::Agent, Environment::Dev).await;
    assert!(matches!(rig.inner, Inner::Acp(_)));
    rig
}

fn acp(rig: &mut Rig) -> &mut AcpRig {
    match &mut rig.inner {
        Inner::Acp(acp) => acp,
        _ => unreachable!("an ACP rig"),
    }
}

/// Asks the agent for a permission, lets `answer` handle the request and returns what the agent
/// said at the end of the turn.
async fn permission_round(
    rig: &mut AcpRig,
    answer: impl FnOnce(&mut PermissionQueue, u64, &AcpRig),
) -> (Vec<String>, Result<StopReason, String>) {
    rig.turn += 1;
    rig.control.prompt(rig.turn, "permission".to_owned());
    let mut queue = PermissionQueue::default();
    let mut said = Vec::new();
    let mut answer = Some(answer);
    let mut answered = false;
    loop {
        match rig.next().await {
            Pumped::Text(t) => said.push(t),
            Pumped::Permission(ask) => {
                let options: Vec<String> =
                    ask.request.options.iter().map(|o| o.id.clone()).collect();
                assert_eq!(
                    options,
                    ["allow-once", "allow-always", "reject-once", "reject-always"]
                );
                let id = queue.push(ask).expect("room in the queue");
                assert_eq!(queue.len(), 1);
                (answer.take().expect("one permission request"))(&mut queue, id, rig);
                answered = true;
            }
            Pumped::Done(result) => {
                assert!(answered, "the agent asked for permission");
                assert!(queue.is_empty(), "the permission queue is empty");
                return (said, result);
            }
        }
    }
}

#[test]
fn p1_a_permission_request_can_be_allowed_once() {
    run(async {
        let mut rig = acp_rig().await;
        let (said, done) = permission_round(acp(&mut rig), |queue, id, _| {
            // An option that was not offered answers nothing.
            assert!(!queue.answer_id(id, "allow-forever"));
            assert_eq!(queue.len(), 1);
            assert!(queue.answer_id(id, "allow-once"));
            // A stale answer changes nothing.
            assert!(!queue.answer_id(id, "reject-once"));
        })
        .await;
        assert_eq!(done, Ok(StopReason::EndTurn));
        assert_eq!(said.last().map(String::as_str), Some("selected:allow-once"));
        rig.close().await;
    });
}

#[test]
fn p2_a_permission_request_can_be_rejected_once() {
    run(async {
        let mut rig = acp_rig().await;
        let (said, done) = permission_round(acp(&mut rig), |queue, id, _| {
            assert!(queue.answer_id(id, "reject-once"));
        })
        .await;
        assert_eq!(done, Ok(StopReason::EndTurn));
        assert_eq!(
            said.last().map(String::as_str),
            Some("selected:reject-once")
        );
        rig.close().await;
    });
}

#[test]
fn p3_cancelling_a_pending_permission_cancels_the_request() {
    run(async {
        let mut rig = acp_rig().await;
        let (said, done) = permission_round(acp(&mut rig), |queue, _id, rig| {
            rig.control.cancel();
            queue.cancel_all();
        })
        .await;
        assert!(done.is_ok(), "{done:?}");
        assert_eq!(said.last().map(String::as_str), Some("cancelled"));
        // A cancelled turn keeps the process.
        if let Some(pid) = acp(&mut rig).pid {
            assert!(alive(pid), "the agent outlives the cancelled turn");
        }
        rig.close().await;
    });
}

#[test]
fn p4_the_descriptor_is_a_loopback_url_with_a_bearer_token_that_never_leaks() {
    run(async {
        let mut rig = acp_rig().await;
        let (url, auth, canary) = {
            let acp = acp(&mut rig);
            (
                acp.url.clone(),
                acp.auth.clone(),
                acp.token_canary().to_owned(),
            )
        };
        let port = url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.strip_suffix("/mcp"))
            .expect("http://127.0.0.1:<port>/mcp");
        assert!(port.parse::<u16>().is_ok_and(|p| p != 0), "{url}");
        let token = auth.strip_prefix("Bearer ").expect("Authorization: Bearer");
        assert_eq!(token.chars().count(), 43, "{auth}");
        // The endpoint refuses a call with no token or a wrong one.
        let addr = acp(&mut rig).client.addr;
        for bad in [String::new(), "Bearer wrong".to_owned()] {
            let client = McpClient { addr, auth: bad };
            let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "guard-suite", "version": "0"}}});
            let reply = client.post(None, &init).await;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
        }
        // A call with the token works, so the file below has an entry to search.
        let out = rig.call(READ, &[]).await;
        assert!(!out.is_error);
        let log = acp(&mut rig).log();
        // The agent echoed its descriptor, so the token is in what the agent *said*; none of the
        // `Debug` texts of the host (start, ready, messages) carries it.
        assert!(
            !log.contains(&canary),
            "the token is in a Debug text:\n{log}"
        );
        assert!(!format!("{:?}", rig.audit.entries()).contains(&canary));
        let file = acp(&mut rig).audit_file.clone();
        // Read before the close: the scratch folder goes with the rig.
        let written = std::fs::read_to_string(&file).unwrap();
        rig.close().await;
        assert!(written.contains("run_query"), "the audit file has the call");
        assert!(!written.contains(&canary), "the token is in the audit file");
    });
}
