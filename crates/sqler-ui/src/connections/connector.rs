// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The [`Connector`] trait: the only door from the UI to the connect pipeline.
//!
//! `sqler-app` implements it (secret, SSH tunnel, TLS mapping, driver registry). The futures run
//! on the tokio runtime, started by `SessionBridge`; dropping one cancels the attempt.

use sqler_config::{
    ConnectionProfile, ConnectionSpec, ProfileSecret, Secret, SecretError, SecretKind, SecretRef,
};
use sqler_driver_api::sqler_core::DriverId;
use sqler_driver_api::{BoxFuture, DriverError, Session};
use thiserror::Error;

/// Where a secret for one connect attempt comes from.
#[derive(Debug, Clone)]
pub enum SecretSource {
    /// No secret is used.
    None,
    /// Read it from the secret store under this reference.
    Keychain(SecretRef),
    /// The user just typed it (an unsaved form); it is not in the store.
    Value(Secret),
}

/// Everything the pipeline needs for one connect attempt.
///
/// `ssh_secret` is the SSH password for `SshAuth::Password`, the key passphrase for
/// `SshAuth::KeyFile` and ignored for `SshAuth::Agent`.
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    /// The connection settings (target, TLS, SSH, colour, environment).
    pub spec: ConnectionSpec,
    /// The database password.
    pub db_password: SecretSource,
    /// The SSH password or key passphrase.
    pub ssh_secret: SecretSource,
}

/// The keychain reference of a profile secret (fails only for an id outside the reference grammar).
pub(crate) fn from_profile_secret(secret: &ProfileSecret) -> Result<SecretRef, SecretError> {
    SecretRef::try_from(secret)
}

impl ConnectRequest {
    /// A request for a saved connection: secrets the profile keeps in the keychain become
    /// [`SecretSource::Keychain`], the rest [`SecretSource::None`].
    ///
    /// # Errors
    /// [`SecretError::InvalidRef`] when the connection id breaks the reference grammar.
    pub fn from_profile(profile: &ConnectionProfile) -> Result<Self, SecretError> {
        let source = |secret: Option<ProfileSecret>| match secret {
            Some(secret) => from_profile_secret(&secret).map(SecretSource::Keychain),
            None => Ok(SecretSource::None),
        };
        Ok(Self {
            spec: profile.spec().clone(),
            db_password: source(profile.password_ref())?,
            ssh_secret: source(profile.ssh_secret_ref())?,
        })
    }
}

/// An SSH host key the user has not trusted yet (or that changed), shown for confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyPrompt {
    /// The SSH server host.
    pub host: String,
    /// The SSH server port.
    pub port: u16,
    /// The key algorithm, for example `ssh-ed25519`.
    pub algorithm: String,
    /// The fingerprint to compare with the one the server administrator publishes.
    pub fingerprint: String,
    /// The public key in `known_hosts` format.
    pub public_key: String,
}

/// Why a connect attempt produced no session. Never contains a secret or the raw target.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConnectFailure {
    /// The connection is set to use a stored secret but the store has none.
    #[error("no {} stored in the keychain for this connection; edit it to set one", .kind.as_id())]
    MissingSecret {
        /// Which secret is missing.
        kind: SecretKind,
    },
    /// The secret store failed.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The SSH server's host key is not in `known_hosts` yet.
    #[error("the SSH host key of {}:{} is not trusted yet", .0.host, .0.port)]
    UnknownHostKey(HostKeyPrompt),
    /// The SSH server's host key differs from the one in `known_hosts`.
    #[error(
        "the SSH host key of {}:{} changed (known_hosts line {line}); this can be an attack, check it with the server administrator",
        .key.host, .key.port
    )]
    HostKeyChanged {
        /// The key the server presented now.
        key: HostKeyPrompt,
        /// The `known_hosts` line that holds the old key.
        line: usize,
    },
    /// The SSH tunnel failed.
    #[error("SSH tunnel failed: {0}")]
    Tunnel(String),
    /// The target could not be used.
    #[error("invalid connection target: {0}")]
    Target(String),
    /// `verify-full` TLS cannot be checked against the real host through the tunnel yet.
    #[error(
        "verify-full TLS through an SSH tunnel is not supported yet; use verify-ca or connect without the tunnel"
    )]
    TlsHostnameThroughTunnel {
        /// The database host the certificate would have to match.
        host: String,
    },
    /// The driver (or the registry) failed.
    #[error(transparent)]
    Driver(#[from] DriverError),
    /// The attempt was aborted before it finished.
    #[error("connect aborted")]
    Aborted,
}

/// The connect pipeline as the UI sees it.
pub trait Connector: Send + Sync {
    /// The ids of the drivers that can be connected to, sorted.
    fn driver_ids(&self) -> Vec<DriverId>;

    /// Connects: secret, tunnel, driver. Dropping the future cancels the attempt.
    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>>;

    /// Adds the host key the user confirmed to `known_hosts`.
    fn trust_host_key(&self, key: HostKeyPrompt) -> BoxFuture<'static, Result<(), ConnectFailure>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::{ConnectionStore, SshAuth, SshProfile};

    fn driver() -> DriverId {
        DriverId::new("postgres").unwrap()
    }

    fn profile(spec: ConnectionSpec) -> ConnectionProfile {
        let mut store = ConnectionStore::new();
        let id = store.add_connection(spec, None).unwrap();
        store.connection(&id).unwrap().clone()
    }

    #[test]
    fn plain_profile_uses_no_secret() {
        let p = profile(ConnectionSpec::new("a", driver(), "postgres://h/d?user=u"));
        let r = ConnectRequest::from_profile(&p).unwrap();
        assert!(matches!(r.db_password, SecretSource::None));
        assert!(matches!(r.ssh_secret, SecretSource::None));
        assert_eq!(r.spec.target, "postgres://h/d?user=u");
    }

    #[test]
    fn keychain_flags_become_references() {
        let ssh = SshProfile::new("bastion", "me", SshAuth::Password);
        let p = profile(
            ConnectionSpec::new("My DB", driver(), "postgres://h/d?user=u")
                .with_password_in_keychain(true)
                .with_ssh(ssh),
        );
        let r = ConnectRequest::from_profile(&p).unwrap();
        let SecretSource::Keychain(db) = r.db_password else {
            panic!("expected a keychain reference");
        };
        assert_eq!(db.to_string(), "my-db:db-password");
        let SecretSource::Keychain(ssh) = r.ssh_secret else {
            panic!("expected a keychain reference");
        };
        assert_eq!(ssh.to_string(), "my-db:ssh-password");
    }

    #[test]
    fn key_passphrase_reference_and_agent_none() {
        let key = SshAuth::KeyFile {
            path: "/k".into(),
            passphrase_in_keychain: true,
        };
        let p = profile(
            ConnectionSpec::new("k", driver(), "postgres://h/d?user=u")
                .with_ssh(SshProfile::new("b", "me", key)),
        );
        let r = ConnectRequest::from_profile(&p).unwrap();
        let SecretSource::Keychain(r) = r.ssh_secret else {
            panic!("expected a keychain reference");
        };
        assert_eq!(r.kind(), SecretKind::SshKeyPassphrase);
        let p =
            profile(
                ConnectionSpec::new("g", driver(), "postgres://h/d?user=u")
                    .with_ssh(SshProfile::new("b", "me", SshAuth::Agent)),
            );
        let r = ConnectRequest::from_profile(&p).unwrap();
        assert!(matches!(r.ssh_secret, SecretSource::None));
    }

    fn prompt() -> HostKeyPrompt {
        HostKeyPrompt {
            host: "bastion.example".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        }
    }

    #[test]
    fn failure_messages_are_stable_and_secret_free() {
        let secret = "hunter2";
        let ref_ = SecretRef::new("c1", SecretKind::DatabasePassword).unwrap();
        let cases: Vec<(ConnectFailure, &str)> = vec![
            (
                ConnectFailure::MissingSecret {
                    kind: SecretKind::SshPassword,
                },
                "no ssh-password stored in the keychain for this connection; edit it to set one",
            ),
            (
                ConnectFailure::Secret(SecretError::Unavailable {
                    reference: ref_,
                    reason: "locked".into(),
                }),
                "keychain unavailable for c1:db-password: locked",
            ),
            (
                ConnectFailure::UnknownHostKey(prompt()),
                "the SSH host key of bastion.example:22 is not trusted yet",
            ),
            (
                ConnectFailure::HostKeyChanged {
                    key: prompt(),
                    line: 7,
                },
                "the SSH host key of bastion.example:22 changed (known_hosts line 7); this can be an attack, check it with the server administrator",
            ),
            (
                ConnectFailure::Tunnel("refused".into()),
                "SSH tunnel failed: refused",
            ),
            (
                ConnectFailure::Target("no host".into()),
                "invalid connection target: no host",
            ),
            (
                ConnectFailure::TlsHostnameThroughTunnel {
                    host: "db.internal".into(),
                },
                "verify-full TLS through an SSH tunnel is not supported yet; use verify-ca or connect without the tunnel",
            ),
            (
                ConnectFailure::Driver(DriverError::Connection("refused".into())),
                "connection failed: refused",
            ),
            (ConnectFailure::Aborted, "connect aborted"),
        ];
        for (failure, text) in cases {
            assert_eq!(failure.to_string(), text);
            assert!(!failure.to_string().contains(secret));
            assert!(!failure.to_string().contains("AAAA"));
        }
    }

    #[test]
    fn request_debug_hides_secret_values() {
        let r = ConnectRequest {
            spec: ConnectionSpec::new("a", driver(), "postgres://h/d?user=u"),
            db_password: SecretSource::Value(Secret::new("hunter2")),
            ssh_secret: SecretSource::None,
        };
        assert!(!format!("{r:?}").contains("hunter2"));
    }
}
