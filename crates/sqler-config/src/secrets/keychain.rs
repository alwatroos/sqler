// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The OS keychain as a [`SecretStore`]: macOS Keychain, Windows Credential Manager and the Linux
//! Secret Service, through `keyring`.

#[cfg(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "freebsd"
))]
use super::KEYCHAIN_SERVICE;
use super::{Secret, SecretError, SecretRef, SecretStore};

/// Stores secrets in the OS keychain under the service [`KEYCHAIN_SERVICE`], with the reference
/// as the account name. It never falls back to a file or to memory: if the keychain cannot be
/// used, the call fails with a [`SecretError`].
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainStore;

impl KeychainStore {
    /// A handle to the OS keychain. Nothing is opened until the first call.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Maps a `keyring` error to ours. The reason is the inner error's `Display`; byte payloads are
/// dropped so a value can never reach a message.
#[cfg(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "freebsd"
))]
fn map_error(reference: &SecretRef, e: &keyring::Error) -> SecretError {
    use keyring::Error as E;
    let reference = reference.clone();
    match e {
        // No store could be set up: no Secret Service on the bus, for example.
        E::NoStorageAccess(_) | E::NoDefaultStore => SecretError::Unavailable {
            reference,
            reason: e.to_string(),
        },
        E::TooLong(..) | E::Invalid(..) => SecretError::Rejected {
            reference,
            reason: e.to_string(),
        },
        E::BadEncoding(_) => SecretError::NotUtf8 { reference },
        E::Ambiguous(_) => SecretError::Ambiguous { reference },
        // `PlatformFailure` and every variant added later.
        _ => SecretError::Backend {
            reference,
            reason: e.to_string(),
        },
    }
}

#[cfg(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "freebsd"
))]
fn entry(r: &SecretRef) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &r.to_string()).map_err(|e| map_error(r, &e))
}

#[cfg(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "freebsd"
))]
impl SecretStore for KeychainStore {
    fn set(&self, r: &SecretRef, secret: &Secret) -> Result<(), SecretError> {
        entry(r)?
            .set_password(secret.expose())
            .map_err(|e| map_error(r, &e))
    }

    fn get(&self, r: &SecretRef) -> Result<Option<Secret>, SecretError> {
        match entry(r)?.get_password() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(r, &e)),
        }
    }

    fn delete(&self, r: &SecretRef) -> Result<bool, SecretError> {
        match entry(r)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(map_error(r, &e)),
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "freebsd"
)))]
fn unsupported(r: &SecretRef) -> SecretError {
    SecretError::Unavailable {
        reference: r.clone(),
        reason: "no supported OS keychain on this platform".to_owned(),
    }
}

#[cfg(not(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "freebsd"
)))]
impl SecretStore for KeychainStore {
    fn set(&self, r: &SecretRef, _secret: &Secret) -> Result<(), SecretError> {
        Err(unsupported(r))
    }

    fn get(&self, r: &SecretRef) -> Result<Option<Secret>, SecretError> {
        Err(unsupported(r))
    }

    fn delete(&self, r: &SecretRef) -> Result<bool, SecretError> {
        Err(unsupported(r))
    }
}

#[cfg(all(
    test,
    any(
        target_os = "macos",
        windows,
        target_os = "linux",
        target_os = "freebsd"
    )
))]
mod tests {
    use super::*;
    use crate::secrets::SecretKind;

    #[allow(clippy::unwrap_used)]
    fn r() -> SecretRef {
        SecretRef::new("c1", SecretKind::DatabasePassword).unwrap()
    }

    fn platform() -> Box<dyn std::error::Error + Send + Sync> {
        "the backend said no".into()
    }

    fn assert_clean(e: &SecretError) {
        assert!(!e.to_string().contains("hunter2"), "{e}");
        assert!(!format!("{e:?}").contains("hunter2"), "{e:?}");
        assert!(e.to_string().contains("c1:db-password"), "{e}");
    }

    #[test]
    fn no_storage_access_is_unavailable() {
        let e = map_error(&r(), &keyring::Error::NoStorageAccess(platform()));
        assert!(matches!(e, SecretError::Unavailable { .. }));
        assert_clean(&e);
    }

    #[test]
    fn no_default_store_is_unavailable() {
        let e = map_error(&r(), &keyring::Error::NoDefaultStore);
        assert!(matches!(e, SecretError::Unavailable { .. }));
    }

    #[test]
    fn platform_failure_is_backend() {
        let e = map_error(&r(), &keyring::Error::PlatformFailure(platform()));
        assert!(matches!(e, SecretError::Backend { .. }));
        assert_clean(&e);
    }

    #[test]
    fn unlisted_variant_is_backend() {
        let e = map_error(&r(), &keyring::Error::BadStoreFormat("odd".to_owned()));
        assert!(matches!(e, SecretError::Backend { .. }));
    }

    #[test]
    fn too_long_and_invalid_are_rejected() {
        let e = map_error(&r(), &keyring::Error::TooLong("user".to_owned(), 10));
        assert!(matches!(e, SecretError::Rejected { .. }));
        assert_clean(&e);
        let e = map_error(
            &r(),
            &keyring::Error::Invalid("user".to_owned(), "bad".to_owned()),
        );
        assert!(matches!(e, SecretError::Rejected { .. }));
        assert_clean(&e);
    }

    #[test]
    fn bad_encoding_drops_the_bytes() {
        let e = map_error(&r(), &keyring::Error::BadEncoding(b"hunter2".to_vec()));
        assert!(matches!(e, SecretError::NotUtf8 { .. }));
        assert_clean(&e);
    }

    #[test]
    fn ambiguous_is_ambiguous() {
        let e = map_error(&r(), &keyring::Error::Ambiguous(Vec::new()));
        assert!(matches!(e, SecretError::Ambiguous { .. }));
        assert_clean(&e);
    }

    /// Manual check on a desktop session; CI has no keychain.
    /// Run: `cargo test -p sqler-config -- --ignored keychain_round_trip`
    #[test]
    #[ignore = "needs a real, unlocked OS keychain"]
    fn keychain_round_trip() {
        let id = format!("test-{}-{}", std::process::id(), 7);
        let k = SecretRef::new(&id, SecretKind::DatabasePassword).unwrap();
        let store = KeychainStore::new();
        store.set(&k, &Secret::from("sqler-test-secret")).unwrap();
        let got = store.get(&k).unwrap().unwrap();
        assert_eq!(got.expose(), "sqler-test-secret");
        assert!(store.delete(&k).unwrap());
        assert!(store.get(&k).unwrap().is_none());
        assert!(!store.delete(&k).unwrap());
    }
}
