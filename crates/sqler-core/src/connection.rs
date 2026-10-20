// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fmt;

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
}

impl ConnectionConfig {
    /// Creates a connection config.
    #[must_use]
    pub fn new(name: impl Into<String>, driver: DriverId, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver,
            target: target.into(),
        }
    }
}

impl fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("name", &self.name)
            .field("driver", &self.driver)
            .field("target", &redact_target(&self.target))
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
}
