// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ACP` client that spawns and hosts agents such as Claude Code, Codex CLI and Gemini CLI inside `SQLer`.
//!
//! A headless client of the Agent Client Protocol: [`AgentHandle::launch`] starts an agent process from an
//! [`AgentProfile`], runs `initialize`, and returns a handle plus an [`AgentEvents`] stream. Sessions
//! ([`AgentHandle::new_session`]) receive `MCP` server descriptors ([`McpServerDescriptor`]) so the agent can use
//! `SQLer`'s tools; prompts stream their updates as [`AgentEvent`]s, permission requests go to an [`AcpHost`].
//!
//! The agent gets a scrubbed environment (see [`agent_env`]) and no client capabilities: file system and
//! terminal methods are not offered. The process is killed and reaped on every exit path. The design is recorded
//! in ADR-0041. Everything is `Send`; the crate needs a tokio runtime with the `time` and `process` drivers.

#![forbid(unsafe_code)]

mod client;
mod connection;
mod descriptor;
mod error;
mod process;
mod profile;
mod types;

pub use client::{AgentEvents, AgentHandle, ExitWatch, LaunchOptions};
pub use descriptor::{MCP_AUTH_HEADER, McpServerDescriptor, McpToken};
pub use error::AcpError;
pub use profile::{AgentProfile, agent_env, builtin_profiles, resolve_program};
pub use types::{
    AcpHost, AgentEvent, AgentInfo, ExitInfo, PermissionDecision, PermissionOption,
    PermissionOptionKind, PermissionRequest, PlanEntry, SessionId, SessionUpdate, StopReason,
    ToolCallInfo, ToolCallStatus,
};
