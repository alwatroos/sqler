// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A mock `ACP` agent for the tests of `sqler-acp`. It speaks the agent side of the protocol on stdio.
//!
//! Flags: `--no-mcp-http` (do not advertise HTTP `MCP`) and `--crash-on-init` (exit with code 4 before
//! answering `initialize`). Other arguments are ignored. The prompt text selects the behaviour:
//!
//! - `echo`: two message chunks, a plan, a tool call, then `end_turn`.
//! - `permission`: a tool call and a permission request with the four option kinds, then a message
//!   `selected:<id>` or `cancelled`.
//! - `crash`: writes `mock: crashing` to stderr and exits with code 3.
//! - `hang`: waits for a cancel, then answers `cancelled`.
//! - `hang-deaf`: never answers and ignores a cancel.
//! - `env`: a message with a JSON object `{"args": [...], "env": {...}}` of the agent's own arguments
//!   (without the program name) and environment.
//! - `descriptor`: a message with the `mcpServers` of the last `session/new`, as JSON.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
    InitializeRequest, InitializeResponse, McpCapabilities, NewSessionRequest, NewSessionResponse,
    PermissionOption, PermissionOptionKind, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
    PromptRequest, PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionId,
    SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};
use tokio::sync::Notify;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Default)]
struct State {
    no_mcp_http: bool,
    crash_on_init: bool,
    cancelled: AtomicBool,
    cancel: Notify,
    sessions: AtomicU32,
    last_mcp: Mutex<serde_json::Value>,
}

fn message(
    cx: &ConnectionTo<Client>,
    session: &SessionId,
    text: &str,
) -> Result<(), agent_client_protocol::Error> {
    cx.send_notification(SessionNotification::new(
        session.clone(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text,
        )))),
    ))
}

async fn wait_for_cancel(state: &State) {
    while !state.cancelled.load(Ordering::SeqCst) {
        state.cancel.notified().await;
    }
}

#[allow(clippy::too_many_lines)] // one arm per scripted behaviour
async fn run_prompt(
    cx: ConnectionTo<Client>,
    state: Arc<State>,
    session: SessionId,
    text: String,
) -> Result<StopReason, agent_client_protocol::Error> {
    state.cancelled.store(false, Ordering::SeqCst);
    match text.trim() {
        "echo" => {
            message(&cx, &session, "Hello")?;
            message(&cx, &session, " world")?;
            cx.send_notification(SessionNotification::new(
                session.clone(),
                SessionUpdate::Plan(Plan::new(vec![
                    PlanEntry::new(
                        "Look at the schema",
                        PlanEntryPriority::High,
                        PlanEntryStatus::Completed,
                    ),
                    PlanEntry::new(
                        "Write the query",
                        PlanEntryPriority::Medium,
                        PlanEntryStatus::InProgress,
                    ),
                ])),
            ))?;
            cx.send_notification(SessionNotification::new(
                session.clone(),
                SessionUpdate::ToolCall(
                    ToolCall::new("call-1", "List tables")
                        .kind(ToolKind::Read)
                        .status(ToolCallStatus::InProgress),
                ),
            ))?;
            Ok(StopReason::EndTurn)
        }
        "permission" => {
            cx.send_notification(SessionNotification::new(
                session.clone(),
                SessionUpdate::ToolCall(
                    ToolCall::new("call-2", "Run DELETE")
                        .kind(ToolKind::Execute)
                        .status(ToolCallStatus::Pending),
                ),
            ))?;
            let options = vec![
                PermissionOption::new("allow-once", "Allow once", PermissionOptionKind::AllowOnce),
                PermissionOption::new(
                    "allow-always",
                    "Allow always",
                    PermissionOptionKind::AllowAlways,
                ),
                PermissionOption::new(
                    "reject-once",
                    "Reject once",
                    PermissionOptionKind::RejectOnce,
                ),
                PermissionOption::new(
                    "reject-always",
                    "Reject always",
                    PermissionOptionKind::RejectAlways,
                ),
            ];
            let tool_call = ToolCallUpdate::new(
                "call-2",
                ToolCallUpdateFields::new()
                    .title("Run DELETE".to_owned())
                    .kind(ToolKind::Execute),
            );
            let response = cx
                .send_request(RequestPermissionRequest::new(
                    session.clone(),
                    tool_call,
                    options,
                ))
                .block_task()
                .await?;
            let reply = match response.outcome {
                RequestPermissionOutcome::Selected(selected) => {
                    format!("selected:{}", selected.option_id)
                }
                _ => "cancelled".to_owned(),
            };
            message(&cx, &session, &reply)?;
            Ok(if state.cancelled.load(Ordering::SeqCst) {
                StopReason::Cancelled
            } else {
                StopReason::EndTurn
            })
        }
        "crash" => {
            eprintln!("mock: crashing");
            std::process::exit(3);
        }
        "hang" => {
            wait_for_cancel(&state).await;
            Ok(StopReason::Cancelled)
        }
        "hang-deaf" => std::future::pending().await,
        "env" => {
            let args: Vec<String> = std::env::args().skip(1).collect();
            let env: BTreeMap<String, String> = std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect();
            message(
                &cx,
                &session,
                &serde_json::json!({ "args": args, "env": env }).to_string(),
            )?;
            Ok(StopReason::EndTurn)
        }
        "descriptor" => {
            let value = state.last_mcp.lock().map(|g| g.clone()).unwrap_or_default();
            message(&cx, &session, &value.to_string())?;
            Ok(StopReason::EndTurn)
        }
        other => {
            message(&cx, &session, &format!("unknown prompt: {other}"))?;
            Ok(StopReason::EndTurn)
        }
    }
}

async fn serve(state: Arc<State>) -> Result<(), agent_client_protocol::Error> {
    let init_state = Arc::clone(&state);
    let session_state = Arc::clone(&state);
    let prompt_state = Arc::clone(&state);
    let cancel_state = Arc::clone(&state);
    let transport = ByteStreams::new(
        tokio::io::stdout().compat_write(),
        tokio::io::stdin().compat(),
    );
    Agent
        .builder()
        .name("sqler-acp-mock-agent")
        .on_receive_request(
            async move |request: InitializeRequest, responder, _cx| {
                if init_state.crash_on_init {
                    std::process::exit(4);
                }
                responder.respond(
                    InitializeResponse::new(request.protocol_version)
                        .agent_capabilities(
                            AgentCapabilities::new().mcp_capabilities(
                                McpCapabilities::new().http(!init_state.no_mcp_http),
                            ),
                        )
                        .agent_info(Implementation::new("sqler-acp-mock-agent", "0.1.0")),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: NewSessionRequest, responder, _cx| {
                let n = session_state.sessions.fetch_add(1, Ordering::SeqCst) + 1;
                if let Ok(mut guard) = session_state.last_mcp.lock() {
                    *guard = serde_json::to_value(&request.mcp_servers).unwrap_or_default();
                }
                responder.respond(NewSessionResponse::new(format!("mock-session-{n}")))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                let state = Arc::clone(&prompt_state);
                let text = request
                    .prompt
                    .iter()
                    .find_map(|block| match block {
                        ContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let task_cx = cx.clone();
                cx.spawn(async move {
                    let stop = run_prompt(task_cx, state, request.session_id, text).await?;
                    responder.respond(PromptResponse::new(stop))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |_cancel: CancelNotification, _cx| {
                cancel_state.cancelled.store(true, Ordering::SeqCst);
                cancel_state.cancel.notify_one();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(transport)
        .await
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let state = Arc::new(State {
        no_mcp_http: args.iter().any(|a| a == "--no-mcp-http"),
        crash_on_init: args.iter().any(|a| a == "--crash-on-init"),
        ..State::default()
    });
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("mock: cannot start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve(state)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mock: {}", error.message);
            ExitCode::FAILURE
        }
    }
}
