// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated checks of `TlsConfig::server_name` through a local forwarder that stands in for an
//! SSH tunnel (needs `SQLER_TEST_POSTGRES_CA`, see AGENTS.md §3).

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use sqler_driver_api::sqler_core::{ConnectionConfig, Query, TlsConfig, TlsMode, Value};
use sqler_driver_api::{DriverError, Session};

use common::{config, db_env, forwarder, run, sql, try_connect};

/// `(url via the forwarder on 127.0.0.1, CA path)`; the forwarder lives on the caller's runtime.
async fn env() -> Option<(String, String)> {
    let url = common::url()?;
    let ca = db_env("SQLER_TEST_POSTGRES_CA", "postgres")?;
    let (head, tail) = url.split_once("@127.0.0.1:").unwrap();
    let digits = tail.chars().take_while(char::is_ascii_digit).count();
    let port: u16 = tail[..digits].parse().unwrap();
    let local = forwarder(port).await;
    Some((format!("{head}@127.0.0.1:{local}{}", &tail[digits..]), ca))
}

fn tls(mode: TlsMode, ca: &str, name: Option<&str>) -> TlsConfig {
    let tls = TlsConfig::new(mode).with_ca_cert(ca);
    match name {
        Some(name) => tls.with_server_name(name),
        None => tls,
    }
}

async fn ssl_in_use(config: &ConnectionConfig) -> Value {
    let mut s = try_connect(config).await.unwrap();
    let rs = sql(
        &mut *s,
        "select ssl from pg_stat_ssl where pid = pg_backend_pid()",
    )
    .await;
    rs.rows()[0].0[0].clone()
}

#[test]
fn verify_full_with_the_certificate_name() {
    run(async {
        let Some((url, ca)) = env().await else { return };
        let cfg = config(&url).with_tls(tls(TlsMode::VerifyFull, &ca, Some("localhost")));
        assert_eq!(ssl_in_use(&cfg).await, Value::Bool(true));
    });
}

#[test]
fn a_wrong_server_name_fails_without_leaking_the_password() {
    run(async {
        let Some((url, ca)) = env().await else { return };
        let cfg = config(&url).with_tls(tls(TlsMode::VerifyFull, &ca, Some("wrong.example")));
        let err = try_connect(&cfg).await.err().unwrap();
        assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
        assert!(!err.to_string().contains("postgres:sqler"));
    });
}

#[test]
fn verify_full_without_a_server_name_still_fails_on_the_ip() {
    run(async {
        let Some((url, ca)) = env().await else { return };
        let cfg = config(&url).with_tls(tls(TlsMode::VerifyFull, &ca, None));
        let err = try_connect(&cfg).await.err().unwrap();
        assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
    });
}

#[test]
fn verify_ca_ignores_the_server_name() {
    run(async {
        let Some((url, ca)) = env().await else { return };
        let cfg = config(&url).with_tls(tls(TlsMode::VerifyCa, &ca, Some("wrong.example")));
        assert_eq!(ssl_in_use(&cfg).await, Value::Bool(true));
    });
}

async fn within<T>(secs: u64, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), fut)
        .await
        .unwrap_or_else(|_| panic!("timed out after {secs} s"))
}

#[test]
fn cancel_a_query_through_the_bridge() {
    run(async {
        let Some((url, ca)) = env().await else { return };
        let cfg = config(&url).with_tls(tls(TlsMode::VerifyFull, &ca, Some("localhost")));
        let mut s = try_connect(&cfg).await.unwrap();
        assert_cancelled(&mut *s, "select pg_sleep(30)").await;
    });
}

async fn assert_cancelled(s: &mut dyn Session, sql_text: &str) {
    let handle = s.cancel_handle();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        handle.cancel().await.unwrap();
    });
    let q = Query::new(sql_text);
    let started = std::time::Instant::now();
    {
        let mut stream = s.execute(&q).await.unwrap();
        let mut result = None;
        while let Some(event) = within(10, stream.next_event()).await {
            if let Err(e) = event {
                result = Some(e);
                break;
            }
        }
        assert!(matches!(result, Some(DriverError::Cancelled)), "{result:?}");
    }
    assert!(started.elapsed() < Duration::from_secs(10));
    // The same session keeps working.
    let rs = sql(s, "select 1").await;
    assert_eq!(rs.rows()[0].0[0], Value::Int(1));
}
