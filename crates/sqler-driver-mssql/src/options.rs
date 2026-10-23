// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Connect options from a `ConnectionConfig`: the target URL plus the TLS settings.
//!
//! Nothing here prints a `tiberius::Config`, a target or a password.

use std::path::PathBuf;

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{ConnectionConfig, TlsMode};
use tiberius::{AuthMethod, Config, EncryptionLevel};

const DEFAULT_PORT: u16 = 1433;

/// The parts of a `sqlserver://` URL.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) user: String,
    pub(crate) password: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) database: Option<String>,
}

// A manual `Debug` keeps the password out of any debug output.
impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

fn invalid(what: &str) -> DriverError {
    DriverError::Connection(format!("invalid SQL Server target: {what}"))
}

/// Percent-decodes `s` by hand; a bad escape or non-UTF-8 result is an error.
fn decode(s: &str, what: &str) -> Result<String, DriverError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| invalid(&format!("bad percent-encoding in the {what}")))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| invalid(&format!("the {what} is not valid UTF-8")))
}

/// Splits `host[:port]`, with `[v6]` brackets.
fn host_port(authority: &str) -> Result<(String, u16), DriverError> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| invalid("unclosed `[` in the host"))?;
        match tail {
            "" => (host, None),
            _ => (
                host,
                Some(
                    tail.strip_prefix(':')
                        .ok_or_else(|| invalid("unexpected text after the host"))?,
                ),
            ),
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Err(invalid("the host is missing"));
    }
    let port = match port {
        None | Some("") => DEFAULT_PORT,
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| invalid("the port is not a number from 1 to 65535"))?,
    };
    Ok((decode(host, "host")?, port))
}

/// Parses `sqlserver://[user[:password]@]host[:port][/database]` (`mssql://` is an alias).
///
/// The query must be empty: any key is rejected, and the error names the key, never the value.
/// Errors never contain the target.
pub(crate) fn parse_target(target: &str) -> Result<Target, DriverError> {
    let (scheme, rest) = target
        .split_once("://")
        .ok_or_else(|| invalid("expected sqlserver://[user[:password]@]host[:port][/database]"))?;
    if !scheme.eq_ignore_ascii_case("sqlserver") && !scheme.eq_ignore_ascii_case("mssql") {
        return Err(invalid("the scheme must be sqlserver:// or mssql://"));
    }
    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => (rest, query),
        None => (rest, ""),
    };
    if let Some(pair) = query.split('&').find(|p| !p.is_empty()) {
        let key = pair.split_once('=').map_or(pair, |(key, _)| key);
        return Err(DriverError::Connection(format!(
            "unsupported option `{key}`"
        )));
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => (userinfo, hostport),
        None => ("", authority),
    };
    let (user, password) = match userinfo.split_once(':') {
        Some((user, password)) => (user, password),
        None => (userinfo, ""),
    };
    let (host, port) = host_port(hostport)?;
    let database = match path {
        "" => None,
        path => Some(decode(path, "database")?),
    };
    Ok(Target {
        user: decode(user, "user")?,
        password: decode(password, "password")?,
        host,
        port,
        database,
    })
}

/// How the server certificate is trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Trust {
    /// Any certificate (`prefer`, `require`: encrypted, not authenticated).
    Anything,
    /// The OS roots.
    SystemRoots,
    /// The CA in this file (in addition to the OS roots).
    CaFile(PathBuf),
}

/// The encryption level, as a type of our own so tests can compare it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encryption {
    NotSupported,
    On,
    Required,
}

/// The TLS settings derived from `ConnectionConfig::tls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TlsPlan {
    pub(crate) encryption: Encryption,
    pub(crate) trust: Trust,
    /// The name the certificate is checked against, when it is not the dial host.
    pub(crate) server_name: Option<String>,
}

fn unsupported(text: &str) -> DriverError {
    DriverError::Unsupported(text.to_owned())
}

/// Maps the TLS settings of `config` (`None` means `prefer`).
pub(crate) fn tls_plan(config: &ConnectionConfig) -> Result<TlsPlan, DriverError> {
    let Some(tls) = &config.tls else {
        return Ok(TlsPlan {
            encryption: Encryption::On,
            trust: Trust::Anything,
            server_name: None,
        });
    };
    if tls.client_cert.is_some() || tls.client_key.is_some() {
        return Err(unsupported(
            "client certificates are not available for SQL Server",
        ));
    }
    let plan = |encryption, trust, server_name| TlsPlan {
        encryption,
        trust,
        server_name,
    };
    match tls.mode {
        TlsMode::Disable => Ok(plan(Encryption::NotSupported, Trust::Anything, None)),
        TlsMode::Prefer => Ok(plan(Encryption::On, Trust::Anything, None)),
        TlsMode::Require => Ok(plan(Encryption::Required, Trust::Anything, None)),
        TlsMode::VerifyFull => {
            let trust = tls
                .ca_cert
                .clone()
                .map_or(Trust::SystemRoots, Trust::CaFile);
            let name = tls.server_name.clone().filter(|n| !n.is_empty());
            Ok(plan(Encryption::Required, trust, name))
        }
        TlsMode::VerifyCa => Err(unsupported(
            "verify-ca is not available for SQL Server; use verify-full or require",
        )),
        _ => Err(unsupported(
            "this TLS mode is not supported by the SQL Server driver",
        )),
    }
}

/// Builds the tiberius config and returns it with the dial host and port.
pub(crate) fn build(config: &ConnectionConfig) -> Result<(Config, Target), DriverError> {
    let target = parse_target(&config.target)?;
    let plan = tls_plan(config)?;
    let mut cfg = Config::new();
    cfg.host(&target.host);
    cfg.port(target.port);
    if let Some(database) = &target.database {
        cfg.database(database);
    }
    cfg.application_name("sqler");
    cfg.authentication(AuthMethod::sql_server(&target.user, &target.password));
    cfg.encryption(match plan.encryption {
        Encryption::NotSupported => EncryptionLevel::NotSupported,
        Encryption::On => EncryptionLevel::On,
        Encryption::Required => EncryptionLevel::Required,
    });
    match &plan.trust {
        Trust::Anything => cfg.trust_cert(),
        Trust::SystemRoots => {}
        Trust::CaFile(path) => cfg.trust_cert_ca(path.to_string_lossy()),
    }
    if let Some(name) = &plan.server_name {
        cfg.hostname_in_certificate(name);
    }
    Ok((cfg, target))
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{DriverId, TlsConfig};

    use super::*;

    fn config(target: &str) -> ConnectionConfig {
        ConnectionConfig::new("t", DriverId::new("sqlserver").unwrap(), target)
    }

    const URL: &str = "sqlserver://sa:pw@localhost:1444/master";

    #[test]
    fn parses_all_parts() {
        let t = parse_target(URL).unwrap();
        assert_eq!(t.user, "sa");
        assert_eq!(t.password, "pw");
        assert_eq!(t.host, "localhost");
        assert_eq!(t.port, 1444);
        assert_eq!(t.database.as_deref(), Some("master"));
    }

    #[test]
    fn defaults() {
        let t = parse_target("sqlserver://db.example").unwrap();
        assert_eq!((t.user.as_str(), t.password.as_str()), ("", ""));
        assert_eq!(t.port, 1433);
        assert_eq!(t.database, None);
        let t = parse_target("mssql://u@h/").unwrap();
        assert_eq!((t.user.as_str(), t.host.as_str(), t.port), ("u", "h", 1433));
        assert_eq!(t.database, None);
        assert_eq!(parse_target("sqlserver://u:@h").unwrap().password, "");
        assert_eq!(parse_target("SQLSERVER://h").unwrap().host, "h");
    }

    #[test]
    fn percent_decoding() {
        let t = parse_target("sqlserver://a%40b:p%3Ass%2Fw%40rd@h%61st:1/d%20b").unwrap();
        assert_eq!(t.user, "a@b");
        assert_eq!(t.password, "p:ss/w@rd");
        assert_eq!(t.host, "hast");
        assert_eq!(t.database.as_deref(), Some("d b"));
        // The last `@` separates the user info, so a raw `@` in the password still parses.
        assert_eq!(parse_target("sqlserver://u:p@w@h").unwrap().password, "p@w");
        let t = parse_target("sqlserver://u:%C3%A9@h").unwrap();
        assert_eq!(t.password, "\u{e9}");
    }

    #[test]
    fn ipv6_host() {
        let t = parse_target("sqlserver://u:p@[::1]:1500/x").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 1500));
        assert_eq!(parse_target("sqlserver://[::1]").unwrap().port, 1433);
    }

    #[test]
    fn rejects_bad_targets_without_echoing_them() {
        for bad in [
            "postgres://u:s3cret@h/x",
            "s3cret",
            "sqlserver://u:s3cret@h:notaport/x",
            "sqlserver://u:s3cret@h:0/x",
            "sqlserver://u:s3cret@h:99999/x",
            "sqlserver://u:s3cret@/x",
            "sqlserver://u:s3cret@[::1/x",
            "sqlserver://u:%zz@h/x",
            "sqlserver://u:%ff%fe@h/x",
            "sqlserver://u:s3cret@h/x?foo=s3cret",
        ] {
            let err = parse_target(bad).unwrap_err();
            assert!(matches!(err, DriverError::Connection(_)), "{bad}");
            let text = err.to_string();
            assert!(!text.contains("s3cret"), "{text}");
        }
    }

    #[test]
    fn query_keys_are_rejected_by_name() {
        let err = parse_target("sqlserver://h/x?TrustServerCertificate=true").unwrap_err();
        assert_eq!(
            err.to_string(),
            "connection failed: unsupported option `TrustServerCertificate`"
        );
        let err = parse_target("sqlserver://h/x?a=1&b=2").unwrap_err();
        assert!(err.to_string().contains("`a`"));
        assert!(parse_target("sqlserver://h/x?").is_ok());
    }

    #[test]
    fn debug_hides_the_password() {
        let t = parse_target("sqlserver://u:s3cret@h").unwrap();
        assert!(!format!("{t:?}").contains("s3cret"));
    }

    fn plan(tls: Option<TlsConfig>) -> Result<TlsPlan, DriverError> {
        let mut cfg = config(URL);
        if let Some(tls) = tls {
            cfg = cfg.with_tls(tls);
        }
        tls_plan(&cfg)
    }

    fn mode(mode: TlsMode) -> Result<TlsPlan, DriverError> {
        plan(Some(TlsConfig::new(mode)))
    }

    #[test]
    fn tls_none_is_prefer() {
        let p = plan(None).unwrap();
        assert_eq!((p.encryption, p.trust), (Encryption::On, Trust::Anything));
    }

    #[test]
    fn every_tls_mode_maps() {
        let p = mode(TlsMode::Disable).unwrap();
        assert_eq!(p.encryption, Encryption::NotSupported);
        let p = mode(TlsMode::Prefer).unwrap();
        assert_eq!((p.encryption, p.trust), (Encryption::On, Trust::Anything));
        let p = mode(TlsMode::Require).unwrap();
        assert_eq!(
            (p.encryption, p.trust),
            (Encryption::Required, Trust::Anything)
        );
        let p = mode(TlsMode::VerifyFull).unwrap();
        assert_eq!(
            (p.encryption, p.trust, p.server_name),
            (Encryption::Required, Trust::SystemRoots, None)
        );
        let p = plan(Some(
            TlsConfig::new(TlsMode::VerifyFull).with_ca_cert("/ca.pem"),
        ))
        .unwrap();
        assert_eq!(p.trust, Trust::CaFile(PathBuf::from("/ca.pem")));
        let err = mode(TlsMode::VerifyCa).unwrap_err();
        assert!(matches!(err, DriverError::Unsupported(ref m) if m.contains("verify-ca")));
    }

    #[test]
    fn server_name_applies_to_verify_full_only() {
        let named = |m| plan(Some(TlsConfig::new(m).with_server_name("db.internal"))).unwrap();
        assert_eq!(
            named(TlsMode::VerifyFull).server_name.as_deref(),
            Some("db.internal")
        );
        assert_eq!(named(TlsMode::Require).server_name, None);
    }

    #[test]
    fn client_certificates_are_unsupported() {
        for tls in [
            TlsConfig::new(TlsMode::Require).with_client_cert("/c.pem"),
            TlsConfig::new(TlsMode::Require).with_client_key("/k.pem"),
        ] {
            let err = plan(Some(tls)).unwrap_err();
            assert!(matches!(err, DriverError::Unsupported(_)));
        }
    }

    #[test]
    fn build_succeeds_for_every_supported_mode() {
        for m in [
            TlsMode::Disable,
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyFull,
        ] {
            let (_, target) = build(&config(URL).with_tls(TlsConfig::new(m))).unwrap();
            assert_eq!(target.host, "localhost");
        }
        let tls = TlsConfig::new(TlsMode::VerifyFull)
            .with_ca_cert("/ca.pem")
            .with_server_name("db.internal");
        assert!(build(&config(URL).with_tls(tls)).is_ok());
    }

    #[test]
    fn no_error_contains_the_password() {
        let cfg = config("sqlserver://u:s3cret@h:1/x").with_tls(TlsConfig::new(TlsMode::VerifyCa));
        assert!(!build(&cfg).unwrap_err().to_string().contains("s3cret"));
        let cfg = config("postgres://u:s3cret@h/x");
        assert!(!build(&cfg).unwrap_err().to_string().contains("s3cret"));
    }
}
