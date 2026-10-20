// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Connect options from a `ConnectionConfig`: the target URL plus the TLS settings.

use std::str::FromStr as _;

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{ConnectionConfig, TlsMode};
use sqlx::ConnectOptions as _;
use sqlx::postgres::{PgConnectOptions, PgSslMode};

/// The URL query keys that carry TLS settings (including the `sqlx` aliases).
const SSL_KEYS: [&str; 9] = [
    "sslmode",
    "ssl-mode",
    "sslrootcert",
    "ssl-root-cert",
    "ssl-ca",
    "sslcert",
    "ssl-cert",
    "sslkey",
    "ssl-key",
];

/// Removes the TLS query keys from `url`; every other part stays as it is.
pub(crate) fn strip_ssl_params(url: &str) -> String {
    let (rest, fragment) = match url.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (url, None),
    };
    let mut out = match rest.split_once('?') {
        None => rest.to_owned(),
        Some((base, query)) => {
            let kept: Vec<&str> = query
                .split('&')
                .filter(|pair| {
                    let key = pair.split_once('=').map_or(*pair, |(key, _)| key);
                    !pair.is_empty() && !SSL_KEYS.contains(&key)
                })
                .collect();
            if kept.is_empty() {
                base.to_owned()
            } else {
                format!("{base}?{}", kept.join("&"))
            }
        }
    };
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

fn ssl_mode(mode: TlsMode) -> Result<PgSslMode, DriverError> {
    Ok(match mode {
        TlsMode::Disable => PgSslMode::Disable,
        TlsMode::Prefer => PgSslMode::Prefer,
        TlsMode::Require => PgSslMode::Require,
        TlsMode::VerifyCa => PgSslMode::VerifyCa,
        TlsMode::VerifyFull => PgSslMode::VerifyFull,
        _ => {
            return Err(DriverError::Unsupported(
                "TLS mode is not supported by the PostgreSQL driver".to_owned(),
            ));
        }
    })
}

/// Builds the connect options for `config`.
///
/// With `tls: None` the URL's own `sslmode`, `sslrootcert`, `sslcert` and `sslkey` apply (default
/// `prefer`). With `tls: Some`, those keys are dropped from the URL and the explicit settings win.
///
/// Errors never contain the target.
pub(crate) fn connect_options(config: &ConnectionConfig) -> Result<PgConnectOptions, DriverError> {
    let target = match &config.tls {
        Some(_) => strip_ssl_params(&config.target),
        None => config.target.clone(),
    };
    let mut opts = PgConnectOptions::from_str(&target)
        .map_err(|e| DriverError::Connection(format!("invalid PostgreSQL target: {e}")))?
        .application_name("sqler")
        .disable_statement_logging();
    if let Some(tls) = &config.tls {
        if tls.client_cert.is_some() != tls.client_key.is_some() {
            return Err(DriverError::Connection(
                "client certificate and key must be set together".to_owned(),
            ));
        }
        opts = opts.ssl_mode(ssl_mode(tls.mode)?);
        if let Some(ca) = &tls.ca_cert {
            opts = opts.ssl_root_cert(ca);
        }
        if let Some(cert) = &tls.client_cert {
            opts = opts.ssl_client_cert(cert);
        }
        if let Some(key) = &tls.client_key {
            opts = opts.ssl_client_key(key);
        }
    }
    Ok(opts)
}

/// The name to verify the certificate against, when it is not the dial host.
///
/// Only with `verify-full` on a TCP target: the explicit `TlsConfig::server_name`, unless it equals
/// the dial host (ASCII case-insensitive, brackets trimmed). Every other case gives `None`.
pub(crate) fn server_name_override(
    config: &ConnectionConfig,
    opts: &PgConnectOptions,
) -> Option<String> {
    let name = config.tls.as_ref()?.server_name.as_deref()?;
    if !matches!(opts.get_ssl_mode(), PgSslMode::VerifyFull) {
        return None;
    }
    let host = opts.get_host();
    // A socket target (`socket` set, or a host that is a directory) is not TCP.
    if opts.get_socket().is_some() || host.starts_with('/') {
        return None;
    }
    let trim = |s: &str| s.trim_matches(['[', ']']).to_owned();
    let name = trim(name);
    if name.is_empty() || name.eq_ignore_ascii_case(&trim(host)) {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{DriverId, TlsConfig};

    use super::*;

    fn config(target: &str) -> ConnectionConfig {
        ConnectionConfig::new("t", DriverId::new("postgres").unwrap(), target)
    }

    const URL: &str = "postgres://u:p@localhost:5432/db";

    #[test]
    fn strips_only_ssl_keys() {
        assert_eq!(strip_ssl_params(URL), URL);
        assert_eq!(strip_ssl_params(&format!("{URL}?sslmode=require")), URL);
        assert_eq!(
            strip_ssl_params(&format!(
                "{URL}?sslmode=require&application_name=x&sslrootcert=/a&sslcert=/b&sslkey=/c&ssl-key=/d"
            )),
            format!("{URL}?application_name=x")
        );
        assert_eq!(
            strip_ssl_params(&format!("{URL}?a=1&ssl-mode=disable&ssl-ca=/x&b=2#frag")),
            format!("{URL}?a=1&b=2#frag")
        );
    }

    #[test]
    fn url_ssl_params_apply_without_tls() {
        let opts = connect_options(&config(&format!("{URL}?sslmode=verify-full"))).unwrap();
        assert!(matches!(opts.get_ssl_mode(), PgSslMode::VerifyFull));
        let opts = connect_options(&config(URL)).unwrap();
        assert!(matches!(opts.get_ssl_mode(), PgSslMode::Prefer));
        assert_eq!(opts.get_application_name(), Some("sqler"));
    }

    #[test]
    fn every_mode_maps() {
        let cases = [
            (TlsMode::Disable, PgSslMode::Disable),
            (TlsMode::Prefer, PgSslMode::Prefer),
            (TlsMode::Require, PgSslMode::Require),
            (TlsMode::VerifyCa, PgSslMode::VerifyCa),
            (TlsMode::VerifyFull, PgSslMode::VerifyFull),
        ];
        for (mode, expected) in cases {
            let cfg = config(&format!("{URL}?sslmode=disable")).with_tls(TlsConfig::new(mode));
            let opts = connect_options(&cfg).unwrap();
            assert_eq!(
                std::mem::discriminant(&opts.get_ssl_mode()),
                std::mem::discriminant(&expected),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn explicit_tls_overrides_url() {
        let cfg = config(&format!("{URL}?sslmode=disable&sslrootcert=/nope"))
            .with_tls(TlsConfig::new(TlsMode::Require));
        let opts = connect_options(&cfg).unwrap();
        assert!(matches!(opts.get_ssl_mode(), PgSslMode::Require));
    }

    #[test]
    fn files_are_accepted() {
        let tls = TlsConfig::new(TlsMode::VerifyCa)
            .with_ca_cert("/ca.pem")
            .with_client_cert("/c.pem")
            .with_client_key("/k.pem");
        assert!(connect_options(&config(URL).with_tls(tls)).is_ok());
        let tls = TlsConfig::new(TlsMode::VerifyCa).with_ca_cert("/ca.pem");
        assert!(connect_options(&config(URL).with_tls(tls)).is_ok());
    }

    #[test]
    fn cert_and_key_come_together() {
        for tls in [
            TlsConfig::new(TlsMode::Require).with_client_cert("/c.pem"),
            TlsConfig::new(TlsMode::Require).with_client_key("/k.pem"),
        ] {
            let err = connect_options(&config(URL).with_tls(tls)).unwrap_err();
            assert!(matches!(
                err,
                DriverError::Connection(ref m) if m == "client certificate and key must be set together"
            ));
        }
    }

    #[test]
    fn bad_url_error_has_no_secret() {
        let err = connect_options(&config("postgres://u:s3cret@host:notaport/x")).unwrap_err();
        assert!(matches!(err, DriverError::Connection(_)));
        assert!(!err.to_string().contains("s3cret"));
    }

    fn named(target: &str, mode: TlsMode, name: &str) -> (ConnectionConfig, PgConnectOptions) {
        let cfg = config(target).with_tls(TlsConfig::new(mode).with_server_name(name));
        let opts = connect_options(&cfg).unwrap();
        (cfg, opts)
    }

    #[test]
    fn server_name_applies_for_verify_full_and_a_different_name() {
        let (cfg, opts) = named(
            "postgres://u:p@127.0.0.1:5432/db",
            TlsMode::VerifyFull,
            "db.internal",
        );
        assert_eq!(
            server_name_override(&cfg, &opts).as_deref(),
            Some("db.internal")
        );
    }

    #[test]
    fn server_name_is_ignored_when_equal_or_not_verify_full() {
        let (cfg, opts) = named(URL, TlsMode::VerifyFull, "LocalHost");
        assert_eq!(server_name_override(&cfg, &opts), None);
        let (cfg, opts) = named("postgres://u:p@[::1]:5432/db", TlsMode::VerifyFull, "[::1]");
        assert_eq!(server_name_override(&cfg, &opts), None);
        for mode in [
            TlsMode::Disable,
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyCa,
        ] {
            let (cfg, opts) = named("postgres://u:p@127.0.0.1/db", mode, "db.internal");
            assert_eq!(server_name_override(&cfg, &opts), None, "{mode:?}");
        }
    }

    #[test]
    fn server_name_is_ignored_without_a_name_or_for_sockets() {
        let cfg =
            config("postgres://u:p@127.0.0.1/db").with_tls(TlsConfig::new(TlsMode::VerifyFull));
        let opts = connect_options(&cfg).unwrap();
        assert_eq!(server_name_override(&cfg, &opts), None);
        let cfg = config("postgres://u:p@127.0.0.1/db");
        let opts = connect_options(&cfg).unwrap();
        assert_eq!(server_name_override(&cfg, &opts), None);
        let (cfg, opts) = named(
            "postgres://u:p@127.0.0.1/db",
            TlsMode::VerifyFull,
            "db.internal",
        );
        let opts = opts.socket("/var/run/postgresql");
        assert_eq!(server_name_override(&cfg, &opts), None);
        let (cfg, opts) = named(
            "postgres://u:p@%2Fvar%2Frun/db",
            TlsMode::VerifyFull,
            "db.internal",
        );
        assert_eq!(server_name_override(&cfg, &opts), None);
    }
}
