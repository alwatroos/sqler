// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The agent supervisor task: owns the child process and the `ACP` connection, serves the commands of an
//! [`AgentHandle`](crate::AgentHandle), relays updates and permission requests, and reaps the process on every
//! exit path.
//!
//! This is the only module that talks to the protocol crate's connection API; the mapping between its types
//! and `SQLer`'s own types lives here too, so a crate bump stays local.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities, Implementation,
    InitializeRequest, NewSessionRequest, PermissionOptionKind as AcpOptionKind, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, StopReason as AcpStopReason, TextContent,
    ToolCallContent, ToolCallStatus as AcpStatus, ToolKind,
};
use agent_client_protocol::{
    Agent, ByteStreams, Client, ConnectionTo, is_incoming_transport_closed,
};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::client::LaunchOptions;
use crate::descriptor::McpServerDescriptor;
use crate::error::AcpError;
use crate::process::{self, AgentProcess};
use crate::types::{
    AcpHost, AgentEvent, AgentInfo, ExitInfo, PermissionDecision, PermissionOption,
    PermissionOptionKind, PermissionRequest, PlanEntry, SessionId, SessionUpdate, StopReason,
    ToolCallInfo, ToolCallStatus,
};

/// What an [`AgentHandle`](crate::AgentHandle) asks the supervisor to do.
pub(crate) enum Command {
    NewSession {
        cwd: PathBuf,
        mcp: Vec<McpServerDescriptor>,
        reply: oneshot::Sender<Result<SessionId, AcpError>>,
    },
    Prompt {
        session: SessionId,
        text: String,
        reply: oneshot::Sender<Result<StopReason, AcpError>>,
    },
    Cancel {
        session: SessionId,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Close,
}

/// Everything the supervisor needs, handed over by `AgentHandle::launch`.
pub(crate) struct Launch {
    pub(crate) profile_name: String,
    pub(crate) host: Arc<dyn AcpHost>,
    pub(crate) opts: LaunchOptions,
    pub(crate) process: AgentProcess,
    pub(crate) events: mpsc::Sender<AgentEvent>,
    pub(crate) commands: mpsc::UnboundedReceiver<Command>,
    pub(crate) ready: oneshot::Sender<Result<AgentInfo, AcpError>>,
    pub(crate) exit: watch::Sender<Option<ExitInfo>>,
}

struct SessionState {
    prompting: bool,
    /// Bumped by every cancel of this session.
    cancel: watch::Sender<u64>,
}

/// State shared by the supervisor, its tasks and the connection handlers.
#[derive(Default)]
struct Shared {
    sessions: Mutex<HashMap<String, SessionState>>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionState>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn add_session(&self, id: &str) {
        let (cancel, _) = watch::channel(0);
        self.lock().insert(
            id.to_owned(),
            SessionState {
                prompting: false,
                cancel,
            },
        );
    }

    fn subscribe(&self, id: &str) -> Option<watch::Receiver<u64>> {
        self.lock().get(id).map(|s| s.cancel.subscribe())
    }

    fn cancel(&self, id: &str) -> bool {
        self.lock()
            .get(id)
            .map(|s| s.cancel.send_modify(|n| *n += 1))
            .is_some()
    }

    /// Mark a prompt as running; refuse if the session is unknown or already busy.
    fn begin_prompt(&self, id: &str) -> Result<watch::Receiver<u64>, AcpError> {
        let mut sessions = self.lock();
        let state = sessions.get_mut(id).ok_or(AcpError::UnknownSession)?;
        if state.prompting {
            return Err(AcpError::PromptInProgress);
        }
        state.prompting = true;
        Ok(state.cancel.subscribe())
    }

    fn end_prompt(&self, id: &str) {
        if let Some(state) = self.lock().get_mut(id) {
            state.prompting = false;
        }
    }
}

/// Run one agent until it is closed or ends. Always reaps the process and publishes the exit.
pub(crate) async fn supervise(launch: Launch) {
    let Launch {
        profile_name,
        host,
        opts,
        process,
        events,
        commands,
        ready,
        exit,
    } = launch;
    let AgentProcess {
        mut child,
        stdin,
        stdout,
        stderr,
    } = process;
    let shared = Arc::new(Shared::default());
    let kill = Arc::new(Notify::new());

    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let updates = events.clone();
    let permission_shared = Arc::clone(&shared);
    let main_ctx = MainCtx {
        profile_name,
        opts: opts.clone(),
        shared: Arc::clone(&shared),
        kill: Arc::clone(&kill),
        commands,
        ready,
    };
    let connection = Client
        .builder()
        .name("sqler")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                let event = AgentEvent::Update {
                    session_id: SessionId(notification.session_id.to_string()),
                    update: map_update(notification.update),
                };
                // A dropped receiver only means nobody listens any more.
                let _ = updates.send(event).await;
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, cx: ConnectionTo<Agent>| {
                let host = Arc::clone(&host);
                let shared = Arc::clone(&permission_shared);
                cx.spawn(async move {
                    let outcome = decide_permission(host.as_ref(), &shared, &request).await;
                    responder.respond(RequestPermissionResponse::new(outcome))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
            client_main(cx, main_ctx).await
        });
    let mut connection = Box::pin(connection);

    // The connection ends on a clean close, on an error, or (for a crash) when the process exits first.
    let (status, grace) = tokio::select! {
        _ = &mut connection => (None, opts.shutdown_grace),
        status = child.wait() => (Some(status.ok()), opts.shutdown_grace),
        () = kill.notified() => {
            // Kill first: closing stdin alone would let a polite agent exit on its own.
            let _ = child.start_kill();
            (None, Duration::ZERO)
        }
    };
    drop(connection); // closes the agent's stdin
    let info = match status {
        Some(status) => stderr.finish(status).await,
        None => process::shutdown(child, stderr, grace).await,
    };
    exit.send_replace(Some(info.clone()));
    let _ = events.send(AgentEvent::Exited(info)).await;
}

struct MainCtx {
    profile_name: String,
    opts: LaunchOptions,
    shared: Arc<Shared>,
    kill: Arc<Notify>,
    commands: mpsc::UnboundedReceiver<Command>,
    ready: oneshot::Sender<Result<AgentInfo, AcpError>>,
}

/// Turn a protocol error into ours. `None` means the transport closed: the caller drops its reply and the
/// handle reports `AgentExited` once the supervisor has reaped the process.
fn map_error(method: &'static str, error: &agent_client_protocol::Error) -> Option<AcpError> {
    if is_incoming_transport_closed(error) {
        None
    } else {
        Some(AcpError::Agent {
            method,
            message: error.message.clone(),
        })
    }
}

/// Run `initialize` with the capabilities `SQLer` offers: none (no file system, no terminal).
/// `Err(None)` means the transport closed, so the launch reports the exit of the process.
async fn initialize(
    cx: &ConnectionTo<Agent>,
    profile_name: String,
    timeout: Duration,
) -> Result<AgentInfo, Option<AcpError>> {
    let request = InitializeRequest::new(ProtocolVersion::LATEST)
        .client_capabilities(
            ClientCapabilities::new()
                .fs(FileSystemCapabilities::new()
                    .read_text_file(false)
                    .write_text_file(false))
                .terminal(false),
        )
        .client_info(Implementation::new("sqler", env!("CARGO_PKG_VERSION")));
    let Ok(outcome) = tokio::time::timeout(timeout, cx.send_request(request).block_task()).await
    else {
        return Err(Some(AcpError::Timeout {
            profile: profile_name,
            step: "initialize",
        }));
    };
    let response = outcome.map_err(|error| map_error("initialize", &error))?;
    if response.protocol_version != ProtocolVersion::LATEST {
        return Err(Some(AcpError::UnsupportedProtocol {
            profile: profile_name,
            version: response.protocol_version.as_u16(),
        }));
    }
    Ok(AgentInfo {
        protocol_version: response.protocol_version.as_u16(),
        name: response.agent_info.as_ref().map(|i| i.name.clone()),
        version: response.agent_info.as_ref().map(|i| i.version.clone()),
        mcp_http: response.agent_capabilities.mcp_capabilities.http,
        mcp_sse: response.agent_capabilities.mcp_capabilities.sse,
    })
}

async fn client_main(
    cx: ConnectionTo<Agent>,
    ctx: MainCtx,
) -> Result<(), agent_client_protocol::Error> {
    let MainCtx {
        profile_name,
        opts,
        shared,
        kill,
        mut commands,
        mut ready,
    } = ctx;

    // A launch that is dropped while the agent initializes (the app quits, the tab closes)
    // kills the process now instead of after the init timeout.
    let init = tokio::select! {
        init = initialize(&cx, profile_name, opts.init_timeout) => init,
        () = ready.closed() => {
            kill.notify_one();
            return std::future::pending().await;
        }
    };
    let info = match init {
        Ok(info) => info,
        Err(error) => {
            if let Some(error) = error {
                let _ = ready.send(Err(error));
            }
            return Ok(());
        }
    };
    if ready.send(Ok(info)).is_err() {
        return Ok(()); // the launch was abandoned
    }

    // Step 2: serve commands until close or until the agent goes away.
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            () = cx.incoming_closed() => None,
        };
        let Some(command) = command else { break };
        match command {
            Command::Close => break,
            Command::NewSession { cwd, mcp, reply } => {
                let cx = cx.clone();
                let shared = Arc::clone(&shared);
                cx.clone().spawn(async move {
                    let servers = mcp.iter().map(McpServerDescriptor::to_acp).collect();
                    let result = cx
                        .send_request(NewSessionRequest::new(cwd).mcp_servers(servers))
                        .block_task()
                        .await;
                    match result {
                        Ok(response) => {
                            let id = response.session_id.to_string();
                            shared.add_session(&id);
                            let _ = reply.send(Ok(SessionId(id)));
                        }
                        Err(error) => {
                            if let Some(error) = map_error("session/new", &error) {
                                let _ = reply.send(Err(error));
                            }
                        }
                    }
                    Ok(())
                })?;
            }
            Command::Prompt {
                session,
                text,
                reply,
            } => {
                let cancel = match shared.begin_prompt(session.as_str()) {
                    Ok(cancel) => cancel,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                };
                let task_cx = cx.clone();
                let shared = Arc::clone(&shared);
                let kill = Arc::clone(&kill);
                let grace = opts.cancel_grace;
                cx.spawn(async move {
                    let result = run_prompt(&task_cx, &session, text, cancel, grace, &kill).await;
                    shared.end_prompt(session.as_str());
                    if let Some(result) = result {
                        let _ = reply.send(result);
                    }
                    Ok(())
                })?;
            }
            Command::Cancel { session, reply } => {
                if shared.cancel(session.as_str()) {
                    let sent = cx.send_notification(CancelNotification::new(session.0.clone()));
                    let _ = reply.send(sent.map_err(|_| AcpError::Closed));
                } else {
                    let _ = reply.send(Err(AcpError::UnknownSession));
                }
            }
        }
    }
    Ok(())
}

/// Send the prompt and wait for it. After a cancel the agent gets `grace` to answer; then the process is
/// killed and the prompt resolves as cancelled. `None` means the transport closed (the handle reports the exit).
async fn run_prompt(
    cx: &ConnectionTo<Agent>,
    session: &SessionId,
    text: String,
    mut cancel: watch::Receiver<u64>,
    grace: Duration,
    kill: &Notify,
) -> Option<Result<StopReason, AcpError>> {
    let request = PromptRequest::new(
        session.0.clone(),
        vec![ContentBlock::Text(TextContent::new(text))],
    );
    let mut response = pin!(cx.send_request(request).block_task());
    let cancelled = tokio::select! {
        result = &mut response => Ok(result),
        _ = cancel.changed() => Err(()),
    };
    let result = if let Ok(result) = cancelled {
        result
    } else {
        // Cancelled: the agent gets the grace period to end the turn itself.
        let Ok(result) = tokio::time::timeout(grace, &mut response).await else {
            kill.notify_one();
            return Some(Ok(StopReason::Cancelled));
        };
        result
    };
    match result {
        Ok(response) => Some(Ok(map_stop_reason(response.stop_reason))),
        Err(error) => map_error("session/prompt", &error).map(Err),
    }
}

/// Ask the host; a cancel of the session answers the agent with `Cancelled` without waiting for the host.
async fn decide_permission(
    host: &dyn AcpHost,
    shared: &Shared,
    request: &RequestPermissionRequest,
) -> RequestPermissionOutcome {
    let session_id = SessionId(request.session_id.to_string());
    let cancel = shared.subscribe(session_id.as_str());
    let mapped = map_permission(request, session_id);
    let offered: Vec<String> = mapped.options.iter().map(|o| o.id.clone()).collect();
    let ask = host.request_permission(mapped);
    let decision = match cancel {
        Some(mut cancel) => tokio::select! {
            decision = ask => decision,
            _ = cancel.changed() => PermissionDecision::Cancelled,
        },
        None => ask.await,
    };
    match decision {
        PermissionDecision::Selected(id) if offered.contains(&id) => {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id))
        }
        _ => RequestPermissionOutcome::Cancelled,
    }
}

fn map_stop_reason(reason: AcpStopReason) -> StopReason {
    match reason {
        AcpStopReason::MaxTokens => StopReason::MaxTokens,
        AcpStopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        AcpStopReason::Refusal => StopReason::Refusal,
        AcpStopReason::Cancelled => StopReason::Cancelled,
        // `EndTurn` and any reason a later protocol version adds: the turn is over.
        _ => StopReason::EndTurn,
    }
}

fn map_permission(request: &RequestPermissionRequest, session_id: SessionId) -> PermissionRequest {
    let options = request
        .options
        .iter()
        .map(|o| PermissionOption {
            id: o.option_id.to_string(),
            name: o.name.clone(),
            kind: match o.kind {
                AcpOptionKind::AllowOnce => PermissionOptionKind::AllowOnce,
                AcpOptionKind::AllowAlways => PermissionOptionKind::AllowAlways,
                AcpOptionKind::RejectAlways => PermissionOptionKind::RejectAlways,
                // `RejectOnce` and anything new: the most cautious reading is a single refusal.
                _ => PermissionOptionKind::RejectOnce,
            },
        })
        .collect();
    let fields = &request.tool_call.fields;
    PermissionRequest {
        session_id,
        tool_call: ToolCallInfo {
            id: request.tool_call.tool_call_id.to_string(),
            title: fields.title.clone(),
            kind: fields.kind.map(kind_name),
            status: fields.status.and_then(map_status),
            text: fields
                .content
                .as_deref()
                .map(content_text)
                .unwrap_or_default(),
        },
        options,
    }
}

fn kind_name(kind: ToolKind) -> String {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "switch_mode",
        _ => "other",
    }
    .to_owned()
}

fn map_status(status: AcpStatus) -> Option<ToolCallStatus> {
    match status {
        AcpStatus::Pending => Some(ToolCallStatus::Pending),
        AcpStatus::InProgress => Some(ToolCallStatus::InProgress),
        AcpStatus::Completed => Some(ToolCallStatus::Completed),
        AcpStatus::Failed => Some(ToolCallStatus::Failed),
        _ => None,
    }
}

fn content_text(content: &[ToolCallContent]) -> Vec<String> {
    content
        .iter()
        .filter_map(|c| match c {
            ToolCallContent::Content(c) => match &c.content {
                ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn chunk_text(chunk: &agent_client_protocol::schema::v1::ContentChunk) -> Option<String> {
    match &chunk.content {
        ContentBlock::Text(t) => Some(t.text.clone()),
        _ => None,
    }
}

/// Map a protocol update. Only text content is carried; everything else becomes `Other`.
pub(crate) fn map_update(
    update: agent_client_protocol::schema::v1::SessionUpdate,
) -> SessionUpdate {
    use agent_client_protocol::schema::v1::SessionUpdate as U;
    match update {
        U::AgentMessageChunk(c) => chunk_text(&c).map_or(
            SessionUpdate::Other("non-text agent message"),
            SessionUpdate::AgentMessage,
        ),
        U::AgentThoughtChunk(c) => chunk_text(&c).map_or(
            SessionUpdate::Other("non-text agent thought"),
            SessionUpdate::AgentThought,
        ),
        U::UserMessageChunk(c) => chunk_text(&c).map_or(
            SessionUpdate::Other("non-text user message"),
            SessionUpdate::UserMessage,
        ),
        U::ToolCall(call) => SessionUpdate::ToolCall(ToolCallInfo {
            id: call.tool_call_id.to_string(),
            title: Some(call.title.clone()),
            kind: Some(kind_name(call.kind)),
            status: map_status(call.status),
            text: content_text(&call.content),
        }),
        U::ToolCallUpdate(update) => {
            let fields = &update.fields;
            SessionUpdate::ToolCall(ToolCallInfo {
                id: update.tool_call_id.to_string(),
                title: fields.title.clone(),
                kind: fields.kind.map(kind_name),
                status: fields.status.and_then(map_status),
                text: fields
                    .content
                    .as_deref()
                    .map(content_text)
                    .unwrap_or_default(),
            })
        }
        U::Plan(plan) => SessionUpdate::Plan(
            plan.entries
                .iter()
                .map(|e| PlanEntry {
                    content: e.content.clone(),
                    priority: format!("{:?}", e.priority).to_lowercase(),
                    status: match e.status {
                        agent_client_protocol::schema::v1::PlanEntryStatus::Pending => "pending",
                        agent_client_protocol::schema::v1::PlanEntryStatus::InProgress => {
                            "in_progress"
                        }
                        agent_client_protocol::schema::v1::PlanEntryStatus::Completed => {
                            "completed"
                        }
                        _ => "unknown",
                    }
                    .to_owned(),
                })
                .collect(),
        ),
        U::AvailableCommandsUpdate(_) => SessionUpdate::Other("available commands"),
        U::CurrentModeUpdate(_) => SessionUpdate::Other("mode"),
        U::ConfigOptionUpdate(_) => SessionUpdate::Other("config option"),
        U::SessionInfoUpdate(_) => SessionUpdate::Other("session info"),
        U::UsageUpdate(_) => SessionUpdate::Other("usage"),
        _ => SessionUpdate::Other("unknown"),
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{
        ContentChunk, Plan, PlanEntry as AcpPlanEntry, PlanEntryPriority, PlanEntryStatus,
        SessionUpdate as U, ToolCall,
    };

    use super::*;

    #[test]
    fn text_chunks_map_and_other_content_is_labelled() {
        let text = U::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "hi",
        ))));
        assert_eq!(
            map_update(text),
            SessionUpdate::AgentMessage("hi".to_owned())
        );
        let thought = U::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("hm"),
        )));
        assert_eq!(
            map_update(thought),
            SessionUpdate::AgentThought("hm".to_owned())
        );
        let user =
            U::UserMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new("u"))));
        assert_eq!(map_update(user), SessionUpdate::UserMessage("u".to_owned()));
    }

    #[test]
    fn tool_call_and_plan_map() {
        let call = ToolCall::new("t1", "Run query")
            .kind(ToolKind::Execute)
            .status(AcpStatus::InProgress);
        let SessionUpdate::ToolCall(info) = map_update(U::ToolCall(call)) else {
            panic!("expected a tool call");
        };
        assert_eq!(info.id, "t1");
        assert_eq!(info.title.as_deref(), Some("Run query"));
        assert_eq!(info.kind.as_deref(), Some("execute"));
        assert_eq!(info.status, Some(ToolCallStatus::InProgress));

        let plan = Plan::new(vec![AcpPlanEntry::new(
            "step",
            PlanEntryPriority::High,
            PlanEntryStatus::InProgress,
        )]);
        let SessionUpdate::Plan(entries) = map_update(U::Plan(plan)) else {
            panic!("expected a plan");
        };
        assert_eq!(
            entries,
            [PlanEntry {
                content: "step".to_owned(),
                priority: "high".to_owned(),
                status: "in_progress".to_owned()
            }]
        );
    }

    #[test]
    fn stop_reasons_map() {
        assert_eq!(map_stop_reason(AcpStopReason::EndTurn), StopReason::EndTurn);
        assert_eq!(
            map_stop_reason(AcpStopReason::MaxTokens),
            StopReason::MaxTokens
        );
        assert_eq!(
            map_stop_reason(AcpStopReason::MaxTurnRequests),
            StopReason::MaxTurnRequests
        );
        assert_eq!(map_stop_reason(AcpStopReason::Refusal), StopReason::Refusal);
        assert_eq!(
            map_stop_reason(AcpStopReason::Cancelled),
            StopReason::Cancelled
        );
    }

    #[test]
    fn session_state_enforces_one_prompt() {
        let shared = Shared::default();
        assert!(matches!(
            shared.begin_prompt("s"),
            Err(AcpError::UnknownSession)
        ));
        shared.add_session("s");
        assert!(shared.begin_prompt("s").is_ok());
        assert!(matches!(
            shared.begin_prompt("s"),
            Err(AcpError::PromptInProgress)
        ));
        shared.end_prompt("s");
        assert!(shared.begin_prompt("s").is_ok());
    }
}
