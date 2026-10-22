// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The error type of the `ACP` client.
//!
//! Texts are written for the user. They never contain an `MCP` token, an `env` value or an argument list.

use crate::types::ExitInfo;

/// Everything that can go wrong while launching or driving an agent.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AcpError {
    /// The agent program is not on `PATH` (checked before anything is spawned).
    #[error("agent \"{profile}\" not found: \"{program}\" is not on PATH; install: {install_hint}")]
    AgentNotFound {
        /// Display name of the profile.
        profile: String,
        /// The program that was looked up.
        program: String,
        /// How to install it (a generic hint when the profile has none).
        install_hint: String,
    },
    /// The operating system refused to start the agent process.
    #[error("cannot start agent \"{profile}\": {source}")]
    Spawn {
        /// Display name of the profile.
        profile: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The profile is malformed (empty command, bad id).
    #[error("invalid agent profile: {0}")]
    InvalidProfile(String),
    /// The agent does not speak a protocol version `SQLer` supports.
    #[error("agent \"{profile}\" speaks ACP version {version}, which SQLer does not support")]
    UnsupportedProtocol {
        /// Display name of the profile.
        profile: String,
        /// The version the agent answered with.
        version: u16,
    },
    /// A step took longer than its limit.
    #[error("agent \"{profile}\" did not answer in time during {step}")]
    Timeout {
        /// Display name of the profile.
        profile: String,
        /// The step that timed out.
        step: &'static str,
    },
    /// The agent process ended while a call was pending, or before it was ready.
    #[error("agent \"{profile}\" exited unexpectedly ({exit})")]
    AgentExited {
        /// Display name of the profile.
        profile: String,
        /// How the process ended, with the tail of its stderr.
        exit: ExitInfo,
    },
    /// The agent answered a request with a JSON-RPC error.
    #[error("agent error in {method}: {message}")]
    Agent {
        /// The JSON-RPC method.
        method: &'static str,
        /// The message the agent sent.
        message: String,
    },
    /// The descriptor needs an `MCP` transport the agent did not advertise.
    #[error("agent \"{profile}\" does not support MCP over {transport}")]
    McpTransportUnsupported {
        /// Display name of the profile.
        profile: String,
        /// The missing transport (`http`).
        transport: &'static str,
    },
    /// An `MCP` descriptor is not acceptable (for example a non-loopback URL).
    #[error("invalid MCP server descriptor: {0}")]
    InvalidDescriptor(String),
    /// The working directory of a session must be an absolute path.
    #[error("the session working directory must be an absolute path")]
    InvalidCwd,
    /// A session runs one prompt at a time.
    #[error("a prompt is already running in this session")]
    PromptInProgress,
    /// The session id is not known to this agent connection.
    #[error("unknown session")]
    UnknownSession,
    /// The handle was closed.
    #[error("the agent connection is closed")]
    Closed,
}
