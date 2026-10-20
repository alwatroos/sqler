// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! SSH session and local port forward for `SQLer` (L1).
//!
//! [`Tunnel::open`] connects to an SSH bastion, verifies the host key against a `known_hosts`
//! file, authenticates and binds a loopback port. Every connection to that port is forwarded to
//! the target host through the bastion, so a database driver only sees a local address.
//!
//! Host keys are never accepted silently: an unknown key is [`TunnelError::UnknownHostKey`] and a
//! changed key is [`TunnelError::HostKeyChanged`]. After the user confirmed the fingerprint,
//! [`trust_host_key`] records it. Secrets travel as [`Secret`] values that print as `***`.
//!
//! RSA user keys (2048, 3072 or 4096 bits; `rsa-sha2-256` and `rsa-sha2-512`) are signed with
//! `ring`; the `rsa` crate is not used (ADR-0018).
//!
//! `Tunnel::open` must run inside a tokio runtime.

#![forbid(unsafe_code)]

mod auth;
mod config;
mod error;
mod host_key;
mod rsa_key;
mod tunnel;

pub use auth::{Secret, SshAuth};
pub use config::{TunnelConfig, default_known_hosts_path};
pub use error::TunnelError;
pub use host_key::{HostKeyInfo, trust_host_key};
pub use tunnel::Tunnel;
