// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fmt;
use std::path::PathBuf;

use crate::CoreError;

/// Identifier of a database driver, for example `sqlite`.
///
/// Non-empty and made of `[a-z0-9-]` only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DriverId(String);

impl DriverId {
    /// Validates and wraps a driver id.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidDriverId`] if `id` is empty or contains a
    /// character outside `[a-z0-9-]`.
    pub fn new(id: &str) -> Result<Self, CoreError> {
        let ok = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if ok {
            Ok(Self(id.to_owned()))
        } else {
            Err(CoreError::InvalidDriverId(id.to_owned()))
        }
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DriverId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The kind of environment a connection points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Environment {
    /// A development database.
    Dev,
    /// A staging database.
    Staging,
    /// A production database.
    Prod,
}

impl Environment {
    /// The lowercase identifier: `dev`, `staging` or `prod`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Staging => "staging",
            Self::Prod => "prod",
        }
    }

    /// Parses an identifier produced by [`Environment::as_str`] (exact lowercase match).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "dev" => Some(Self::Dev),
            "staging" => Some(Self::Staging),
            "prod" => Some(Self::Prod),
            _ => None,
        }
    }
}

/// How a driver secures the network connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum TlsMode {
    /// No TLS.
    Disable,
    /// Use TLS if the server offers it.
    #[default]
    Prefer,
    /// Require TLS; do not verify the certificate.
    Require,
    /// Require TLS and verify the certificate chain.
    VerifyCa,
    /// Require TLS, verify the chain and the host name.
    VerifyFull,
}

impl TlsMode {
    /// The identifier: `disable`, `prefer`, `require`, `verify-ca` or `verify-full`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }

    /// Parses an identifier produced by [`TlsMode::as_str`] (exact lowercase match).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "disable" => Some(Self::Disable),
            "prefer" => Some(Self::Prefer),
            "require" => Some(Self::Require),
            "verify-ca" => Some(Self::VerifyCa),
            "verify-full" => Some(Self::VerifyFull),
            _ => None,
        }
    }
}

/// TLS options for a network connection. The paths are not secret.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct TlsConfig {
    /// The TLS mode.
    pub mode: TlsMode,
    /// Root certificate (CA) file.
    pub ca_cert: Option<PathBuf>,
    /// Client certificate file.
    pub client_cert: Option<PathBuf>,
    /// Client private key file.
    pub client_key: Option<PathBuf>,
}

impl TlsConfig {
    /// A config with `mode` and no files.
    #[must_use]
    pub fn new(mode: TlsMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Sets the CA certificate file.
    #[must_use]
    pub fn with_ca_cert(mut self, path: impl Into<PathBuf>) -> Self {
        self.ca_cert = Some(path.into());
        self
    }

    /// Sets the client certificate file.
    #[must_use]
    pub fn with_client_cert(mut self, path: impl Into<PathBuf>) -> Self {
        self.client_cert = Some(path.into());
        self
    }

    /// Sets the client key file.
    #[must_use]
    pub fn with_client_key(mut self, path: impl Into<PathBuf>) -> Self {
        self.client_key = Some(path.into());
        self
    }
}

/// How to reach one database.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionConfig {
    /// Human-readable name of the connection.
    pub name: String,
    /// The driver that handles this connection.
    pub driver: DriverId,
    /// Driver-specific DSN: a file path, `:memory:` or a URL.
    ///
    /// Secrets in the target move to the OS keychain in M2.
    pub target: String,
    /// Optional environment tag (dev, staging, prod).
    pub environment: Option<Environment>,
    /// Optional accent colour as lowercase `#rrggbb`; validated by `sqler-config`.
    pub color: Option<String>,
    /// TLS options. `None` means the driver follows the target's own parameters (such as
    /// `sslmode=`) and otherwise uses `prefer`; `Some` is explicit and overrides them.
    pub tls: Option<TlsConfig>,
}

impl ConnectionConfig {
    /// Creates a connection config.
    #[must_use]
    pub fn new(name: impl Into<String>, driver: DriverId, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver,
            target: target.into(),
            environment: None,
            color: None,
            tls: None,
        }
    }

    /// Sets the environment tag.
    #[must_use]
    pub fn with_environment(mut self, env: Environment) -> Self {
        self.environment = Some(env);
        self
    }

    /// Sets the accent colour.
    #[must_use]
    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Sets the TLS options.
    #[must_use]
    pub fn with_tls(mut self, tls: TlsConfig) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Whether the target embeds a secret (URL userinfo or a secret-looking parameter).
    #[must_use]
    pub fn target_has_secret(&self) -> bool {
        redact_target(&self.target) != self.target
    }
}

impl fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("name", &self.name)
            .field("driver", &self.driver)
            .field("target", &redact_target(&self.target))
            .field("environment", &self.environment)
            .field("color", &self.color)
            .field("tls", &self.tls)
            .finish()
    }
}

/// Masks the userinfo of a URL-style target and the values of secret-looking
/// `key=value` parameters. Never returns the raw target when it carries either.
fn redact_target(target: &str) -> String {
    let mut s = target.to_owned();
    if let Some(i) = target.find("://") {
        let start = i + 3;
        let rest = &target[start..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if let Some(at) = rest[..end].rfind('@') {
            s = format!("{}***@{}", &target[..start], &target[start + at + 1..]);
        }
    }
    mask_secret_params(&s)
}

/// Replaces the value of `password`/`pwd`/`secret`/`token` pairs delimited by
/// `;`, `&` or `?` with `***`.
fn mask_secret_params(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for part in s.split_inclusive([';', '&', '?']) {
        let body = part.trim_end_matches([';', '&', '?']);
        let key = body
            .split_once('=')
            .map(|(k, _)| k.trim().to_ascii_lowercase());
        let secret = key
            .as_deref()
            .is_some_and(|k| matches!(k, "password" | "pwd" | "secret" | "token"));
        match (secret, body.find('=')) {
            (true, Some(eq)) => {
                out.push_str(&body[..=eq]);
                out.push_str("***");
                out.push_str(&part[body.len()..]);
            }
            _ => out.push_str(part),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_id_accepts() {
        for ok in ["sqlite", "postgres", "mssql", "duck-db", "a1", "-", "0"] {
            let id = DriverId::new(ok).unwrap();
            assert_eq!(id.as_str(), ok);
            assert_eq!(id.to_string(), ok);
        }
    }

    #[test]
    fn driver_id_rejects() {
        for bad in ["", "SQLite", "a b", "a_b", "é", "a.b", "a/b"] {
            assert_eq!(
                DriverId::new(bad),
                Err(CoreError::InvalidDriverId(bad.to_owned()))
            );
        }
    }

    #[test]
    fn driver_id_orders() {
        let a = DriverId::new("a").unwrap();
        let b = DriverId::new("b").unwrap();
        assert!(a < b);
    }

    #[test]
    fn config_new() {
        let c = ConnectionConfig::new("dev", DriverId::new("sqlite").unwrap(), ":memory:");
        assert_eq!(c.name, "dev");
        assert_eq!(c.driver.as_str(), "sqlite");
        assert_eq!(c.target, ":memory:");
    }

    #[test]
    fn debug_redacts_target_secrets() {
        let d = DriverId::new("postgres").unwrap();
        let c = ConnectionConfig::new(
            "p",
            d.clone(),
            "postgres://bob:hunter2@db:5432/x?sslmode=require&password=s3",
        );
        let out = format!("{c:?}");
        assert!(!out.contains("hunter2") && !out.contains("bob") && !out.contains("s3"));
        assert!(out.contains("postgres://***@db:5432/x"));
        assert!(out.contains("sslmode=require"));
        let c = ConnectionConfig::new("m", d.clone(), "Server=h;User=sa;Password=abc;Db=x");
        let out = format!("{c:?}");
        assert!(!out.contains("abc") && out.contains("Password=***;Db=x"));
        let c = ConnectionConfig::new("s", d, ":memory:");
        assert!(format!("{c:?}").contains(":memory:"));
    }

    #[test]
    fn environment_ids() {
        for (e, id) in [
            (Environment::Dev, "dev"),
            (Environment::Staging, "staging"),
            (Environment::Prod, "prod"),
        ] {
            assert_eq!(e.as_str(), id);
            assert_eq!(Environment::from_id(id), Some(e));
        }
        for bad in ["", "Dev", "production", " dev"] {
            assert_eq!(Environment::from_id(bad), None);
        }
    }

    #[test]
    fn builders_set_fields() {
        let d = DriverId::new("sqlite").unwrap();
        let c = ConnectionConfig::new("a", d, ":memory:");
        assert_eq!((c.environment, c.color.as_deref()), (None, None));
        let c = c.with_environment(Environment::Prod).with_color("#aabbcc");
        assert_eq!(c.environment, Some(Environment::Prod));
        assert_eq!(c.color.as_deref(), Some("#aabbcc"));
    }

    #[test]
    fn target_secret_detection() {
        let d = DriverId::new("postgres").unwrap();
        let has = |t: &str| ConnectionConfig::new("x", d.clone(), t).target_has_secret();
        assert!(has("postgres://bob:pw@db/x"));
        assert!(has("postgres://db/x?password=1"));
        assert!(has("Server=h;Password=abc"));
        assert!(!has("postgres://db:5432/x?sslmode=require"));
        assert!(!has("app.db"));
        assert!(!has(":memory:"));
    }

    #[test]
    fn debug_shows_new_fields() {
        let d = DriverId::new("postgres").unwrap();
        let c = ConnectionConfig::new("p", d, "postgres://bob:hunter2@db/x")
            .with_environment(Environment::Staging)
            .with_color("#112233");
        let out = format!("{c:?}");
        assert!(out.contains("Staging") && out.contains("#112233"));
        assert!(!out.contains("hunter2") && !out.contains("bob"));
    }

    #[test]
    fn tls_mode_ids() {
        for (m, id) in [
            (TlsMode::Disable, "disable"),
            (TlsMode::Prefer, "prefer"),
            (TlsMode::Require, "require"),
            (TlsMode::VerifyCa, "verify-ca"),
            (TlsMode::VerifyFull, "verify-full"),
        ] {
            assert_eq!(m.as_str(), id);
            assert_eq!(TlsMode::from_id(id), Some(m));
        }
        for bad in ["", "Disable", "verify_ca", "verify", " require"] {
            assert_eq!(TlsMode::from_id(bad), None);
        }
        assert_eq!(TlsMode::default(), TlsMode::Prefer);
    }

    #[test]
    fn tls_config_builders() {
        let t = TlsConfig::new(TlsMode::VerifyFull);
        assert_eq!(t.mode, TlsMode::VerifyFull);
        assert_eq!(
            (&t.ca_cert, &t.client_cert, &t.client_key),
            (&None, &None, &None)
        );
        assert_eq!(TlsConfig::default().mode, TlsMode::Prefer);
        let t = t
            .with_ca_cert("/ca.pem")
            .with_client_cert("/c.pem")
            .with_client_key("/k.pem");
        assert_eq!(t.ca_cert, Some(PathBuf::from("/ca.pem")));
        assert_eq!(t.client_cert, Some(PathBuf::from("/c.pem")));
        assert_eq!(t.client_key, Some(PathBuf::from("/k.pem")));
    }

    #[test]
    fn config_tls_default_and_debug() {
        let d = DriverId::new("postgres").unwrap();
        let c = ConnectionConfig::new("p", d, "postgres://bob:hunter2@db/x");
        assert_eq!(c.tls, None);
        assert!(format!("{c:?}").contains("tls: None"));
        let tls = TlsConfig::new(TlsMode::VerifyCa).with_ca_cert("/ca.pem");
        let c = c.with_tls(tls.clone());
        assert_eq!(c.tls, Some(tls));
        let out = format!("{c:?}");
        assert!(out.contains("VerifyCa") && out.contains("/ca.pem"));
        assert!(!out.contains("hunter2") && !out.contains("bob"));
    }
}
