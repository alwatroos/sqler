// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Secrets (database passwords, SSH passwords and key passphrases) and where they are stored.
//!
//! A [`SecretRef`] names a secret as `<connection-id>:<kind>`. A [`SecretStore`] sets, gets and
//! deletes the [`Secret`] behind a reference: [`KeychainStore`] in the OS keychain,
//! [`MemoryStore`] in memory for tests and headless use.

mod error;
mod keychain;
mod memory;

use std::fmt;
use std::str::FromStr;

pub use error::SecretError;
pub use keychain::KeychainStore;
pub use memory::MemoryStore;

/// The keychain service name every `SQLer` entry is stored under. Changing it orphans stored secrets.
pub const KEYCHAIN_SERVICE: &str = "SQLer";

pub(crate) const MAX_ID_LEN: usize = 64;

/// What a secret is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SecretKind {
    /// The password for the database itself.
    DatabasePassword,
    /// The password for an SSH tunnel login.
    SshPassword,
    /// The passphrase of an SSH private key.
    SshKeyPassphrase,
}

impl SecretKind {
    /// The stable id used in references (`db-password`, `ssh-password`, `ssh-key-passphrase`).
    #[must_use]
    pub fn as_id(self) -> &'static str {
        match self {
            Self::DatabasePassword => "db-password",
            Self::SshPassword => "ssh-password",
            Self::SshKeyPassphrase => "ssh-key-passphrase",
        }
    }

    /// The kind with this id, or `None` for an unknown id.
    #[must_use]
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "db-password" => Some(Self::DatabasePassword),
            "ssh-password" => Some(Self::SshPassword),
            "ssh-key-passphrase" => Some(Self::SshKeyPassphrase),
            _ => None,
        }
    }
}

/// A stable name for one secret: a connection id and the kind of secret.
///
/// Displays as `<connection-id>:<kind-id>`; [`FromStr`] is the exact inverse. The connection id is
/// 1 to 64 characters of `[A-Za-z0-9._-]` and starts with a letter or digit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretRef {
    connection_id: String,
    kind: SecretKind,
}

pub(crate) fn validate_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() {
        return Err("connection id is empty");
    }
    if id.len() > MAX_ID_LEN {
        return Err("connection id is longer than 64 characters");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err("connection id may only contain letters, digits, '.', '_' and '-'");
    }
    if !id.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err("connection id must start with a letter or digit");
    }
    Ok(())
}

impl SecretRef {
    /// A reference to the `kind` secret of connection `connection_id`.
    ///
    /// # Errors
    /// [`SecretError::InvalidRef`] if the id breaks the grammar.
    pub fn new(connection_id: &str, kind: SecretKind) -> Result<Self, SecretError> {
        validate_id(connection_id).map_err(|reason| SecretError::InvalidRef {
            input: connection_id.to_owned(),
            reason,
        })?;
        Ok(Self {
            connection_id: connection_id.to_owned(),
            kind,
        })
    }

    /// The connection id.
    #[must_use]
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// The kind of secret.
    #[must_use]
    pub fn kind(&self) -> SecretKind {
        self.kind
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.connection_id, self.kind.as_id())
    }
}

impl FromStr for SecretRef {
    type Err = SecretError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = |reason| SecretError::InvalidRef {
            input: s.to_owned(),
            reason,
        };
        let (id, kind) = s
            .rsplit_once(':')
            .ok_or_else(|| invalid("expected <connection-id>:<kind>"))?;
        let kind = SecretKind::from_id(kind).ok_or_else(|| invalid("unknown secret kind"))?;
        validate_id(id).map_err(invalid)?;
        Ok(Self {
            connection_id: id.to_owned(),
            kind,
        })
    }
}

/// A secret value. Its `Debug` output never shows the value; there is no `Display` and no serde.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value. Call this only where the secret is actually used.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Where secrets are kept. Calls may block (the OS keychain does), so async callers run them on a
/// blocking thread.
pub trait SecretStore: Send + Sync {
    /// Stores the secret, replacing any existing one.
    ///
    /// # Errors
    /// A [`SecretError`] if the store cannot be used or refuses the entry.
    fn set(&self, r: &SecretRef, secret: &Secret) -> Result<(), SecretError>;

    /// Reads the secret; `None` if there is no entry.
    ///
    /// # Errors
    /// A [`SecretError`] if the store cannot be used or the entry cannot be read.
    fn get(&self, r: &SecretRef) -> Result<Option<Secret>, SecretError>;

    /// Deletes the secret; `false` if there was no entry.
    ///
    /// # Errors
    /// A [`SecretError`] if the store cannot be used.
    fn delete(&self, r: &SecretRef) -> Result<bool, SecretError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [SecretKind; 3] = [
        SecretKind::DatabasePassword,
        SecretKind::SshPassword,
        SecretKind::SshKeyPassphrase,
    ];

    #[test]
    fn ref_round_trips_for_every_kind() {
        for kind in KINDS {
            let r = SecretRef::new("conn-1.a_b", kind).unwrap();
            let text = r.to_string();
            assert_eq!(text, format!("conn-1.a_b:{}", kind.as_id()));
            assert_eq!(text.parse::<SecretRef>().unwrap(), r);
            assert_eq!(SecretKind::from_id(kind.as_id()), Some(kind));
        }
    }

    #[test]
    fn ref_accessors() {
        let r = SecretRef::new("c1", SecretKind::SshPassword).unwrap();
        assert_eq!(r.connection_id(), "c1");
        assert_eq!(r.kind(), SecretKind::SshPassword);
    }

    #[test]
    fn id_of_64_chars_is_accepted() {
        assert!(SecretRef::new(&"a".repeat(64), SecretKind::DatabasePassword).is_ok());
    }

    #[test]
    fn bad_ids_are_rejected() {
        let long = "a".repeat(65);
        for id in ["", long.as_str(), "a b", "a/b", "-a", ".a", "a:b", "é"] {
            assert!(
                matches!(
                    SecretRef::new(id, SecretKind::DatabasePassword),
                    Err(SecretError::InvalidRef { .. })
                ),
                "{id:?} should be rejected"
            );
        }
    }

    #[test]
    fn bad_strings_are_rejected() {
        for s in [
            "",
            "abc",
            "abc:",
            "abc:nope",
            ":db-password",
            "a b:db-password",
            "-a:db-password",
        ] {
            assert!(
                matches!(s.parse::<SecretRef>(), Err(SecretError::InvalidRef { .. })),
                "{s:?} should be rejected"
            );
        }
    }

    #[test]
    fn unknown_kind_id_is_none() {
        assert_eq!(SecretKind::from_id("password"), None);
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::from("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert!(!format!("{s:#?}").contains("hunter2"));
        assert_eq!(s.expose(), "hunter2");
        assert_eq!(Secret::from(String::from("x")).expose(), "x");
    }

    #[test]
    fn error_display_names_ref_without_value() {
        let r = SecretRef::new("c1", SecretKind::DatabasePassword).unwrap();
        let e = SecretError::NotUtf8 { reference: r };
        assert_eq!(
            e.to_string(),
            "stored secret for c1:db-password is not valid UTF-8"
        );
    }
}
