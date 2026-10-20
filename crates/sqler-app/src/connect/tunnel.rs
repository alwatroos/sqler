// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The SSH tunnel as the pipeline sees it: a small trait over `sqler-tunnel` so tests can use a
//! fake, and the mapping of tunnel errors to [`ConnectFailure`].

use std::net::SocketAddr;

use sqler_driver_api::BoxFuture;
use sqler_tunnel::{HostKeyInfo, SshAuth, Tunnel, TunnelConfig, TunnelError};
use sqler_ui::connections::{ConnectFailure, HostKeyPrompt};

/// Opens tunnels.
pub(crate) trait TunnelOpener: Send + Sync {
    /// Connects to the bastion, verifies its host key, authenticates and binds a loopback port.
    fn open(
        &self,
        config: TunnelConfig,
        auth: SshAuth,
    ) -> BoxFuture<'static, Result<Box<dyn TunnelHandle>, TunnelError>>;
}

/// One open tunnel.
pub(crate) trait TunnelHandle: Send {
    /// The loopback address that forwards to the target.
    fn local_addr(&self) -> SocketAddr;

    /// Closes the tunnel.
    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), TunnelError>>;
}

/// The real tunnels (`sqler-tunnel`).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SshTunnels;

struct SshHandle(Tunnel);

impl TunnelHandle for SshHandle {
    fn local_addr(&self) -> SocketAddr {
        self.0.local_addr()
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), TunnelError>> {
        Box::pin(async move { self.0.close().await })
    }
}

impl TunnelOpener for SshTunnels {
    fn open(
        &self,
        config: TunnelConfig,
        auth: SshAuth,
    ) -> BoxFuture<'static, Result<Box<dyn TunnelHandle>, TunnelError>> {
        Box::pin(async move {
            let tunnel = Tunnel::open(&config, &auth).await?;
            Ok(Box::new(SshHandle(tunnel)) as Box<dyn TunnelHandle>)
        })
    }
}

fn prompt(key: HostKeyInfo) -> HostKeyPrompt {
    HostKeyPrompt {
        host: key.host,
        port: key.port,
        algorithm: key.algorithm,
        fingerprint: key.fingerprint,
        public_key: key.public_key,
    }
}

/// The host key the user confirmed, as `sqler-tunnel` wants it.
pub(crate) fn host_key_info(key: HostKeyPrompt) -> HostKeyInfo {
    HostKeyInfo {
        host: key.host,
        port: key.port,
        algorithm: key.algorithm,
        fingerprint: key.fingerprint,
        public_key: key.public_key,
    }
}

/// Unknown and changed host keys become the prompt variants, everything else a text.
pub(crate) fn map_tunnel_error(error: TunnelError) -> ConnectFailure {
    match error {
        TunnelError::UnknownHostKey(key) => ConnectFailure::UnknownHostKey(prompt(key)),
        TunnelError::HostKeyChanged { key, line } => ConnectFailure::HostKeyChanged {
            key: prompt(key),
            line,
        },
        other => ConnectFailure::Tunnel(other.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A scripted tunnel opener for tests.

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use super::*;

    /// Records what was opened and closed.
    #[derive(Default)]
    pub(crate) struct FakeTunnels {
        pub(crate) opened: Mutex<Vec<(TunnelConfig, SshAuth)>>,
        pub(crate) closed: Arc<AtomicUsize>,
        pub(crate) fail: Mutex<Option<TunnelError>>,
        pub(crate) port: u16,
    }

    impl FakeTunnels {
        pub(crate) fn new(port: u16) -> Self {
            Self {
                port,
                ..Self::default()
            }
        }

        pub(crate) fn closed(&self) -> usize {
            self.closed.load(Ordering::SeqCst)
        }

        pub(crate) fn opened(&self) -> usize {
            self.opened
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
        }
    }

    struct FakeHandle {
        port: u16,
        closed: Arc<AtomicUsize>,
    }

    impl TunnelHandle for FakeHandle {
        fn local_addr(&self) -> SocketAddr {
            SocketAddr::from(([127, 0, 0, 1], self.port))
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), TunnelError>> {
            self.closed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    impl TunnelOpener for FakeTunnels {
        fn open(
            &self,
            config: TunnelConfig,
            auth: SshAuth,
        ) -> BoxFuture<'static, Result<Box<dyn TunnelHandle>, TunnelError>> {
            self.opened
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((config, auth));
            let failure = self
                .fail
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let handle = FakeHandle {
                port: self.port,
                closed: Arc::clone(&self.closed),
            };
            Box::pin(async move {
                match failure {
                    Some(error) => Err(error),
                    None => Ok(Box::new(handle) as Box<dyn TunnelHandle>),
                }
            })
        }
    }

    pub(crate) fn handle(port: u16, closed: &Arc<AtomicUsize>) -> Box<dyn TunnelHandle> {
        Box::new(FakeHandle {
            port,
            closed: Arc::clone(closed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> HostKeyInfo {
        HostKeyInfo {
            host: "bastion".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        }
    }

    #[test]
    fn host_key_errors_become_prompts() {
        let f = map_tunnel_error(TunnelError::UnknownHostKey(key()));
        let ConnectFailure::UnknownHostKey(p) = f else {
            panic!("expected UnknownHostKey");
        };
        assert_eq!((p.host.as_str(), p.port), ("bastion", 22));
        assert_eq!(p.fingerprint, "SHA256:abc");
        assert_eq!(host_key_info(p), key());
        let f = map_tunnel_error(TunnelError::HostKeyChanged {
            key: key(),
            line: 4,
        });
        assert!(matches!(f, ConnectFailure::HostKeyChanged { line: 4, .. }));
    }

    #[test]
    fn other_errors_become_text() {
        let f = map_tunnel_error(TunnelError::AuthFailed);
        assert_eq!(
            f.to_string(),
            "SSH tunnel failed: SSH authentication failed"
        );
        let f = map_tunnel_error(TunnelError::Timeout);
        assert!(matches!(f, ConnectFailure::Tunnel(_)));
    }
}
