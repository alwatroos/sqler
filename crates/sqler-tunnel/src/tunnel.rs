// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use russh::Disconnect;
use russh::client::{self, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::HashAlg;
use russh::keys::{Error as KeyError, PrivateKeyWithHashAlg, load_secret_key};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use crate::auth::{Secret, SshAuth};
use crate::config::TunnelConfig;
use crate::error::TunnelError;
use crate::host_key::HostKeyHandler;
use crate::rsa_key::{self, RsaSigner};

/// Keepalive interval of the SSH session. After three unanswered keepalives russh closes it.
const KEEPALIVE: Duration = Duration::from_secs(30);

/// Inactivity bound of the SSH session. It must exceed [`KEEPALIVE`]: keepalive replies reset it,
/// so an idle tunnel survives, while a session task left behind by a cancelled open (a server
/// that sent its banner and then stalled in key exchange) ends within this bound.
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(3 * 30);

/// The pipe of the Windows `OpenSSH` agent.
#[cfg(windows)]
const WINDOWS_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

type SshHandle = Handle<HostKeyHandler>;

/// An open SSH tunnel: an SSH session plus a loopback listener that forwards every connection to
/// the target host.
///
/// [`Tunnel::close`] ends it and waits for the teardown. Dropping the value also stops it, without
/// blocking. Dropping the future returned by [`Tunnel::open`] cancels the open: the TCP socket is
/// closed at once if the server has not yet sent its banner, otherwise russh's session task ends
/// at the latest after the inactivity timeout.
///
/// # Panics
/// [`Tunnel::open`] and dropping a `Tunnel` must happen inside a tokio runtime; tokio panics
/// otherwise.
pub struct Tunnel {
    local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), TunnelError>>>,
}

impl fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tunnel")
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

impl Tunnel {
    /// Connects to the SSH server, verifies its host key, authenticates and starts forwarding
    /// from a loopback port to `config.target_host:config.target_port`.
    ///
    /// The whole sequence is bounded by `config.connect_timeout`.
    ///
    /// # Errors
    /// [`TunnelError::InvalidConfig`] for a bad configuration, [`TunnelError::Timeout`],
    /// [`TunnelError::Connect`], [`TunnelError::UnknownHostKey`],
    /// [`TunnelError::HostKeyChanged`], the key and agent errors, [`TunnelError::AuthFailed`] and
    /// [`TunnelError::Bind`].
    pub async fn open(config: &TunnelConfig, auth: &SshAuth) -> Result<Self, TunnelError> {
        config.validate()?;
        tokio::time::timeout(
            config.connect_timeout,
            open_inner(config, auth, INACTIVITY_TIMEOUT),
        )
        .await
        .map_err(|_| TunnelError::Timeout)?
    }

    /// The loopback address drivers connect to.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops accepting, drops the open forwards, disconnects the SSH session and waits for the
    /// background task.
    ///
    /// # Errors
    /// The disconnect error, if the session could not be told to disconnect (for example because
    /// the server had already closed it).
    pub async fn close(mut self) -> Result<(), TunnelError> {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        match self.task.take() {
            Some(task) => task
                .await
                .map_err(|e| TunnelError::Ssh(format!("tunnel task failed: {e}")))?,
            None => Ok(()),
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn open_inner(
    config: &TunnelConfig,
    auth: &SshAuth,
    inactivity_timeout: Duration,
) -> Result<Tunnel, TunnelError> {
    let stream = TcpStream::connect((config.ssh_host.as_str(), config.ssh_port))
        .await
        .map_err(|source| TunnelError::Connect {
            host: config.ssh_host.clone(),
            port: config.ssh_port,
            source,
        })?;
    let _ = stream.set_nodelay(true);

    let ssh_config = Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE),
        inactivity_timeout: Some(inactivity_timeout),
        nodelay: true,
        ..client::Config::default()
    });
    let handler = HostKeyHandler {
        host: config.ssh_host.clone(),
        port: config.ssh_port,
        known_hosts: config.known_hosts.clone(),
    };
    let mut handle = client::connect_stream(ssh_config, stream, handler).await?;

    authenticate(&mut handle, &config.user, auth).await?;

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.local_port))
        .await
        .map_err(TunnelError::Bind)?;
    let local_addr = listener.local_addr().map_err(TunnelError::Bind)?;

    let (shutdown, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(accept_loop(
        handle,
        listener,
        shutdown_rx,
        config.target_host.clone(),
        config.target_port,
    ));
    Ok(Tunnel {
        local_addr,
        shutdown: Some(shutdown),
        task: Some(task),
    })
}

async fn authenticate(
    handle: &mut SshHandle,
    user: &str,
    auth: &SshAuth,
) -> Result<(), TunnelError> {
    match auth {
        SshAuth::Password(pw) => {
            let result = handle.authenticate_password(user, pw.expose()).await?;
            require(result.success())
        }
        SshAuth::KeyFile { path, passphrase } => {
            let result = match load_key(path, passphrase.as_ref())? {
                LoadedKey::Native(key) => {
                    handle
                        .authenticate_publickey(
                            user,
                            PrivateKeyWithHashAlg::new(Arc::new(key), None),
                        )
                        .await?
                }
                LoadedKey::Rsa(mut signer) => {
                    let hash = match handle.best_supported_rsa_hash().await? {
                        Some(Some(hash)) => hash,
                        Some(None) => return Err(rsa_key::sha1_only()),
                        // The server sent no `server-sig-algs`: try the SHA-2 signature anyway.
                        None => HashAlg::Sha256,
                    };
                    let public = signer.public_key().clone();
                    handle
                        .authenticate_publickey_with(user, public, Some(hash), &mut signer)
                        .await?
                }
            };
            require(result.success())
        }
        SshAuth::Agent { socket } => authenticate_agent(handle, user, socket.as_deref()).await,
        #[allow(unreachable_patterns)]
        _ => Err(TunnelError::Ssh("unsupported authentication method".into())),
    }
}

fn require(ok: bool) -> Result<(), TunnelError> {
    if ok {
        Ok(())
    } else {
        Err(TunnelError::AuthFailed)
    }
}

/// A private key ready to authenticate with.
enum LoadedKey {
    /// A key `russh` signs with itself (Ed25519, ECDSA).
    Native(russh::keys::PrivateKey),
    /// An RSA key, signed by `ring` through [`RsaSigner`].
    Rsa(RsaSigner),
}

fn load_key(path: &Path, passphrase: Option<&Secret>) -> Result<LoadedKey, TunnelError> {
    if let Some(signer) = RsaSigner::load_if_rsa(path, passphrase)? {
        return Ok(LoadedKey::Rsa(signer));
    }
    let key = load_secret_key(path, passphrase.map(Secret::expose)).map_err(|e| {
        match (e, passphrase) {
            (KeyError::KeyIsEncrypted, None) => TunnelError::KeyPassphraseRequired {
                path: path.to_path_buf(),
            },
            (KeyError::KeyIsEncrypted, Some(_)) => TunnelError::KeyFile {
                path: path.to_path_buf(),
                message: "the passphrase does not decrypt the key".into(),
            },
            (e, _) => TunnelError::KeyFile {
                path: path.to_path_buf(),
                message: e.to_string(),
            },
        }
    })?;
    // RSA keys in a format `RsaSigner` does not read cannot be signed with here.
    if key.algorithm().is_rsa() {
        return Err(TunnelError::KeyFile {
            path: path.to_path_buf(),
            message: "this RSA key format is not supported; convert it with `ssh-keygen -p -o -f \
                      <file>` (OpenSSH format), or use an Ed25519 or ECDSA key"
                .into(),
        });
    }
    Ok(LoadedKey::Native(key))
}

#[cfg(unix)]
async fn authenticate_agent(
    handle: &mut SshHandle,
    user: &str,
    socket: Option<&Path>,
) -> Result<(), TunnelError> {
    let client = match socket {
        Some(path) => AgentClient::connect_uds(path).await,
        None => AgentClient::connect_env().await,
    }
    .map_err(|e| TunnelError::AgentUnavailable(e.to_string()))?;
    agent_identities(handle, user, client).await
}

#[cfg(windows)]
async fn authenticate_agent(
    handle: &mut SshHandle,
    user: &str,
    socket: Option<&Path>,
) -> Result<(), TunnelError> {
    let pipe = socket.map_or_else(
        || std::ffi::OsString::from(WINDOWS_AGENT_PIPE),
        |p| p.as_os_str().to_owned(),
    );
    let client = AgentClient::connect_named_pipe(pipe)
        .await
        .map_err(|e| TunnelError::AgentUnavailable(e.to_string()))?;
    agent_identities(handle, user, client).await
}

#[cfg(not(any(unix, windows)))]
async fn authenticate_agent(
    _handle: &mut SshHandle,
    _user: &str,
    _socket: Option<&Path>,
) -> Result<(), TunnelError> {
    Err(TunnelError::AgentUnavailable(
        "the SSH agent is not supported on this platform".into(),
    ))
}

/// Tries each plain public key the agent holds; the first one the server accepts wins.
#[cfg(any(unix, windows))]
async fn agent_identities<S>(
    handle: &mut SshHandle,
    user: &str,
    mut client: AgentClient<S>,
) -> Result<(), TunnelError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let identities = client
        .request_identities()
        .await
        .map_err(|e| TunnelError::AgentUnavailable(e.to_string()))?;
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let hash = if key.algorithm().is_rsa() {
            handle.best_supported_rsa_hash().await?.flatten()
        } else {
            None
        };
        let result = handle
            .authenticate_publickey_with(user, key, hash, &mut client)
            .await
            .map_err(|e| TunnelError::Ssh(e.to_string()))?;
        if result.success() {
            return Ok(());
        }
    }
    Err(TunnelError::AuthFailed)
}

/// Accepts local connections until `shutdown` fires, then drops the forwards and disconnects.
async fn accept_loop(
    handle: SshHandle,
    listener: TcpListener,
    mut shutdown: oneshot::Receiver<()>,
    target_host: String,
    target_port: u16,
) -> Result<(), TunnelError> {
    let handle = Arc::new(handle);
    let mut forwards = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        forwards.spawn(forward(
                            Arc::clone(&handle),
                            stream,
                            peer,
                            target_host.clone(),
                            target_port,
                        ));
                    }
                    // Persistent errors (EMFILE) must not spin; shutdown still wins the select.
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
            Some(_) = forwards.join_next(), if !forwards.is_empty() => {}
        }
    }
    drop(listener);
    forwards.shutdown().await;
    handle
        .disconnect(Disconnect::ByApplication, "", "en")
        .await
        .map_err(TunnelError::from)
}

/// Pipes one local connection through a `direct-tcpip` channel. A failed channel open only
/// closes that local connection.
async fn forward(
    handle: Arc<SshHandle>,
    mut stream: TcpStream,
    peer: SocketAddr,
    host: String,
    port: u16,
) {
    let Ok(channel) = handle
        .channel_open_direct_tcpip(
            host,
            u32::from(port),
            Ipv4Addr::LOCALHOST.to_string(),
            u32::from(peer.port()),
        )
        .await
    else {
        return;
    };
    let mut channel = channel.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut channel).await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// A server that sends its banner and then stalls: russh spawns its session task after the
    /// banner, so a cancelled open leaves that task behind until the inactivity timeout ends it.
    #[tokio::test]
    async fn stalled_after_banner_is_closed_by_inactivity_timeout() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let eof = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&eof);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"SSH-2.0-x\r\n").await.unwrap();
            let mut buf = [0u8; 1024];
            while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
            flag.store(true, Ordering::SeqCst);
        });

        let dir = std::env::temp_dir().join(format!("sqler-tunnel-{}-stall", std::process::id()));
        let config = TunnelConfig::new("127.0.0.1", "u", "127.0.0.1", 9, dir.join("known_hosts"))
            .with_ssh_port(port);
        let auth = SshAuth::Password(Secret::new("x"));
        let res = tokio::time::timeout(
            Duration::from_millis(300),
            open_inner(&config, &auth, Duration::from_millis(500)),
        )
        .await;
        assert!(res.is_err(), "open should still be pending");

        let closed = tokio::time::timeout(Duration::from_secs(5), server).await;
        assert!(closed.is_ok(), "server task did not finish");
        assert!(eof.load(Ordering::SeqCst), "socket was not closed");
    }
}
