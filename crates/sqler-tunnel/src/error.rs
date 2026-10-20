// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::path::PathBuf;

use thiserror::Error;

use crate::host_key::HostKeyInfo;

/// Errors from opening or running an SSH tunnel.
///
/// No variant carries a password, a passphrase or key bytes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TunnelError {
    /// The configuration is invalid.
    #[error("invalid tunnel configuration: {0}")]
    InvalidConfig(String),
    /// The TCP connection to the SSH server failed.
    #[error("cannot connect to {host}:{port}: {source}")]
    Connect {
        /// SSH server host.
        host: String,
        /// SSH server port.
        port: u16,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Opening the tunnel took longer than the configured timeout.
    #[error("timed out opening the SSH tunnel")]
    Timeout,
    /// The server key is not in `known_hosts`. Show the fingerprint to the user; after they
    /// confirm it, call [`trust_host_key`](crate::trust_host_key).
    #[error("unknown host key for {}:{} ({})", .0.host, .0.port, .0.fingerprint)]
    UnknownHostKey(HostKeyInfo),
    /// The server key differs from the one recorded in `known_hosts`.
    #[error(
        "host key for {}:{} changed (known_hosts line {line}, now {})",
        .key.host, .key.port, .key.fingerprint
    )]
    HostKeyChanged {
        /// The key the server presented now.
        key: HostKeyInfo,
        /// The line of the conflicting entry in `known_hosts` (1-based).
        line: usize,
    },
    /// The `known_hosts` file could not be read or written.
    #[error("known_hosts {}: {message}", .path.display())]
    KnownHosts {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// The private key file could not be loaded.
    #[error("private key {}: {message}", .path.display())]
    KeyFile {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// The private key is an RSA key, which this build does not support (RUSTSEC-2023-0071).
    #[error(
        "private key {} is an RSA key, which is not supported; use an ed25519 or ECDSA key",
        .path.display()
    )]
    UnsupportedKeyType {
        /// The file.
        path: PathBuf,
    },
    /// The private key is encrypted and no passphrase was given.
    #[error("private key {} is encrypted and needs a passphrase", .path.display())]
    KeyPassphraseRequired {
        /// The file.
        path: PathBuf,
    },
    /// The SSH agent cannot be reached.
    #[error("SSH agent unavailable: {0}")]
    AgentUnavailable(String),
    /// The server rejected the credentials.
    #[error("SSH authentication failed")]
    AuthFailed,
    /// The local listener could not be bound.
    #[error("cannot bind the local tunnel port: {0}")]
    Bind(#[source] std::io::Error),
    /// Any other SSH protocol error.
    #[error("SSH error: {0}")]
    Ssh(String),
}

impl From<russh::Error> for TunnelError {
    fn from(e: russh::Error) -> Self {
        Self::Ssh(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> HostKeyInfo {
        HostKeyInfo {
            host: "bastion".into(),
            port: 2222,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        }
    }

    #[test]
    fn display_strings() {
        assert_eq!(
            TunnelError::InvalidConfig("empty user".into()).to_string(),
            "invalid tunnel configuration: empty user"
        );
        assert_eq!(
            TunnelError::Timeout.to_string(),
            "timed out opening the SSH tunnel"
        );
        assert_eq!(
            TunnelError::UnknownHostKey(key()).to_string(),
            "unknown host key for bastion:2222 (SHA256:abc)"
        );
        assert_eq!(
            TunnelError::HostKeyChanged {
                key: key(),
                line: 3
            }
            .to_string(),
            "host key for bastion:2222 changed (known_hosts line 3, now SHA256:abc)"
        );
        assert_eq!(
            TunnelError::KeyPassphraseRequired {
                path: PathBuf::from("id")
            }
            .to_string(),
            "private key id is encrypted and needs a passphrase"
        );
        assert_eq!(
            TunnelError::UnsupportedKeyType {
                path: PathBuf::from("id")
            }
            .to_string(),
            "private key id is an RSA key, which is not supported; use an ed25519 or ECDSA key"
        );
        assert_eq!(
            TunnelError::AgentUnavailable("no socket".into()).to_string(),
            "SSH agent unavailable: no socket"
        );
        assert_eq!(
            TunnelError::AuthFailed.to_string(),
            "SSH authentication failed"
        );
        assert_eq!(
            TunnelError::Ssh("boom".into()).to_string(),
            "SSH error: boom"
        );
        let connect = TunnelError::Connect {
            host: "h".into(),
            port: 22,
            source: std::io::Error::other("refused"),
        };
        assert_eq!(connect.to_string(), "cannot connect to h:22: refused");
    }

    #[test]
    fn russh_error_maps_to_ssh() {
        let e: TunnelError = russh::Error::Disconnect.into();
        assert!(matches!(e, TunnelError::Ssh(_)));
    }
}
