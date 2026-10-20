// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end tests against an in-process SSH server. Loopback only; no Docker.

#![allow(clippy::unwrap_used, clippy::unused_async_trait_impl)]

mod support;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use sqler_tunnel::{
    HostKeyInfo, Secret, SshAuth, Tunnel, TunnelConfig, TunnelError, trust_host_key,
};
use support::{
    PASSPHRASE, PASSWORD, USER, eventually, new_client_key, start_echo, start_server, start_silent,
    temp_dir, write_key,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn host_info(server: &support::TestServer) -> HostKeyInfo {
    HostKeyInfo {
        host: "127.0.0.1".into(),
        port: server.addr.port(),
        algorithm: "ssh-ed25519".into(),
        fingerprint: String::new(),
        public_key: server.host_key.public_key().to_openssh().unwrap(),
    }
}

fn config(server: &support::TestServer, target: SocketAddr, dir: &std::path::Path) -> TunnelConfig {
    TunnelConfig::new(
        "127.0.0.1",
        USER,
        "127.0.0.1",
        target.port(),
        dir.join("known_hosts"),
    )
    .with_ssh_port(server.addr.port())
    .with_connect_timeout(Duration::from_secs(10))
}

/// Server, echo target, temp dir and a config whose `known_hosts` already trusts the server.
async fn trusted(test: &str) -> (support::TestServer, SocketAddr, PathBuf, TunnelConfig) {
    let dir = temp_dir(test);
    let client = new_client_key();
    let server = start_server(Some(client.public_key().clone())).await;
    let (echo, task) = start_echo().await;
    std::mem::forget(task); // runs until the test runtime shuts down
    let cfg = config(&server, echo, &dir);
    trust_host_key(&cfg.known_hosts, &host_info(&server)).unwrap();
    (server, echo, dir, cfg)
}

fn password() -> SshAuth {
    SshAuth::Password(Secret::new(PASSWORD))
}

async fn echo_roundtrip(addr: SocketAddr, payload: &[u8]) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(payload).await.unwrap();
    let mut buf = vec![0; payload.len()];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(buf, payload);
}

fn assert_no_secrets(err: &TunnelError) {
    for text in [err.to_string(), format!("{err:?}")] {
        assert!(!text.contains(PASSWORD), "{text}");
        assert!(!text.contains(PASSPHRASE), "{text}");
    }
}

#[tokio::test]
async fn forwards_two_concurrent_connections() {
    let (server, _echo, _dir, cfg) = trusted("echo").await;
    let tunnel = Tunnel::open(&cfg, &password()).await.unwrap();
    assert!(tunnel.local_addr().ip().is_loopback());
    assert!(format!("{tunnel:?}").contains(&tunnel.local_addr().port().to_string()));

    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let a = echo_roundtrip(tunnel.local_addr(), b"hello through the tunnel");
    let b = echo_roundtrip(tunnel.local_addr(), &big);
    tokio::join!(a, b);

    assert!(eventually(5, || server.counters.channels() == 0).await);
    tunnel.close().await.unwrap();
}

#[tokio::test]
async fn auth_password_ok_and_wrong() {
    let (_server, _echo, _dir, cfg) = trusted("pw").await;
    Tunnel::open(&cfg, &password()).await.unwrap();

    let err = Tunnel::open(&cfg, &SshAuth::Password(Secret::new("wrong-pw")))
        .await
        .unwrap_err();
    assert!(matches!(err, TunnelError::AuthFailed), "{err:?}");
    assert_no_secrets(&err);
}

#[tokio::test]
async fn auth_key_files() {
    let dir = temp_dir("keys");
    let key = new_client_key();
    let server = start_server(Some(key.public_key().clone())).await;
    let (echo, task) = start_echo().await;
    std::mem::forget(task);
    let cfg = config(&server, echo, &dir);
    trust_host_key(&cfg.known_hosts, &host_info(&server)).unwrap();

    let plain = write_key(&dir, "id_plain", &key, None);
    let enc = write_key(&dir, "id_enc", &key, Some(PASSPHRASE));
    let other = write_key(&dir, "id_other", &new_client_key(), None);

    let t = Tunnel::open(
        &cfg,
        &SshAuth::KeyFile {
            path: plain,
            passphrase: None,
        },
    )
    .await
    .unwrap();
    echo_roundtrip(t.local_addr(), b"plain").await;
    t.close().await.unwrap();

    let t = Tunnel::open(
        &cfg,
        &SshAuth::KeyFile {
            path: enc.clone(),
            passphrase: Some(Secret::new(PASSPHRASE)),
        },
    )
    .await
    .unwrap();
    echo_roundtrip(t.local_addr(), b"encrypted").await;
    t.close().await.unwrap();

    let err = Tunnel::open(
        &cfg,
        &SshAuth::KeyFile {
            path: enc.clone(),
            passphrase: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, TunnelError::KeyPassphraseRequired { path } if *path == enc),
        "{err:?}"
    );

    let err = Tunnel::open(
        &cfg,
        &SshAuth::KeyFile {
            path: enc,
            passphrase: Some(Secret::new("not-the-passphrase")),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, TunnelError::KeyFile { .. }), "{err:?}");
    assert_no_secrets(&err);

    let err = Tunnel::open(
        &cfg,
        &SshAuth::KeyFile {
            path: other,
            passphrase: None,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, TunnelError::AuthFailed), "{err:?}");

    let err = Tunnel::open(
        &cfg,
        &SshAuth::KeyFile {
            path: dir.join("missing"),
            passphrase: None,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, TunnelError::KeyFile { .. }), "{err:?}");
}

#[tokio::test]
async fn rsa_key_files_are_unsupported() {
    // RSA is compiled out (RUSTSEC-2023-0071); the fixtures are throwaway test keys.
    let (_server, _echo, _dir, cfg) = trusted("rsa-keys").await;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for name in ["rsa_openssh_unencrypted", "rsa_pkcs1_unencrypted"] {
        let path = fixtures.join(name);
        let err = Tunnel::open(
            &cfg,
            &SshAuth::KeyFile {
                path: path.clone(),
                passphrase: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, TunnelError::UnsupportedKeyType { path: p } if *p == path),
            "{name}: {err:?}"
        );
        assert!(err.to_string().contains("ed25519 or ECDSA"), "{err}");
    }
}

#[tokio::test]
async fn agent_missing_socket_is_unavailable() {
    let (_server, _echo, dir, cfg) = trusted("agent-missing").await;
    #[cfg(unix)]
    let socket = dir.join("no-such-agent.sock");
    #[cfg(windows)]
    let socket = {
        let _ = &dir; // only the Unix socket lives in the temp dir
        PathBuf::from(format!(
            r"\\.\pipe\sqler-no-such-agent-{}",
            std::process::id()
        ))
    };
    let err = Tunnel::open(
        &cfg,
        &SshAuth::Agent {
            socket: Some(socket),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, TunnelError::AgentUnavailable(_)), "{err:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn agent_with_key_and_without() {
    use russh::keys::agent::client::AgentClient;
    use russh::keys::agent::server::serve;
    use tokio::net::UnixListener;
    use tokio_stream::wrappers::UnixListenerStream;

    let dir = temp_dir("agent");
    let key = new_client_key();
    let server = start_server(Some(key.public_key().clone())).await;
    let (echo, task) = start_echo().await;
    std::mem::forget(task);
    let cfg2 = config(&server, echo, &dir);
    trust_host_key(&cfg2.known_hosts, &host_info(&server)).unwrap();

    let socket = dir.join("agent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let agent = tokio::spawn(serve(UnixListenerStream::new(listener), ()));
    let auth = SshAuth::Agent {
        socket: Some(socket.clone()),
    };

    // Empty agent: no identity can be accepted.
    let err = Tunnel::open(&cfg2, &auth).await.unwrap_err();
    assert!(matches!(err, TunnelError::AuthFailed), "{err:?}");

    let mut client = AgentClient::connect_uds(&socket).await.unwrap();
    client.add_identity(&key, &[]).await.unwrap();

    let t = Tunnel::open(&cfg2, &auth).await.unwrap();
    echo_roundtrip(t.local_addr(), b"agent").await;
    t.close().await.unwrap();
    agent.abort();
}

#[tokio::test]
async fn host_key_unknown_trust_and_changed() {
    let dir = temp_dir("hostkey");
    let server = start_server(None).await;
    let (echo, task) = start_echo().await;
    std::mem::forget(task);
    let cfg = config(&server, echo, &dir);

    // Unknown (no known_hosts file at all).
    let err = Tunnel::open(&cfg, &password()).await.unwrap_err();
    let TunnelError::UnknownHostKey(info) = err else {
        panic!("expected UnknownHostKey, got {err:?}");
    };
    assert!(info.fingerprint.starts_with("SHA256:"), "{info:?}");
    assert_eq!(info.port, server.addr.port());
    assert_eq!(
        info.fingerprint,
        server
            .host_key
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string()
    );
    assert_eq!(server.counters.channels(), 0);

    // Trust it, then it opens.
    trust_host_key(&cfg.known_hosts, &info).unwrap();
    let t = Tunnel::open(&cfg, &password()).await.unwrap();
    echo_roundtrip(t.local_addr(), b"trusted").await;
    t.close().await.unwrap();

    // A different key recorded for the same host:port is a changed key.
    let dir2 = temp_dir("hostkey-changed");
    let mut other = host_info(&server);
    other.public_key = new_client_key().public_key().to_openssh().unwrap();
    let cfg2 = config(&server, echo, &dir2);
    trust_host_key(&cfg2.known_hosts, &other).unwrap();
    let err = Tunnel::open(&cfg2, &password()).await.unwrap_err();
    let TunnelError::HostKeyChanged { key, line } = err else {
        panic!("expected HostKeyChanged, got {err:?}");
    };
    assert_eq!(line, 1);
    assert_eq!(key.public_key, info.public_key);
}

#[tokio::test]
async fn close_tears_down() {
    let (server, _echo, _dir, cfg) = trusted("close").await;
    let tunnel = Tunnel::open(&cfg, &password()).await.unwrap();
    let addr = tunnel.local_addr();
    echo_roundtrip(addr, b"x").await;
    assert_eq!(server.counters.sessions(), 1);

    tunnel.close().await.unwrap();
    assert!(TcpStream::connect(addr).await.is_err());
    assert!(eventually(5, || server.counters.sessions() == 0).await);
}

#[tokio::test]
async fn drop_tears_down() {
    let (server, _echo, _dir, cfg) = trusted("drop").await;
    let tunnel = Tunnel::open(&cfg, &password()).await.unwrap();
    let addr = tunnel.local_addr();
    // A live forward must be torn down as well.
    let mut live = TcpStream::connect(addr).await.unwrap();
    live.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    live.read_exact(&mut buf).await.unwrap();
    assert_eq!(server.counters.sessions(), 1);

    drop(tunnel);
    assert!(eventually(5, || server.counters.sessions() == 0).await);
    assert!(eventually(5, || server.counters.channels() == 0).await);
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), live.read_to_end(&mut rest)).await;
    assert!(read.is_ok(), "live forward was not closed");
    assert!(eventually(5, || std::net::TcpStream::connect(addr).is_err()).await);
}

#[tokio::test]
async fn unreachable_target_closes_only_that_connection() {
    let (_server, echo, dir, mut cfg) = trusted("unreachable").await;
    // A port nobody listens on: bind and drop.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    cfg.target_port = dead;
    let tunnel = Tunnel::open(&cfg, &password()).await.unwrap();
    let mut s = TcpStream::connect(tunnel.local_addr()).await.unwrap();
    let mut buf = Vec::new();
    let r = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf))
        .await
        .unwrap();
    assert!(r.is_err() || buf.is_empty());
    // The tunnel itself still accepts connections.
    assert!(TcpStream::connect(tunnel.local_addr()).await.is_ok());
    tunnel.close().await.unwrap();
    let _ = (echo, dir);
}

#[tokio::test]
async fn cancel_open_closes_the_socket() {
    let silent = start_silent().await;
    let dir = temp_dir("cancel");
    let cfg = TunnelConfig::new("127.0.0.1", USER, "127.0.0.1", 9, dir.join("known_hosts"))
        .with_ssh_port(silent.addr.port());

    let started = Instant::now();
    let res =
        tokio::time::timeout(Duration::from_millis(200), Tunnel::open(&cfg, &password())).await;
    assert!(res.is_err(), "open should still be pending");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(eventually(2, || silent.eof.load(Ordering::SeqCst)).await);
}

#[tokio::test]
async fn connect_timeout_is_reported() {
    let silent = start_silent().await;
    let dir = temp_dir("timeout");
    let cfg = TunnelConfig::new("127.0.0.1", USER, "127.0.0.1", 9, dir.join("known_hosts"))
        .with_ssh_port(silent.addr.port())
        .with_connect_timeout(Duration::from_millis(150));
    let err = Tunnel::open(&cfg, &password()).await.unwrap_err();
    assert!(matches!(err, TunnelError::Timeout), "{err:?}");
    assert!(eventually(2, || silent.eof.load(Ordering::SeqCst)).await);
}

#[tokio::test]
async fn connection_refused_and_invalid_config() {
    let dir = temp_dir("refused");
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let cfg = TunnelConfig::new("127.0.0.1", USER, "127.0.0.1", 9, dir.join("known_hosts"))
        .with_ssh_port(dead);
    let err = Tunnel::open(&cfg, &password()).await.unwrap_err();
    assert!(matches!(err, TunnelError::Connect { .. }), "{err:?}");

    let bad = TunnelConfig::new("", USER, "127.0.0.1", 9, dir.join("known_hosts"));
    let err = Tunnel::open(&bad, &password()).await.unwrap_err();
    assert!(matches!(err, TunnelError::InvalidConfig(_)), "{err:?}");
}

#[tokio::test]
async fn local_port_is_honoured_and_busy_port_is_bind_error() {
    let (_server, _echo, _dir, cfg) = trusted("bind").await;
    let first = Tunnel::open(&cfg, &password()).await.unwrap();
    let port = first.local_addr().port();
    let err = Tunnel::open(&cfg.clone().with_local_port(port), &password())
        .await
        .unwrap_err();
    assert!(matches!(err, TunnelError::Bind(_)), "{err:?}");
    first.close().await.unwrap();
}
