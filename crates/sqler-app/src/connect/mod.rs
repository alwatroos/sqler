// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The connect pipeline (ADR-0016): secret, SSH tunnel, TLS and target mapping, driver registry.
//!
//! [`Pipeline`] implements the UI's `Connector`. `sqler-ui` never sees `sqler-tunnel` or a driver
//! crate; everything that does I/O for a connection lives here.
//!
//! Order of one attempt: resolve the secrets (keychain reads on a blocking thread), build the
//! `ConnectionConfig`, refuse `verify-full` through a tunnel before anything is opened, open the
//! tunnel, point the target at its local port, put the database password into the in-memory
//! target and connect through the registry. The password never reaches a stored or printed target.

mod session;
mod target;
mod tunnel;

use std::path::PathBuf;
use std::sync::Arc;

use sqler_config::{ConnectionSpec, Secret, SecretKind, SecretStore, SshAuth, TlsMode};
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};
use sqler_driver_api::{BoxFuture, DriverRegistry, Session};
use sqler_tunnel::{Secret as TunnelSecret, SshAuth as TunnelAuth, TunnelConfig};
use sqler_ui::connections::{
    ConnectFailure, ConnectRequest, Connector, HostKeyPrompt, SecretSource,
};
use tokio::runtime::Handle;

use self::session::{TunnelGuard, TunnelledSession};
use self::target::UrlTarget;
pub(crate) use self::tunnel::SshTunnels;
use self::tunnel::{TunnelOpener, host_key_info, map_tunnel_error};

/// Connects saved or unsaved connections.
pub(crate) struct Pipeline {
    registry: Arc<DriverRegistry>,
    secrets: Arc<dyn SecretStore>,
    tunnels: Arc<dyn TunnelOpener>,
    known_hosts: Option<PathBuf>,
    handle: Handle,
}

impl Pipeline {
    pub(crate) fn new(
        registry: Arc<DriverRegistry>,
        secrets: Arc<dyn SecretStore>,
        tunnels: Arc<dyn TunnelOpener>,
        known_hosts: Option<PathBuf>,
        handle: Handle,
    ) -> Self {
        Self {
            registry,
            secrets,
            tunnels,
            known_hosts,
            handle,
        }
    }

    fn known_hosts(&self) -> Result<PathBuf, ConnectFailure> {
        self.known_hosts
            .clone()
            .ok_or_else(|| ConnectFailure::Tunnel("no home directory for known_hosts".to_owned()))
    }
}

impl Connector for Pipeline {
    fn driver_ids(&self) -> Vec<DriverId> {
        self.registry.ids().cloned().collect()
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
        let registry = Arc::clone(&self.registry);
        let secrets = Arc::clone(&self.secrets);
        let tunnels = Arc::clone(&self.tunnels);
        let known_hosts = self.known_hosts();
        let handle = self.handle.clone();
        Box::pin(async move {
            let ConnectRequest {
                spec,
                db_password,
                ssh_secret,
            } = request;
            let (db_password, ssh_secret) = resolve(secrets, db_password, ssh_secret).await?;
            let mut config = core_config(&spec);
            let Some(ssh) = spec.ssh.as_ref() else {
                if let Some(password) = &db_password {
                    config.target = UrlTarget::parse(&spec.target)?
                        .with_password(password)?
                        .render();
                }
                return Ok(registry.connect(&config).await?);
            };

            let target = UrlTarget::parse(&spec.target)?;
            let verify_full = match &spec.tls {
                Some(tls) => tls.mode == TlsMode::VerifyFull,
                None => target.wants_verify_full(),
            };
            if verify_full {
                return Err(ConnectFailure::TlsHostnameThroughTunnel {
                    host: target.host().to_owned(),
                });
            }
            let auth = tunnel_auth(&ssh.auth, ssh_secret)?;
            let tunnel_config = TunnelConfig::new(
                ssh.host.clone(),
                ssh.user.clone(),
                target.host(),
                target.port(),
                known_hosts?,
            )
            .with_ssh_port(ssh.port);
            let opened = tunnels
                .open(tunnel_config, auth)
                .await
                .map_err(map_tunnel_error)?;
            let port = opened.local_addr().port();
            // From here on a dropped future hands the tunnel back to the runtime.
            let mut guard = TunnelGuard::new(opened, handle);

            let rewritten = target.with_host_port("127.0.0.1", port);
            let rewritten = match &db_password {
                Some(password) => rewritten.with_password(password),
                None => Ok(rewritten),
            };
            let rewritten = match rewritten {
                Ok(target) => target,
                Err(failure) => {
                    let _ = guard.close().await;
                    return Err(failure);
                }
            };
            config.target = rewritten.render();
            match registry.connect(&config).await {
                Ok(inner) => Ok(Box::new(TunnelledSession::new(inner, guard)) as Box<dyn Session>),
                Err(error) => {
                    let _ = guard.close().await;
                    Err(error.into())
                }
            }
        })
    }

    fn trust_host_key(&self, key: HostKeyPrompt) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        let known_hosts = self.known_hosts();
        Box::pin(async move {
            let path = known_hosts?;
            let info = host_key_info(key);
            tokio::task::spawn_blocking(move || sqler_tunnel::trust_host_key(&path, &info))
                .await
                .map_err(|_| ConnectFailure::Aborted)?
                .map_err(map_tunnel_error)
        })
    }
}

/// The `ConnectionConfig` for a spec (the tunnel and the keychain flag are not part of it).
fn core_config(spec: &ConnectionSpec) -> ConnectionConfig {
    let mut config =
        ConnectionConfig::new(spec.name.clone(), spec.driver.clone(), spec.target.clone());
    if let Some(color) = &spec.color {
        config = config.with_color(color.clone());
    }
    if let Some(environment) = spec.environment {
        config = config.with_environment(environment);
    }
    if let Some(tls) = &spec.tls {
        config = config.with_tls(tls.to_core());
    }
    config
}

/// Maps the profile's SSH auth and the resolved secret to the tunnel's auth.
fn tunnel_auth(auth: &SshAuth, secret: Option<Secret>) -> Result<TunnelAuth, ConnectFailure> {
    let secret = secret.map(|s| TunnelSecret::new(s.expose()));
    match auth {
        SshAuth::Password => {
            secret
                .map(TunnelAuth::Password)
                .ok_or(ConnectFailure::MissingSecret {
                    kind: SecretKind::SshPassword,
                })
        }
        SshAuth::KeyFile { path, .. } => Ok(TunnelAuth::KeyFile {
            path: path.clone(),
            passphrase: secret,
        }),
        // The agent needs no secret; whatever was resolved is dropped.
        SshAuth::Agent => Ok(TunnelAuth::Agent { socket: None }),
        _ => Err(ConnectFailure::Tunnel(
            "unsupported SSH authentication method".to_owned(),
        )),
    }
}

/// Resolves both sources with one blocking call (the OS keychain blocks).
async fn resolve(
    store: Arc<dyn SecretStore>,
    db: SecretSource,
    ssh: SecretSource,
) -> Result<(Option<Secret>, Option<Secret>), ConnectFailure> {
    if matches!(
        (&db, &ssh),
        (SecretSource::Keychain(_), _) | (_, SecretSource::Keychain(_))
    ) {
        tokio::task::spawn_blocking(move || Ok((read(&*store, db)?, read(&*store, ssh)?)))
            .await
            .map_err(|_| ConnectFailure::Aborted)?
    } else {
        Ok((value(db), value(ssh)))
    }
}

fn value(source: SecretSource) -> Option<Secret> {
    match source {
        SecretSource::Value(secret) => Some(secret),
        _ => None,
    }
}

fn read(store: &dyn SecretStore, source: SecretSource) -> Result<Option<Secret>, ConnectFailure> {
    match source {
        SecretSource::Keychain(reference) => match store.get(&reference) {
            Ok(Some(secret)) => Ok(Some(secret)),
            Ok(None) => Err(ConnectFailure::MissingSecret {
                kind: reference.kind(),
            }),
            Err(error) => Err(ConnectFailure::Secret(error)),
        },
        other => Ok(value(other)),
    }
}

#[cfg(test)]
mod tests;
