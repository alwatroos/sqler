// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The public handle of a running agent.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use crate::connection::{Command, Launch, supervise};
use crate::descriptor::McpServerDescriptor;
use crate::error::AcpError;
use crate::process::{self, install_hint};
use crate::profile::{AgentProfile, resolve_program};
use crate::types::{AcpHost, AgentEvent, AgentInfo, ExitInfo, SessionId, StopReason};

/// Capacity of the event channel. The supervisor waits when it is full.
const EVENT_CAPACITY: usize = 256;

/// Time limits of a launched agent.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// How long `initialize` may take (default 30 s).
    pub init_timeout: Duration,
    /// After a cancel, how long the agent may take to end the prompt before the process is killed (default 5 s).
    pub cancel_grace: Duration,
    /// After stdin is closed, how long the process may take to exit before it is killed (default 2 s).
    pub shutdown_grace: Duration,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            init_timeout: Duration::from_secs(30),
            cancel_grace: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(2),
        }
    }
}

/// Receiver of the events of one agent.
#[derive(Debug)]
pub struct AgentEvents {
    rx: mpsc::Receiver<AgentEvent>,
}

impl AgentEvents {
    /// The next event, or `None` once the agent is gone and everything has been read.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.rx.recv().await
    }
}

/// Resolves when the agent process has ended and been reaped. Cloneable; it outlives the [`AgentHandle`].
#[derive(Debug, Clone)]
pub struct ExitWatch {
    rx: watch::Receiver<Option<ExitInfo>>,
}

impl ExitWatch {
    /// Wait for the exit. Returns at once if the process is already gone.
    pub async fn wait(&mut self) -> ExitInfo {
        self.outcome().await.unwrap_or_default()
    }

    /// `None` when the supervisor ended without reporting (it panicked).
    async fn outcome(&mut self) -> Option<ExitInfo> {
        let waited = self
            .rx
            .wait_for(Option::is_some)
            .await
            .map(|value| value.clone());
        match waited {
            Ok(value) => value,
            Err(_) => self.rx.borrow().clone(),
        }
    }
}

/// A running agent. `Send + Sync`; every method takes `&self`.
///
/// Dropping the handle closes the agent without waiting (stdin closed, killed after the grace period, reaped
/// in the background); [`AgentHandle::close`] waits for that.
#[derive(Debug)]
pub struct AgentHandle {
    profile: String,
    info: AgentInfo,
    pid: Option<u32>,
    commands: mpsc::UnboundedSender<Command>,
    exit: ExitWatch,
}

impl AgentHandle {
    /// Start the agent of `profile` and run `initialize`. Must be called inside a tokio runtime.
    ///
    /// Permission requests go to `host`. On any failure the process is killed and reaped before this returns.
    ///
    /// # Errors
    /// [`AcpError::InvalidProfile`], [`AcpError::AgentNotFound`] (naming the profile and its install hint),
    /// [`AcpError::Spawn`], [`AcpError::Timeout`], [`AcpError::UnsupportedProtocol`],
    /// [`AcpError::AgentExited`] or [`AcpError::Agent`].
    pub async fn launch(
        profile: &AgentProfile,
        host: Arc<dyn AcpHost>,
        opts: LaunchOptions,
    ) -> Result<(AgentHandle, AgentEvents), AcpError> {
        profile.validate()?;
        let path = std::env::var_os("PATH");
        let Some(program) = resolve_program(&profile.command, path.as_deref()) else {
            return Err(AcpError::AgentNotFound {
                profile: profile.name.clone(),
                program: profile.command.clone(),
                install_hint: install_hint(profile),
            });
        };
        let (events_tx, events_rx) = mpsc::channel(EVENT_CAPACITY);
        let process = process::spawn(profile, &program, events_tx.clone())?;
        let pid = process.child.id();
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (exit_tx, exit_rx) = watch::channel(None);
        tokio::spawn(supervise(Launch {
            profile_name: profile.name.clone(),
            host,
            opts,
            process,
            events: events_tx,
            commands: commands_rx,
            ready: ready_tx,
            exit: exit_tx,
        }));
        let mut exit = ExitWatch { rx: exit_rx };
        match ready_rx.await {
            Ok(Ok(info)) => Ok((
                AgentHandle {
                    profile: profile.name.clone(),
                    info,
                    pid,
                    commands: commands_tx,
                    exit,
                },
                AgentEvents { rx: events_rx },
            )),
            Ok(Err(error)) => {
                drop(commands_tx);
                exit.wait().await; // killed and reaped before we report
                Err(error)
            }
            Err(_) => {
                let exit = exit.wait().await;
                Err(AcpError::AgentExited {
                    profile: profile.name.clone(),
                    exit,
                })
            }
        }
    }

    /// What the agent said about itself in `initialize`.
    #[must_use]
    pub fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// Process id of the agent, while it is known.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// A [`ExitWatch`] for this agent.
    #[must_use]
    pub fn exit_watch(&self) -> ExitWatch {
        self.exit.clone()
    }

    /// Open a session in `cwd` (absolute) and hand the agent the `mcp` servers.
    ///
    /// # Errors
    /// [`AcpError::InvalidCwd`], [`AcpError::InvalidDescriptor`], [`AcpError::McpTransportUnsupported`] (an HTTP
    /// descriptor and an agent without HTTP `MCP`), [`AcpError::Agent`] or [`AcpError::AgentExited`].
    pub async fn new_session(
        &self,
        cwd: &Path,
        mcp: Vec<McpServerDescriptor>,
    ) -> Result<SessionId, AcpError> {
        if !cwd.is_absolute() {
            return Err(AcpError::InvalidCwd);
        }
        for server in &mcp {
            server.validate()?;
            if server.needs_http() && !self.info.mcp_http {
                return Err(AcpError::McpTransportUnsupported {
                    profile: self.profile.clone(),
                    transport: "http",
                });
            }
        }
        let cwd = cwd.to_path_buf();
        self.call(|reply| Command::NewSession { cwd, mcp, reply })
            .await
    }

    /// Send a prompt and wait for the turn to end. Updates arrive on [`AgentEvents`] meanwhile.
    ///
    /// # Errors
    /// [`AcpError::PromptInProgress`], [`AcpError::UnknownSession`], [`AcpError::Agent`] or
    /// [`AcpError::AgentExited`].
    pub async fn prompt(&self, session: &SessionId, text: String) -> Result<StopReason, AcpError> {
        let session = session.clone();
        self.call(|reply| Command::Prompt {
            session,
            text,
            reply,
        })
        .await
    }

    /// Cancel the running prompt of a session. A pending permission request of the session is answered with
    /// `Cancelled`. If the prompt does not end within `cancel_grace`, the process is killed and the prompt
    /// resolves as [`StopReason::Cancelled`].
    ///
    /// # Errors
    /// [`AcpError::UnknownSession`] or [`AcpError::AgentExited`].
    pub async fn cancel(&self, session: &SessionId) -> Result<(), AcpError> {
        let session = session.clone();
        self.call(|reply| Command::Cancel { session, reply }).await
    }

    /// Close the agent: stdin is closed, the process is killed after the grace period, and it is reaped.
    pub async fn close(self) -> ExitInfo {
        let _ = self.commands.send(Command::Close);
        let mut exit = self.exit.clone();
        exit.wait().await
    }

    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, AcpError>>) -> Command,
    ) -> Result<T, AcpError> {
        let (reply, answer) = oneshot::channel();
        if self.commands.send(make(reply)).is_ok()
            && let Ok(result) = answer.await
        {
            return result;
        }
        // The supervisor is gone or dropped the call: the agent ended.
        let mut exit = self.exit.clone();
        match exit.outcome().await {
            Some(exit) => Err(AcpError::AgentExited {
                profile: self.profile.clone(),
                exit,
            }),
            None => Err(AcpError::Closed),
        }
    }
}

impl Drop for AgentHandle {
    fn drop(&mut self) {
        // Non-blocking; the supervisor reaps the process in the background.
        let _ = self.commands.send(Command::Close);
    }
}
