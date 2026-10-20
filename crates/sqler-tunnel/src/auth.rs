// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fmt;
use std::path::PathBuf;

/// A secret string (password or key passphrase). `Debug` prints `Secret(***)`; there is no
/// `Display`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Returns the secret text. Use it only to hand it to the SSH library.
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

/// How to authenticate to the SSH server. Secrets are passed in by the caller, never read from
/// configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SshAuth {
    /// Password authentication.
    Password(Secret),
    /// A private key file (`OpenSSH`, PKCS#8 or PEM), with the passphrase if it is encrypted.
    KeyFile {
        /// Path of the private key file.
        path: PathBuf,
        /// Passphrase of an encrypted key.
        passphrase: Option<Secret>,
    },
    /// The SSH agent. `None` selects the platform default: `SSH_AUTH_SOCK` on Unix, the
    /// `OpenSSH` agent pipe on Windows.
    Agent {
        /// Agent socket (Unix) or pipe name (Windows).
        socket: Option<PathBuf>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_secrets() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(s.expose(), "hunter2");

        let pw = format!("{:?}", SshAuth::Password(Secret::new("hunter2")));
        let key = format!(
            "{:?}",
            SshAuth::KeyFile {
                path: PathBuf::from("id_ed25519"),
                passphrase: Some(Secret::new("hunter2")),
            }
        );
        assert!(!pw.contains("hunter2"), "{pw}");
        assert!(!key.contains("hunter2"), "{key}");
        assert!(key.contains("id_ed25519"));
    }
}
