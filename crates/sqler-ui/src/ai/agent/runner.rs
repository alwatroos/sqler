// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Starting an agent and the task that owns it (ADR-0051).
//!
//! [`start_agent`] runs on tokio. It builds the tab's own tool set, serves it on a loopback MCP
//! endpoint, launches the agent and opens one ACP session that gets the endpoint and its token.
//! One runner task per tab then owns the agent handle, the endpoint and the connection source; the
//! workspace talks to it through an [`AgentControl`], which is synchronous and needs no runtime.
//! Every way out (close, quit, crash, AI off, a dropped control) runs one teardown, in this
//! order: drop the prompt, stop the endpoint, shut the source, close the agent (kill and reap).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use sqler_acp::{
    AcpError, AgentEvent, AgentEvents, AgentHandle, AgentInfo, AgentProfile, ExitInfo,
    LaunchOptions, McpServerDescriptor, SessionId, StopReason,
};
use sqler_ai_tools::{ConnectionInfo, ToolSet};
use sqler_config::AiConfig;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{oneshot, watch};

use super::endpoint::{Endpoint, bind_endpoint};
use super::host::{AgentMsg, TabAcpHost};
use crate::ai::AiAudit;
use crate::ai::approval::ChannelApprover;
use crate::ai::session::TabSource;
use crate::connections::{ConnectRequest, Connector};
use crate::tabs::TabId;

/// After the agent's stdin is closed, how long it may take to exit before it is killed.
pub const AGENT_SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
/// The name the agent sees for the tools of `SQLer`.
const SERVER_NAME: &str = "sqler";
/// How long the teardown waits for the statement in flight to be cancelled.
const CANCEL_LIMIT: Duration = Duration::from_millis(250);
/// How long the teardown waits for the events the agent left behind.
const DRAIN_LIMIT: Duration = Duration::from_secs(1);
/// How long the teardown waits for the idle session to close.
const SOURCE_CLOSE_LIMIT: Duration = Duration::from_millis(500);
/// Why a start that was stopped midway reports a failure.
const START_ABORTED: &str = "The agent was stopped while it started.";
/// The text of a prompt that arrives while another one runs.
const PROMPT_BUSY: &str = "The agent is still working on the previous prompt.";

/// The time limits of an agent in the app: 30 s to start, 5 s to end a cancelled turn, 0.5 s to
/// exit after its stdin closes.
#[must_use]
pub fn app_launch_options() -> LaunchOptions {
    LaunchOptions {
        init_timeout: Duration::from_secs(30),
        cancel_grace: Duration::from_secs(5),
        shutdown_grace: AGENT_SHUTDOWN_GRACE,
    }
}

/// The side of an abort signal that the tab keeps: [`AgentAbortHandle::fire`] (or dropping it)
/// makes the start give up and kill what it already launched, and makes a started agent tear
/// down even when the tab never adopted it.
#[derive(Debug)]
pub struct AgentAbortHandle(watch::Sender<bool>);

impl AgentAbortHandle {
    /// Aborts the start or the running agent. Safe to call any number of times.
    pub fn fire(&self) {
        self.0.send_replace(true);
    }
}

/// The side of an abort signal that [`start_agent`] and the runner watch.
#[derive(Debug)]
pub struct AgentAbort(Option<watch::Receiver<bool>>);

impl AgentAbort {
    /// A signal and the handle that fires it.
    #[must_use]
    pub fn new() -> (AgentAbortHandle, Self) {
        let (tx, rx) = watch::channel(false);
        (AgentAbortHandle(tx), Self(Some(rx)))
    }

    /// A signal that never fires.
    #[must_use]
    pub fn never() -> Self {
        Self(None)
    }

    /// Whether the signal fired (or its handle is gone).
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|rx| *rx.borrow() || rx.has_changed().is_err())
    }

    /// Resolves when the signal fires or its handle is dropped; never for [`AgentAbort::never`].
    async fn fired(&mut self) {
        let Some(rx) = &mut self.0 else {
            return std::future::pending().await;
        };
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Runs `fut` unless the signal fires first; the future is dropped then.
    async fn guard<T>(&mut self, fut: impl Future<Output = T>) -> Option<T> {
        tokio::select! {
            biased;
            () = self.fired() => None,
            value = fut => Some(value),
        }
    }
}

/// Everything [`start_agent`] needs.
pub struct AgentStart {
    /// The agent tab.
    pub tab: TabId,
    /// The agent to run.
    pub profile: AgentProfile,
    /// The connection the tab is about.
    pub info: ConnectionInfo,
    /// How to connect to it.
    pub request: ConnectRequest,
    /// The AI limits and the redaction list the tools use.
    pub config: AiConfig,
    /// Where the audit entries go, as surface `agent` (required).
    pub audit: AiAudit,
    /// The working folder of the agent session (absolute; created when missing).
    pub cwd: PathBuf,
    /// The time limits of the agent.
    pub launch: LaunchOptions,
    /// The host channel of the tab.
    pub tx: UnboundedSender<AgentMsg>,
    /// Fired when the tab stops the start (close, AI off, quit).
    pub abort: AgentAbort,
}

// Ids only: never the profile's `env`.
impl std::fmt::Debug for AgentStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentStart")
            .field("tab", &self.tab)
            .field("profile", &self.profile.id)
            .finish_non_exhaustive()
    }
}

/// A started agent.
pub struct AgentStarted {
    /// What the tab keeps.
    pub ready: AgentReady,
    /// Resolves with the exit of the process once the agent and its endpoint are torn down.
    pub finished: oneshot::Receiver<ExitInfo>,
}

impl std::fmt::Debug for AgentStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentStarted").finish_non_exhaustive()
    }
}

/// What the tab keeps of a running agent.
pub struct AgentReady {
    /// Sends the runner its commands.
    pub control: AgentControl,
    /// What the agent said about itself.
    pub agent: AgentInfo,
    /// `http://127.0.0.1:<port>/mcp`; it holds no token.
    pub endpoint_url: String,
    /// The connection source the tools use.
    pub source: Arc<TabSource>,
}

// Ids only.
impl std::fmt::Debug for AgentReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentReady")
            .field("pid", &self.control.pid())
            .field("url", &self.endpoint_url)
            .finish_non_exhaustive()
    }
}

/// Why an agent did not start or stopped, in words for the user.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct AgentFailure {
    /// What happened.
    pub message: String,
    /// How to install the agent, when it was not found.
    pub install_hint: Option<String>,
    /// The last lines the agent wrote to stderr.
    pub stderr_tail: Vec<String>,
}

// The tail comes from the agent: its size only.
impl std::fmt::Debug for AgentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentFailure")
            .field("message", &self.message)
            .field("install_hint", &self.install_hint.is_some())
            .field("stderr_lines", &self.stderr_tail.len())
            .finish()
    }
}

impl AgentFailure {
    /// A failure with only a message.
    #[must_use]
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    /// The failure that an ACP error means. The texts of `AcpError` hold no token and no `env`.
    fn from_acp(error: &AcpError) -> Self {
        match error {
            AcpError::AgentNotFound {
                profile,
                program,
                install_hint,
            } => Self {
                message: format!(
                    "Agent \"{profile}\" was not found: \"{program}\" is not on PATH."
                ),
                install_hint: Some(install_hint.clone()),
                stderr_tail: Vec::new(),
            },
            AcpError::AgentExited { exit, .. } => Self {
                message: error.to_string(),
                install_hint: None,
                stderr_tail: exit.stderr_tail.clone(),
            },
            other => Self::plain(other.to_string()),
        }
    }
}

enum Command {
    Prompt { turn: u64, text: String },
    Cancel,
    Close,
}

/// Sends the runner of one agent its commands. Every method returns at once and needs no
/// runtime. When the last clone is dropped, the runner tears the agent down.
#[derive(Clone)]
pub struct AgentControl {
    tx: UnboundedSender<Command>,
    pid: Option<u32>,
}

impl std::fmt::Debug for AgentControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentControl")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl AgentControl {
    /// Sends `text` as the prompt of `turn`; the end of the turn comes back as
    /// [`AgentMsg::PromptDone`].
    pub fn prompt(&self, turn: u64, text: String) {
        let _ = self.tx.send(Command::Prompt { turn, text });
    }

    /// Cancels the running prompt. The process is killed only when the agent ignores the cancel
    /// for the grace period of the launch options.
    pub fn cancel(&self) {
        let _ = self.tx.send(Command::Cancel);
    }

    /// Tears the agent down: endpoint, source, then the process (killed and reaped).
    pub fn close(&self) {
        let _ = self.tx.send(Command::Close);
    }

    /// The process id of the agent.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

/// Builds the tool set and its endpoint, launches the agent and opens its session. On failure,
/// everything already started is torn down before this returns.
///
/// # Errors
/// An [`AgentFailure`] naming what failed; an agent that was not found carries its install hint.
pub async fn start_agent(
    start: AgentStart,
    connector: Arc<dyn Connector>,
) -> Result<AgentStarted, AgentFailure> {
    let AgentStart {
        profile,
        info,
        request,
        config,
        audit,
        cwd,
        launch,
        tx,
        mut abort,
        ..
    } = start;
    let aborted = || AgentFailure::plain(START_ABORTED);
    create_cwd(cwd.clone()).await?;
    if abort.is_set() {
        return Err(aborted());
    }
    let source = Arc::new(TabSource::new(info, request, connector));
    let approver = Arc::new(ChannelApprover::new(tx.clone(), AgentMsg::Ask));
    let tools = ToolSet::configured(source.clone(), &config).with_approver(approver);
    let (endpoint, token) = match bind_endpoint(tools, &audit).await {
        Ok(bound) => bound,
        Err(message) => {
            close_source(&source).await;
            return Err(AgentFailure::plain(message));
        }
    };
    let host = Arc::new(TabAcpHost(tx.clone()));
    // A launch that is dropped midway kills the process it spawned (sqler-acp).
    let launched = abort
        .guard(AgentHandle::launch(&profile, host, launch))
        .await;
    let (handle, events) = match launched {
        Some(Ok(launched)) => launched,
        Some(Err(error)) => {
            unwind(endpoint, &source).await;
            return Err(AgentFailure::from_acp(&error));
        }
        None => {
            unwind(endpoint, &source).await;
            return Err(aborted());
        }
    };
    let url = endpoint.url().to_owned();
    let descriptor = McpServerDescriptor::Http {
        name: SERVER_NAME.to_owned(),
        url: url.clone(),
        token,
    };
    let opened = abort
        .guard(handle.new_session(&cwd, vec![descriptor]))
        .await;
    let session = match opened {
        Some(Ok(session)) if !abort.is_set() => session,
        Some(Ok(_)) | None => {
            unwind(endpoint, &source).await;
            handle.close().await;
            return Err(aborted());
        }
        Some(Err(error)) => {
            let mut failure = AgentFailure::from_acp(&error);
            unwind(endpoint, &source).await;
            let exit = handle.close().await;
            if failure.stderr_tail.is_empty() {
                failure.stderr_tail = exit.stderr_tail;
            }
            return Err(failure);
        }
    };
    let (commands, cmds) = unbounded_channel();
    let (finish, finished) = oneshot::channel();
    let control = AgentControl {
        tx: commands,
        pid: handle.pid(),
    };
    let agent = handle.info().clone();
    tokio::spawn(
        Runner {
            handle,
            events,
            session,
            cmds,
            endpoint,
            source: Arc::clone(&source),
            tx,
            finish,
            abort,
        }
        .run(),
    );
    Ok(AgentStarted {
        ready: AgentReady {
            control,
            agent,
            endpoint_url: url,
            source,
        },
        finished,
    })
}

/// Creates the working folder of the agent on the blocking pool.
async fn create_cwd(cwd: PathBuf) -> Result<(), AgentFailure> {
    let made = tokio::task::spawn_blocking(move || std::fs::create_dir_all(cwd)).await;
    match made {
        Ok(Ok(())) => Ok(()),
        _ => Err(AgentFailure::plain(
            "The working folder of the agent could not be created.",
        )),
    }
}

/// Stops the endpoint of a start that did not finish, then shuts the source.
async fn unwind(endpoint: Endpoint, source: &TabSource) {
    endpoint.stop().await;
    close_source(source).await;
}

/// Shuts the source and closes its idle session, for at most [`SOURCE_CLOSE_LIMIT`]: a slow
/// remote close must not delay the kill of the agent.
async fn close_source(source: &TabSource) {
    if let Some(session) = source.shut() {
        let _ = tokio::time::timeout(SOURCE_CLOSE_LIMIT, session.close()).await;
    }
}

type Pending<'a, T> = Option<Pin<Box<dyn Future<Output = T> + Send + 'a>>>;

/// Waits for the future in `slot`, or forever when there is none.
async fn pending_or<T>(slot: &mut Pending<'_, T>) -> T {
    match slot {
        Some(fut) => fut.await,
        None => std::future::pending().await,
    }
}

/// The task that owns one agent.
struct Runner {
    handle: AgentHandle,
    events: AgentEvents,
    session: SessionId,
    cmds: UnboundedReceiver<Command>,
    endpoint: Endpoint,
    source: Arc<TabSource>,
    tx: UnboundedSender<AgentMsg>,
    finish: oneshot::Sender<ExitInfo>,
    abort: AgentAbort,
}

impl Runner {
    async fn run(self) {
        let Self {
            handle,
            mut events,
            session,
            mut cmds,
            endpoint,
            source,
            tx,
            finish,
            mut abort,
        } = self;
        let mut exited: Option<ExitInfo> = None;
        {
            let mut prompt: Pending<'_, (u64, Result<StopReason, AcpError>)> = None;
            let mut cancel: Pending<'_, ()> = None;
            loop {
                // Biased: what the agent said before it ended a prompt reaches the tab before
                // the end of the prompt does, and a command is never starved by a chatty agent.
                tokio::select! {
                    biased;
                    () = abort.fired() => break,
                    command = cmds.recv() => match command {
                        Some(Command::Prompt { turn, text }) => {
                            if prompt.is_some() {
                                let _ = tx.send(AgentMsg::PromptDone {
                                    turn,
                                    result: Err(PROMPT_BUSY.to_owned()),
                                });
                            } else {
                                let (handle, session) = (&handle, &session);
                                prompt = Some(Box::pin(async move {
                                    (turn, handle.prompt(session, text).await)
                                }));
                            }
                        }
                        Some(Command::Cancel) => {
                            if prompt.is_some() && cancel.is_none() {
                                let (handle, session) = (&handle, &session);
                                cancel = Some(Box::pin(async move {
                                    let _ = handle.cancel(session).await;
                                }));
                            }
                        }
                        Some(Command::Close) | None => break,
                    },
                    event = events.recv() => match event {
                        Some(AgentEvent::Exited(exit)) => {
                            exited = Some(exit);
                            break;
                        }
                        Some(event) => {
                            let _ = tx.send(AgentMsg::Event(event));
                        }
                        None => break,
                    },
                    (turn, result) = pending_or(&mut prompt) => {
                        prompt = None;
                        cancel = None;
                        let result = result.map_err(|error| error.to_string());
                        let _ = tx.send(AgentMsg::PromptDone { turn, result });
                    }
                    () = pending_or(&mut cancel) => {
                        cancel = None;
                    }
                }
            }
        }
        // The prompt future is gone. Stop what the agent can reach, then the agent.
        if let Some(in_flight) = source.cancel_in_flight() {
            let _ = tokio::time::timeout(CANCEL_LIMIT, in_flight.cancel()).await;
        }
        endpoint.stop().await;
        close_source(&source).await;
        let exit = close_and_drain(handle, &mut events, &tx, &mut exited).await;
        let exit = exited.unwrap_or(exit);
        let _ = tx.send(AgentMsg::Event(AgentEvent::Exited(exit.clone())));
        let _ = finish.send(exit);
    }
}

/// Closes the agent while forwarding what it still says: the events channel is bounded, and a
/// supervisor that waits on a full channel would never exit. The `Exited` event is kept for the
/// caller, who sends it last.
async fn close_and_drain(
    handle: AgentHandle,
    events: &mut AgentEvents,
    tx: &UnboundedSender<AgentMsg>,
    exited: &mut Option<ExitInfo>,
) -> ExitInfo {
    let close = handle.close();
    tokio::pin!(close);
    let mut open = true;
    let exit = loop {
        tokio::select! {
            exit = &mut close => break exit,
            event = events.recv(), if open => match event {
                Some(AgentEvent::Exited(exit)) => *exited = Some(exit),
                Some(event) => {
                    let _ = tx.send(AgentMsg::Event(event));
                }
                None => open = false,
            },
        }
    };
    while let Ok(Some(event)) = tokio::time::timeout(DRAIN_LIMIT, events.recv()).await {
        match event {
            AgentEvent::Exited(exit) => *exited = Some(exit),
            event => {
                let _ = tx.send(AgentMsg::Event(event));
            }
        }
    }
    exit
}
