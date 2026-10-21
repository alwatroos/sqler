// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The agent child process: spawn with a scrubbed environment, capture stderr, shut down and reap.

use std::collections::VecDeque;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::AcpError;
use crate::profile::{AgentProfile, agent_env};
use crate::types::{AgentEvent, ExitInfo};

/// Lines of stderr kept for the crash report.
const TAIL_LINES: usize = 64;
/// Longest stderr line kept or forwarded, in characters.
const MAX_LINE_CHARS: usize = 4096;
/// How long to wait for the stderr reader to finish after the process ended.
const STDERR_DRAIN: Duration = Duration::from_millis(500);

type Tail = Arc<Mutex<VecDeque<String>>>;

/// The stderr reader of a running agent and the tail it keeps.
pub(crate) struct StderrCapture {
    task: JoinHandle<()>,
    tail: Tail,
}

/// A running agent process with its pipes.
pub(crate) struct AgentProcess {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: ChildStdout,
    pub(crate) stderr: StderrCapture,
}

/// Start the agent. The environment is built from nothing (`env_clear`) plus [`agent_env`].
pub(crate) fn spawn(
    profile: &AgentProfile,
    program: &Path,
    events: mpsc::Sender<AgentEvent>,
) -> Result<AgentProcess, AcpError> {
    let mut command = Command::new(program);
    command
        .args(&profile.args)
        .env_clear()
        .envs(agent_env(profile, std::env::vars_os()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            AcpError::AgentNotFound {
                profile: profile.name.clone(),
                program: profile.command.clone(),
                install_hint: install_hint(profile),
            }
        } else {
            AcpError::Spawn {
                profile: profile.name.clone(),
                source,
            }
        }
    })?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        // Cannot happen with piped stdio; do not leave the process behind if it does.
        let _ = child.start_kill();
        return Err(AcpError::Spawn {
            profile: profile.name.clone(),
            source: std::io::Error::other("the agent's standard streams are not available"),
        });
    };
    let tail: Tail = Arc::new(Mutex::new(VecDeque::new()));
    let task = tokio::spawn(read_stderr(stderr, events, Arc::clone(&tail)));
    Ok(AgentProcess {
        child,
        stdin,
        stdout,
        stderr: StderrCapture { task, tail },
    })
}

/// The install hint of a profile, or a generic one.
pub(crate) fn install_hint(profile: &AgentProfile) -> String {
    profile
        .install_hint
        .clone()
        .unwrap_or_else(|| "install the agent and make sure it is on PATH".to_owned())
}

async fn read_stderr(stderr: ChildStderr, events: mpsc::Sender<AgentEvent>, tail: Tail) {
    let mut reader = BufReader::new(stderr);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&buf);
        let line: String = text
            .trim_end_matches(['\n', '\r'])
            .chars()
            .take(MAX_LINE_CHARS)
            .collect();
        {
            let mut guard = tail.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.len() == TAIL_LINES {
                guard.pop_front();
            }
            guard.push_back(line.clone());
        }
        // A closed or slow event channel must not stop the capture of the tail.
        let _ = events.send(AgentEvent::Stderr(line)).await;
    }
}

/// Reap the child and report. The caller has already dropped the child's `stdin` (EOF is the polite request).
/// After `grace` the process is killed; `wait` always runs, so no zombie is left.
pub(crate) async fn shutdown(mut child: Child, stderr: StderrCapture, grace: Duration) -> ExitInfo {
    let status = if let Ok(status) = tokio::time::timeout(grace, child.wait()).await {
        status.ok()
    } else {
        let _ = child.start_kill();
        child.wait().await.ok()
    };
    stderr.finish(status).await
}

impl StderrCapture {
    /// Build the exit report once the process has been reaped.
    pub(crate) async fn finish(self, status: Option<ExitStatus>) -> ExitInfo {
        let Self { task, tail } = self;
        let abort = task.abort_handle();
        if tokio::time::timeout(STDERR_DRAIN, task).await.is_err() {
            // A grandchild may keep the pipe open; the tail collected so far is enough.
            abort.abort();
        }
        let stderr_tail = tail
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        let (code, signal) = status.map_or((None, None), |s| (s.code(), signal_of(s)));
        ExitInfo {
            code,
            signal,
            stderr_tail,
        }
    }
}

#[cfg(unix)]
fn signal_of(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: ExitStatus) -> Option<i32> {
    None
}
