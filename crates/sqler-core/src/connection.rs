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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}
