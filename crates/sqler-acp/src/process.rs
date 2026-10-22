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
/// Most bytes of one stderr line held in memory (four bytes per character at most).
const MAX_LINE_BYTES: usize = MAX_LINE_CHARS * 4;
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

/// Splits stderr bytes into lines on `\n` and `\r` while holding at most `MAX_LINE_BYTES` of a line in
/// memory. The rest of an over-long line is discarded up to the next delimiter.
#[derive(Default)]
struct LineSplitter {
    buf: Vec<u8>,
    /// The line is over the limit; skip bytes until a delimiter.
    overflow: bool,
    /// The previous byte was `\r`, so a following `\n` is the second half of `\r\n`.
    after_cr: bool,
}

impl LineSplitter {
    fn feed(&mut self, bytes: &[u8], mut line: impl FnMut(String)) {
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                let skip = byte == b'\n' && self.after_cr;
                // An empty segment from `\r` (a spinner redraw) is noise; an empty `\n` line is kept.
                let keep = !skip && (byte == b'\n' || !self.buf.is_empty());
                self.after_cr = byte == b'\r';
                if keep {
                    line(self.take());
                }
                self.buf.clear();
                self.overflow = false;
                continue;
            }
            self.after_cr = false;
            if self.overflow {
                continue;
            }
            if self.buf.len() >= MAX_LINE_BYTES {
                self.overflow = true;
            } else {
                self.buf.push(byte);
            }
        }
    }

    /// The last line when the stream ends without a delimiter.
    fn finish(&mut self, line: impl FnOnce(String)) {
        if !self.buf.is_empty() {
            line(self.take());
        }
    }

    fn take(&mut self) -> String {
        String::from_utf8_lossy(&self.buf)
            .chars()
            .take(MAX_LINE_CHARS)
            .collect()
    }
}

async fn read_stderr(stderr: ChildStderr, events: mpsc::Sender<AgentEvent>, tail: Tail) {
    let mut reader = BufReader::new(stderr);
    let mut splitter = LineSplitter::default();
    let record = |line: String| {
        {
            let mut guard = tail.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.len() == TAIL_LINES {
                guard.pop_front();
            }
            guard.push_back(line.clone());
        }
        // The tail is already kept, so the live event is best effort: when nobody drains the 256-slot
        // channel the line is dropped rather than stalling the reader and filling the agent's pipe.
        let _ = events.try_send(AgentEvent::Stderr(line));
    };
    loop {
        let chunk = match reader.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(chunk) => chunk,
        };
        let n = chunk.len();
        splitter.feed(chunk, &record);
        reader.consume(n);
    }
    splitter.finish(record);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<String> {
        let mut out = Vec::new();
        let mut splitter = LineSplitter::default();
        for chunk in chunks {
            splitter.feed(chunk, |l| out.push(l));
        }
        splitter.finish(|l| out.push(l));
        out
    }

    #[test]
    fn splits_on_newline_and_carriage_return() {
        assert_eq!(
            split(&[b"a\nb\r", b"\nc\rd\r\r", b"e"]),
            ["a", "b", "c", "d", "e"]
        );
        assert_eq!(split(&[b"x\n\ny\n"]), ["x", "", "y"]);
    }

    #[test]
    fn a_huge_line_is_bounded_and_the_next_line_is_intact() {
        let huge = vec![b'z'; MAX_LINE_BYTES * 50];
        let mut splitter = LineSplitter::default();
        let mut out = Vec::new();
        splitter.feed(&huge, |l| out.push(l));
        assert!(splitter.buf.len() <= MAX_LINE_BYTES);
        splitter.feed(b"\nok\n", |l| out.push(l));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].chars().count(), MAX_LINE_CHARS);
        assert_eq!(out[1], "ok");
    }

    #[test]
    fn endless_carriage_returns_hold_nothing() {
        let mut splitter = LineSplitter::default();
        splitter.feed(&b"\r".repeat(1_000_000), |_| panic!("no line"));
        assert_eq!(splitter.buf.len(), 0);
    }
}
