// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated TLS checks (needs `SQLER_TEST_MSSQL_CA`, see AGENTS.md §3). The test server
//! forces encryption and its certificate is valid for `localhost` only.

#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{ConnectionConfig, TlsConfig, TlsMode, Value};

use common::{config, db_env, run, sql, try_connect};

/// `(url on localhost, url on 127.0.0.1, CA path)` of the test server.
fn env() -> Option<(String, String, String)> {
    let url = common::url()?;
    let ca = db_env("SQLER_TEST_MSSQL_CA", "mssql")?;
    let ip = url.replace("@localhost", "@127.0.0.1");
    assert_ne!(url, ip, "the test URL must use localhost");
    Some((url, ip, ca))
}

async fn encrypted(config: &ConnectionConfig) -> Value {
    let mut s = try_connect(config).await.unwrap();
    let rs = sql(
        &mut *s,
        "select encrypt_option from sys.dm_exec_connections where session_id = @@spid",
    )
    .await;
    rs.rows()[0].0[0].clone()
}

fn yes() -> Value {
    Value::Text("TRUE".to_owned())
}

#[test]
fn default_is_prefer_and_encrypted() {
    let Some((url, _, _)) = env() else { return };
    run(async {
        assert_eq!(encrypted(&config(&url)).await, yes());
    });
}

#[test]
fn prefer_and_require_succeed() {
    let Some((_, ip, _)) = env() else { return };
    run(async {
        for mode in [TlsMode::Prefer, TlsMode::Require] {
            let cfg = config(&ip).with_tls(TlsConfig::new(mode));
            assert_eq!(encrypted(&cfg).await, yes(), "{mode:?}");
        }
    });
}

#[test]
fn verify_full_with_ca_to_localhost() {
    let Some((url, _, ca)) = env() else { return };
    run(async {
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::VerifyFull).with_ca_cert(&ca));
        assert_eq!(encrypted(&cfg).await, yes());
    });
}

#[test]
fn verify_full_to_ip_fails() {
    let Some((_, ip, ca)) = env() else { return };
    run(async {
        let cfg = config(&ip).with_tls(TlsConfig::new(TlsMode::VerifyFull).with_ca_cert(&ca));
        let err = try_connect(&cfg).await.err().unwrap();
        assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
        assert!(!err.to_string().contains("127.0.0.1"), "{err}");
        assert!(!err.to_string().contains("Sqler-Test-1pw"), "{err}");
    });
}

#[test]
fn verify_full_with_server_name_to_ip() {
    let Some((_, ip, ca)) = env() else { return };
    run(async {
        let cfg = config(&ip).with_tls(
            TlsConfig::new(TlsMode::VerifyFull)
                .with_ca_cert(&ca)
                .with_server_name("localhost"),
        );
        assert_eq!(encrypted(&cfg).await, yes());
    });
}

#[test]
fn verify_full_without_ca_fails() {
    let Some((url, _, _)) = env() else { return };
    run(async {
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::VerifyFull));
        let err = try_connect(&cfg).await.err().unwrap();
        assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
    });
}

#[test]
fn verify_ca_is_unsupported() {
    let Some((url, _, ca)) = env() else { return };
    run(async {
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::VerifyCa).with_ca_cert(&ca));
        let err = try_connect(&cfg).await.err().unwrap();
        assert!(matches!(err, DriverError::Unsupported(_)), "{err:?}");
    });
}

#[test]
fn disable_against_forced_tls_errors_without_hanging() {
    let Some((url, _, _)) = env() else { return };
    run(async {
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::Disable));
        let result = tokio::time::timeout(Duration::from_secs(25), try_connect(&cfg))
            .await
            .expect("the connect hung");
        assert!(result.is_err());
    });
}
