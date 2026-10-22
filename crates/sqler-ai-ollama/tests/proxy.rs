// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A loopback server must be reached directly even when the environment names
//! a proxy. The proxy variables are set on a child process (this crate forbids
//! `unsafe`, so the test cannot change its own environment), which keeps the
//! other test binaries free of environment races.

#![allow(clippy::unwrap_used)]

mod support;

use std::process::Command;

use sqler_ai_api::AiProvider;
use sqler_ai_ollama::OllamaProvider;
use support::{MockServer, Reply, Route, dead_port_url};

const CHILD: &str = "SQLER_PROXY_TEST_CHILD";

#[tokio::test]
async fn child_lists_models_with_a_dead_proxy_in_the_environment() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let server = MockServer::start(vec![
        Route::new(
            "GET",
            "/api/tags",
            Reply::json(200, r#"{"models":[{"name":"a"}]}"#),
        ),
        Route::new("POST", "/api/show", Reply::json(200, "{}")),
    ])
    .await;
    let models = OllamaProvider::new(Some(&server.url()))
        .unwrap()
        .list_models()
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(server.requests().len(), 2, "the proxy was used");
}

#[tokio::test]
async fn loopback_bypasses_the_environment_proxy() {
    let dead = dead_port_url().await;
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args([
            "--exact",
            "child_lists_models_with_a_dead_proxy_in_the_environment",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("HTTP_PROXY", &dead)
        .env("http_proxy", &dead)
        .env("ALL_PROXY", &dead)
        .env("all_proxy", &dead)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
