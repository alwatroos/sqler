// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::path::PathBuf;
use std::time::Duration;

use crate::error::TunnelError;

/// Where to connect and what to forward.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TunnelConfig {
    /// SSH server (bastion) host.
    pub ssh_host: String,
    /// SSH server port.
    pub ssh_port: u16,
    /// SSH user.
    pub user: String,
    /// Host the bastion connects to (the database host), as the bastion resolves it.
    pub target_host: String,
    /// Port on the target host.
    pub target_port: u16,
    /// Local loopback port; `0` picks a free one.
    pub local_port: u16,
    /// The `known_hosts` file used to verify the server key.
    pub known_hosts: PathBuf,
    /// Upper bound for connect, handshake, authentication and bind.
    pub connect_timeout: Duration,
}

impl TunnelConfig {
    /// A configuration with SSH port 22, an ephemeral local port and a 15 s timeout.
    pub fn new(
        ssh_host: impl Into<String>,
        user: impl Into<String>,
        target_host: impl Into<String>,
        target_port: u16,
        known_hosts: PathBuf,
    ) -> Self {
        Self {
            ssh_host: ssh_host.into(),
            ssh_port: 22,
            user: user.into(),
            target_host: target_host.into(),
            target_port,
            local_port: 0,
            known_hosts,
            connect_timeout: Duration::from_secs(15),
        }
    }

    /// Sets the SSH server port.
    #[must_use]
    pub fn with_ssh_port(mut self, p: u16) -> Self {
        self.ssh_port = p;
        self
    }

    /// Sets the local loopback port (`0` = ephemeral).
    #[must_use]
    pub fn with_local_port(mut self, p: u16) -> Self {
        self.local_port = p;
        self
    }

    /// Sets the open timeout.
    #[must_use]
    pub fn with_connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    /// Checks the fields.
    ///
    /// # Errors
    /// [`TunnelError::InvalidConfig`] for an empty host, user or target, a zero SSH or target
    /// port, or a zero timeout.
    pub fn validate(&self) -> Result<(), TunnelError> {
        let bad = |m: &str| Err(TunnelError::InvalidConfig(m.into()));
        if self.ssh_host.trim().is_empty() {
            return bad("SSH host is empty");
        }
        if self.user.trim().is_empty() {
            return bad("SSH user is empty");
        }
        if self.target_host.trim().is_empty() {
            return bad("target host is empty");
        }
        if self.ssh_port == 0 {
            return bad("SSH port is 0");
        }
        if self.target_port == 0 {
            return bad("target port is 0");
        }
        if self.connect_timeout.is_zero() {
            return bad("connect timeout is zero");
        }
        Ok(())
    }
}

/// The default `known_hosts` path: `$HOME/.ssh/known_hosts`, or `%USERPROFILE%\.ssh\known_hosts`
/// on Windows. `None` when neither variable is set.
#[must_use]
pub fn default_known_hosts_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()))?;
    Some(PathBuf::from(home).join(".ssh").join("known_hosts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TunnelConfig {
        TunnelConfig::new("bastion", "me", "db.internal", 5432, PathBuf::from("kh"))
    }

    #[test]
    fn defaults() {
        let c = cfg();
        assert_eq!(c.ssh_port, 22);
        assert_eq!(c.local_port, 0);
        assert_eq!(c.connect_timeout, Duration::from_secs(15));
        assert!(c.validate().is_ok());
    }

    #[test]
    fn builders() {
        let c = cfg()
            .with_ssh_port(2222)
            .with_local_port(15432)
            .with_connect_timeout(Duration::from_secs(3));
        assert_eq!((c.ssh_port, c.local_port), (2222, 15432));
        assert_eq!(c.connect_timeout, Duration::from_secs(3));
    }

    #[test]
    fn validation_rejects_each_bad_field() {
        let mut c = cfg();
        c.ssh_host = " ".into();
        assert!(c.validate().is_err());
        let mut c = cfg();
        c.user = String::new();
        assert!(c.validate().is_err());
        let mut c = cfg();
        c.target_host = String::new();
        assert!(c.validate().is_err());
        let mut c = cfg();
        c.target_port = 0;
        assert!(c.validate().is_err());
        assert!(cfg().with_ssh_port(0).validate().is_err());
        assert!(
            cfg()
                .with_connect_timeout(Duration::ZERO)
                .validate()
                .is_err()
        );
        let err = cfg().with_ssh_port(0).validate().unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid tunnel configuration: SSH port is 0"
        );
    }

    #[test]
    fn default_known_hosts_shape() {
        if let Some(p) = default_known_hosts_path() {
            assert!(p.ends_with(".ssh/known_hosts") || p.ends_with(".ssh\\known_hosts"));
        }
    }
}
