// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maintainer-only check against a real bastion. Run:
//! `cargo test -p sqler-tunnel --test real_bastion -- --ignored --nocapture`
//!
//! Reads `SQLER_TEST_BASTION_{HOST,PORT,USER,KEY,TARGET_HOST,TARGET_PORT}`. The bastion must be
//! in the real `~/.ssh/known_hosts`. `KEY` is the path of an unencrypted private key file.

#![allow(clippy::unwrap_used)]

use sqler_tunnel::{SshAuth, Tunnel, TunnelConfig, default_known_hosts_path};
use tokio::net::TcpStream;

fn var(name: &str) -> Option<String> {
    std::env::var(format!("SQLER_TEST_BASTION_{name}")).ok()
}

#[tokio::test]
#[ignore = "needs a real SSH bastion; see the module docs"]
async fn tunnel_through_real_bastion() {
    let (Some(host), Some(port), Some(user), Some(key), Some(target_host), Some(target_port)) = (
        var("HOST"),
        var("PORT"),
        var("USER"),
        var("KEY"),
        var("TARGET_HOST"),
        var("TARGET_PORT"),
    ) else {
        println!("SQLER_TEST_BASTION_* not all set; skipping");
        return;
    };
    let Some(known_hosts) = default_known_hosts_path() else {
        println!("no home directory; skipping");
        return;
    };
    let config = TunnelConfig::new(
        host,
        user,
        target_host,
        target_port.parse().unwrap(),
        known_hosts,
    )
    .with_ssh_port(port.parse().unwrap());
    let auth = SshAuth::KeyFile {
        path: key.into(),
        passphrase: None,
    };
    let tunnel = Tunnel::open(&config, &auth).await.unwrap();
    println!("tunnel at {}", tunnel.local_addr());
    let stream = TcpStream::connect(tunnel.local_addr()).await.unwrap();
    drop(stream);
    tunnel.close().await.unwrap();
}
