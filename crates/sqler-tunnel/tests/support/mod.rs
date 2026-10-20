// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! In-process SSH server and TCP targets for the tunnel tests. Loopback only.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Server as _};
use russh::{Channel, ChannelOpenFailure};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

pub const PASSWORD: &str = "test-pw";
pub const PASSPHRASE: &str = "key-pass-phrase";
pub const USER: &str = "tester";

/// Live session and channel counts of the test server.
#[derive(Default)]
pub struct Counters {
    pub sessions: AtomicUsize,
    pub channels: AtomicUsize,
}

impl Counters {
    pub fn sessions(&self) -> usize {
        self.sessions.load(Ordering::SeqCst)
    }

    pub fn channels(&self) -> usize {
        self.channels.load(Ordering::SeqCst)
    }
}

struct CountGuard(Arc<Counters>, fn(&Counters) -> &AtomicUsize);

impl CountGuard {
    fn new(c: &Arc<Counters>, field: fn(&Counters) -> &AtomicUsize) -> Self {
        field(c).fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(c), field)
    }
}

impl Drop for CountGuard {
    fn drop(&mut self) {
        (self.1)(&self.0).fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub host_key: PrivateKey,
    pub counters: Arc<Counters>,
    task: JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct ServerState {
    counters: Arc<Counters>,
    client_key: Option<PublicKey>,
}

struct SessionHandler {
    state: ServerState,
    _guard: CountGuard,
}

impl server::Server for ServerState {
    type Handler = SessionHandler;

    fn new_client(&mut self, _peer: Option<SocketAddr>) -> SessionHandler {
        SessionHandler {
            _guard: CountGuard::new(&self.counters, |c| &c.sessions),
            state: self.clone(),
        }
    }
}

impl server::Handler for SessionHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == USER && password == PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        if user == USER && self.state.client_key.as_ref() == Some(key) {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        self.auth_publickey(user, key).await
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        let Ok(port) = u16::try_from(port_to_connect) else {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        let Ok(mut target) = TcpStream::connect((host_to_connect, port)).await else {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        reply.accept().await;
        let guard = CountGuard::new(&self.state.counters, |c| &c.channels);
        tokio::spawn(async move {
            let _guard = guard;
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut target).await;
        });
        Ok(())
    }
}

/// Starts a server on `127.0.0.1:0` that accepts [`PASSWORD`] and, if given, `client_key`.
pub async fn start_server(client_key: Option<PublicKey>) -> TestServer {
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![host_key.clone()],
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..server::Config::default()
    });
    let counters = Arc::new(Counters::default());
    let mut state = ServerState {
        counters: Arc::clone(&counters),
        client_key,
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, peer)) = listener.accept().await else {
                break;
            };
            let handler = state.new_client(Some(peer));
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(config, stream, handler).await {
                    let _ = session.await;
                }
            });
        }
    });
    TestServer {
        addr,
        host_key,
        counters,
        task,
    }
}

/// A TCP echo target. Returns its address and the task to keep alive.
pub async fn start_echo() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = stream.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    (addr, task)
}

/// A listener that accepts and never sends a banner. `eof` turns true once the peer closed.
pub struct SilentListener {
    pub addr: SocketAddr,
    pub eof: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for SilentListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start_silent() -> SilentListener {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let eof = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&eof);
    let task = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            // The client sends its identification string first; keep reading until EOF.
            while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
            flag.store(true, Ordering::SeqCst);
            let _ = stream.shutdown().await;
        }
    });
    SilentListener { addr, eof, task }
}

/// A fresh temp directory under the system temp dir.
pub fn temp_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sqler-tunnel-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn new_client_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

/// Writes `key` in `OpenSSH` format; encrypted with `passphrase` when given.
pub fn write_key(dir: &Path, name: &str, key: &PrivateKey, passphrase: Option<&str>) -> PathBuf {
    let path = dir.join(name);
    let text = match passphrase {
        Some(p) => key
            .encrypt(&mut rand::rng(), p)
            .unwrap()
            .to_openssh(LineEnding::LF)
            .unwrap(),
        None => key.to_openssh(LineEnding::LF).unwrap(),
    };
    std::fs::write(&path, text.as_bytes()).unwrap();
    path
}

/// Polls `cond` for up to `secs` seconds.
pub async fn eventually(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}
