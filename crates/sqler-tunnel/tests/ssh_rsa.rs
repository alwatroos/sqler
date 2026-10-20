// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! RSA user keys against a real OpenSSH server (ADR-0018). The server comes from
//! `tests/docker/start-sshd.sh`; see AGENTS.md section 3. The tests skip unless
//! `SQLER_TEST_SSH_HOST` is set, and fail instead if `ssh` is listed in `SQLER_TEST_REQUIRE_DB`.
//!
//! Port `PORT_SHA256` accepts only `rsa-sha2-256` signatures, port `PORT_SHA512` only
//! `rsa-sha2-512`. Each test opens a tunnel to the container's own sshd (127.0.0.1:22) and reads
//! its banner.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sqler_tunnel::{Secret, SshAuth, Tunnel, TunnelConfig, TunnelError};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

struct Env {
    host: String,
    user: String,
    known_hosts: PathBuf,
    ports: [u16; 2],
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn required(name: &str) -> String {
    var(name).unwrap_or_else(|| panic!("{name} is not set"))
}

/// The test server, or `None` (skip) when `SQLER_TEST_SSH_HOST` is not set.
fn env() -> Option<Env> {
    let Some(host) = var("SQLER_TEST_SSH_HOST") else {
        let gated = std::env::var("SQLER_TEST_REQUIRE_DB")
            .is_ok_and(|list| list.split(',').any(|id| id.trim() == "ssh"));
        assert!(
            !gated,
            "SQLER_TEST_SSH_HOST is not set, but ssh is listed in SQLER_TEST_REQUIRE_DB"
        );
        println!("skipped: SQLER_TEST_SSH_HOST is not set (see AGENTS.md section 3)");
        return None;
    };
    let port = |name| required(name).parse::<u16>().unwrap();
    Some(Env {
        host,
        user: required("SQLER_TEST_SSH_USER"),
        known_hosts: PathBuf::from(required("SQLER_TEST_SSH_KNOWN_HOSTS")),
        ports: [
            port("SQLER_TEST_SSH_PORT_SHA256"),
            port("SQLER_TEST_SSH_PORT_SHA512"),
        ],
    })
}

impl Env {
    fn config(&self, port: u16) -> TunnelConfig {
        TunnelConfig::new(
            &self.host,
            &self.user,
            "127.0.0.1",
            22,
            self.known_hosts.clone(),
        )
        .with_ssh_port(port)
    }

    /// Opens a tunnel and checks that it carries the banner of the container's sshd.
    async fn banner(&self, port: u16, auth: &SshAuth) {
        let tunnel = Tunnel::open(&self.config(port), auth).await.unwrap();
        let mut stream = TcpStream::connect(tunnel.local_addr()).await.unwrap();
        let mut buf = [0u8; 8];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"SSH-2.0-");
        drop(stream);
        tunnel.close().await.unwrap();
    }

    async fn open_err(&self, port: u16, auth: &SshAuth) -> TunnelError {
        Tunnel::open(&self.config(port), auth).await.unwrap_err()
    }
}

fn key_file(var_name: &str, passphrase: Option<&str>) -> SshAuth {
    SshAuth::KeyFile {
        path: PathBuf::from(required(var_name)),
        passphrase: passphrase.map(Secret::new),
    }
}

#[tokio::test]
async fn rsa_keys_authenticate_on_both_hashes() {
    let Some(env) = env() else { return };
    let passphrase = required("SQLER_TEST_SSH_KEY_PASSPHRASE");
    for port in env.ports {
        env.banner(port, &key_file("SQLER_TEST_SSH_KEY_RSA", None))
            .await;
        env.banner(port, &key_file("SQLER_TEST_SSH_KEY_RSA_PEM", None))
            .await;
        env.banner(port, &key_file("SQLER_TEST_SSH_KEY_RSA_PKCS8", None))
            .await;
        env.banner(
            port,
            &key_file("SQLER_TEST_SSH_KEY_RSA_ENC", Some(&passphrase)),
        )
        .await;
    }
}

#[tokio::test]
async fn encrypted_key_passphrase_errors() {
    let Some(env) = env() else { return };
    let port = env.ports[0];
    let err = env
        .open_err(port, &key_file("SQLER_TEST_SSH_KEY_RSA_ENC", None))
        .await;
    assert!(
        matches!(err, TunnelError::KeyPassphraseRequired { .. }),
        "{err:?}"
    );
    let err = env
        .open_err(port, &key_file("SQLER_TEST_SSH_KEY_RSA_ENC", Some("wrong")))
        .await;
    assert!(
        matches!(&err, TunnelError::KeyFile { message, .. } if message.contains("passphrase does not decrypt")),
        "{err:?}"
    );
}

#[tokio::test]
async fn unknown_rsa_key_is_rejected_by_the_server() {
    let Some(env) = env() else { return };
    let dir = std::env::temp_dir().join(format!("sqler-ssh-rsa-{}-other", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = generate(&dir, "other", 2048, "RFC4716");
    let err = env
        .open_err(
            env.ports[0],
            &SshAuth::KeyFile {
                path: key,
                passphrase: None,
            },
        )
        .await;
    assert!(matches!(err, TunnelError::AuthFailed), "{err:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn unsupported_rsa_keys_are_refused_before_authentication() {
    let Some(env) = env() else { return };
    let dir = std::env::temp_dir().join(format!("sqler-ssh-rsa-{}-refused", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for bits in [1024, 2560] {
        for format in ["RFC4716", "PEM"] {
            let key = generate(&dir, &format!("k{bits}-{format}"), bits, format);
            let err = env
                .open_err(
                    env.ports[0],
                    &SshAuth::KeyFile {
                        path: key,
                        passphrase: None,
                    },
                )
                .await;
            let TunnelError::KeyFile { message, .. } = &err else {
                panic!("{bits} {format}: {err:?}");
            };
            assert!(message.contains("2048, 3072 or 4096"), "{message}");
            assert!(message.contains("Ed25519 or ECDSA"), "{message}");
        }
    }
    let legacy = dir.join("legacy");
    run_keygen(&[
        "-q",
        "-t",
        "rsa",
        "-b",
        "2048",
        "-m",
        "PEM",
        "-N",
        "legacy-pass",
        "-f",
        legacy.to_str().unwrap(),
    ]);
    let err = env
        .open_err(
            env.ports[0],
            &SshAuth::KeyFile {
                path: legacy,
                passphrase: Some(Secret::new("legacy-pass")),
            },
        )
        .await;
    assert!(
        matches!(&err, TunnelError::KeyFile { message, .. } if message.contains("legacy-encrypted")),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn run_keygen(args: &[&str]) {
    let out = Command::new("ssh-keygen").args(args).output().unwrap();
    assert!(out.status.success(), "ssh-keygen {args:?}: {out:?}");
}

/// An unencrypted RSA key of `bits` in `format` (`RFC4716` or `PEM`), generated into `dir`.
fn generate(dir: &Path, name: &str, bits: u32, format: &str) -> PathBuf {
    let path = dir.join(name);
    run_keygen(&[
        "-q",
        "-t",
        "rsa",
        "-b",
        &bits.to_string(),
        "-m",
        format,
        "-N",
        "",
        "-f",
        path.to_str().unwrap(),
    ]);
    path
}
