// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated TLS checks (needs `SQLER_TEST_POSTGRES_CA`, see AGENTS.md §3).

#![allow(clippy::unwrap_used)]

mod common;

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{TlsConfig, TlsMode, Value};

use common::{config, db_env, run, sql, try_connect};

/// `(url on 127.0.0.1, url on localhost, CA path)` of the test server.
fn env() -> Option<(String, String, String)> {
    let url = common::url()?;
    let ca = db_env("SQLER_TEST_POSTGRES_CA", "postgres")?;
    let localhost = url.replace("@127.0.0.1", "@localhost");
    assert_ne!(url, localhost, "the test URL must use 127.0.0.1");
    Some((url, localhost, ca))
}

async fn ssl_in_use(config: &sqler_driver_api::sqler_core::ConnectionConfig) -> Value {
    let mut s = try_connect(config).await.unwrap();
    let rs = sql(
        &mut *s,
        "select ssl from pg_stat_ssl where pid = pg_backend_pid()",
    )
    .await;
    rs.rows()[0].0[0].clone()
}

#[test]
fn verify_full_to_localhost() {
    let Some((_, localhost, ca)) = env() else {
        return;
    };
    run(async {
        let cfg =
            config(&localhost).with_tls(TlsConfig::new(TlsMode::VerifyFull).with_ca_cert(&ca));
        assert_eq!(ssl_in_use(&cfg).await, Value::Bool(true));
    });
}

#[test]
fn verify_ca_to_ip() {
    let Some((url, _, ca)) = env() else { return };
    run(async {
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::VerifyCa).with_ca_cert(&ca));
        assert_eq!(ssl_in_use(&cfg).await, Value::Bool(true));
    });
}

#[test]
fn verify_full_to_ip_fails() {
    let Some((url, _, ca)) = env() else { return };
    run(async {
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::VerifyFull).with_ca_cert(&ca));
        let err = try_connect(&cfg).await.err().unwrap();
        assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
        assert!(!err.to_string().contains("postgres:sqler"));
    });
}

#[test]
fn verify_without_the_ca_fails() {
    let Some((_, localhost, _)) = env() else {
        return;
    };
    run(async {
        // The test CA is not a trusted root.
        let cfg = config(&localhost).with_tls(TlsConfig::new(TlsMode::VerifyFull));
        let err = try_connect(&cfg).await.err().unwrap();
        assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
    });
}

#[test]
fn disable_has_no_ssl() {
    let Some((url, _, _)) = env() else { return };
    run(async {
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::Disable));
        assert_eq!(ssl_in_use(&cfg).await, Value::Bool(false));
    });
}

#[test]
fn require_works_without_the_ca() {
    let Some((url, _, _)) = env() else { return };
    run(async {
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::Require));
        assert_eq!(ssl_in_use(&cfg).await, Value::Bool(true));
    });
}

#[test]
fn prefer_uses_ssl_when_offered() {
    let Some((url, _, _)) = env() else { return };
    run(async {
        assert_eq!(ssl_in_use(&config(&url)).await, Value::Bool(true));
    });
}

#[test]
fn url_parameters_apply_without_a_tls_config() {
    let Some((_, localhost, ca)) = env() else {
        return;
    };
    run(async {
        let url = format!("{localhost}?sslmode=verify-full&sslrootcert={ca}");
        assert_eq!(ssl_in_use(&config(&url)).await, Value::Bool(true));
        // An explicit config overrides the URL's own sslmode.
        let cfg = config(&url).with_tls(TlsConfig::new(TlsMode::Disable));
        assert_eq!(ssl_in_use(&cfg).await, Value::Bool(false));
    });
}

#[test]
fn verify_full_with_a_wrong_ca_fails() {
    let Some((_, localhost, ca)) = env() else {
        return;
    };
    // A second, unrelated CA next to the real one.
    let other = std::path::Path::new(&ca).with_file_name("other-ca.crt");
    let key = std::path::Path::new(&ca).with_file_name("other-ca.key");
    let made = std::process::Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        ])
        .args(["-subj", "/CN=other-ca", "-keyout"])
        .arg(&key)
        .arg("-out")
        .arg(&other)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(made.success());
    run(async {
        let cfg = config(&localhost)
            .with_tls(TlsConfig::new(TlsMode::VerifyFull).with_ca_cert(other.to_str().unwrap()));
        let err = try_connect(&cfg).await.err().unwrap();
        assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
    });
}
