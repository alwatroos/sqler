// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `verify-full` against a server name other than the dial address (ADR-0019), against the server
//! of `SQLER_TEST_MYSQL_URL`. A TCP forwarder stands in for an SSH tunnel: the driver dials its
//! port on `127.0.0.1`, and the certificate (issued for `localhost`) is checked against the
//! configured server name. Without the variable the tests skip (ADR-0009).
//!
//! One test runs the steps one after the other, so the server is started once.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use common::{Server, config, forwarder, run, start, url_port};
use sqler_driver_api::sqler_core::{ConnectionConfig, Query, TlsConfig, TlsMode, Value};
use sqler_driver_api::{Driver as _, DriverError, Session, collect};
use sqler_driver_mysql::MySqlDriver;

async fn sql(session: &mut dyn Session, text: &str) -> sqler_driver_api::sqler_core::ResultSet {
    let query = Query::new(text);
    let mut stream = session.execute(&query).await.unwrap();
    collect(&mut *stream).await.unwrap()
}

async fn cipher(session: &mut dyn Session) -> String {
    let rs = sql(session, "show session status like 'Ssl_cipher'").await;
    let i = rs.columns().iter().position(|c| c.name == "Value").unwrap();
    match rs.rows()[0].get(i).unwrap() {
        Value::Text(text) => text.clone(),
        other => panic!("unexpected {other:?}"),
    }
}

/// The config that dials the forwarder, with `tls`.
fn through(url: &str, forward: u16, tls: TlsConfig) -> ConnectionConfig {
    let direct = url_port(url);
    let url = url.replace(&format!(":{direct}"), &format!(":{forward}"));
    config(&url).with_tls(tls)
}

async fn connect_err(cfg: &ConnectionConfig) -> DriverError {
    MySqlDriver::new().connect(cfg).await.err().unwrap()
}

#[test]
fn server_name_steps() {
    run(async {
        if let Some(server) = start().await {
            steps(&server).await;
        }
    });
}

async fn steps(server: &Server) {
    let Some(ca) = &server.ca else {
        println!("skipped: SQLER_TEST_MYSQL_CA is not set (see AGENTS.md §3)");
        return;
    };
    let url = server.url();
    let forward = forwarder(url_port(&url)).await;
    let tls = |mode| TlsConfig::new(mode).with_ca_cert(ca);

    // The matching name passes verify-full although the dial address is 127.0.0.1.
    eprintln!("matching_name");
    let cfg = through(
        &url,
        forward,
        tls(TlsMode::VerifyFull).with_server_name("localhost"),
    );
    let mut s = connect_via(&cfg).await;
    assert!(!cipher(&mut *s).await.is_empty(), "verify-full + name");
    s.close().await.unwrap();

    // A name the certificate does not carry fails closed, without the password in the text.
    eprintln!("wrong_name");
    let cfg = through(
        &url,
        forward,
        tls(TlsMode::VerifyFull).with_server_name("wrong.example"),
    );
    let err = connect_err(&cfg).await;
    assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
    let text = format!("{err} {err:?}");
    assert!(!text.contains(common::PASSWORD), "{text}");

    // Without a name the dial address is verified, and 127.0.0.1 is not in the certificate.
    eprintln!("no_name");
    let cfg = through(&url, forward, tls(TlsMode::VerifyFull));
    let err = connect_err(&cfg).await;
    assert!(matches!(err, DriverError::Connection(_)), "{err:?}");

    // Other modes ignore the name: verify-ca does not check the host name.
    eprintln!("verify_ca_ignores_name");
    let cfg = through(
        &url,
        forward,
        tls(TlsMode::VerifyCa).with_server_name("wrong.example"),
    );
    let mut s = connect_via(&cfg).await;
    assert!(!cipher(&mut *s).await.is_empty(), "verify-ca");
    s.close().await.unwrap();

    // The cancel side connection goes through the same bridge.
    eprintln!("cancel_through_bridge");
    let cfg = through(
        &url,
        forward,
        tls(TlsMode::VerifyFull).with_server_name("localhost"),
    );
    let mut s = connect_via(&cfg).await;
    let handle = s.cancel_handle();
    let killer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        handle.cancel().await
    });
    let started = Instant::now();
    let sleep = Query::new("select sleep(30)");
    let mut stream = s.execute(&sleep).await.unwrap();
    let err = collect(&mut *stream).await.unwrap_err();
    drop(stream);
    assert!(matches!(err, DriverError::Cancelled), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    killer.await.unwrap().unwrap();
    assert_eq!(sql(&mut *s, "select 1 as one").await.len(), 1);
    s.close().await.unwrap();
}

async fn connect_via(cfg: &ConnectionConfig) -> Box<dyn Session> {
    MySqlDriver::new().connect(cfg).await.unwrap()
}
