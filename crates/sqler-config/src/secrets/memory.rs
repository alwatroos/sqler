// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! An in-memory [`SecretStore`] for tests, the CLI and headless use.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{Secret, SecretError, SecretRef, SecretStore};

/// Keeps secrets in memory only; they are gone when the store is dropped.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<SecretRef, String>>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SecretRef, String>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryStore")
            .field("len", &self.lock().len())
            .finish()
    }
}

impl SecretStore for MemoryStore {
    fn set(&self, r: &SecretRef, secret: &Secret) -> Result<(), SecretError> {
        self.lock().insert(r.clone(), secret.expose().to_owned());
        Ok(())
    }

    fn get(&self, r: &SecretRef) -> Result<Option<Secret>, SecretError> {
        Ok(self.lock().get(r).map(|v| Secret::new(v.as_str())))
    }

    fn delete(&self, r: &SecretRef) -> Result<bool, SecretError> {
        Ok(self.lock().remove(r).is_some())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;
    use crate::secrets::SecretKind;

    fn r(id: &str, kind: SecretKind) -> SecretRef {
        SecretRef::new(id, kind).unwrap()
    }

    #[test]
    fn set_then_get() {
        let s = MemoryStore::new();
        let k = r("c1", SecretKind::DatabasePassword);
        s.set(&k, &Secret::from("hunter2")).unwrap();
        assert_eq!(s.get(&k).unwrap().unwrap().expose(), "hunter2");
    }

    #[test]
    fn overwrite_replaces() {
        let s = MemoryStore::new();
        let k = r("c1", SecretKind::DatabasePassword);
        s.set(&k, &Secret::from("one")).unwrap();
        s.set(&k, &Secret::from("two")).unwrap();
        assert_eq!(s.get(&k).unwrap().unwrap().expose(), "two");
    }

    #[test]
    fn missing_is_none() {
        let s = MemoryStore::new();
        assert!(
            s.get(&r("c1", SecretKind::DatabasePassword))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn delete_reports_presence() {
        let s = MemoryStore::new();
        let k = r("c1", SecretKind::SshPassword);
        s.set(&k, &Secret::from("x")).unwrap();
        assert!(s.delete(&k).unwrap());
        assert!(!s.delete(&k).unwrap());
        assert!(s.get(&k).unwrap().is_none());
    }

    #[test]
    fn kinds_are_kept_apart() {
        let s = MemoryStore::new();
        let a = r("c1", SecretKind::DatabasePassword);
        let b = r("c1", SecretKind::SshKeyPassphrase);
        s.set(&a, &Secret::from("a")).unwrap();
        s.set(&b, &Secret::from("b")).unwrap();
        assert_eq!(s.get(&a).unwrap().unwrap().expose(), "a");
        assert_eq!(s.get(&b).unwrap().unwrap().expose(), "b");
        assert!(s.delete(&a).unwrap());
        assert_eq!(s.get(&b).unwrap().unwrap().expose(), "b");
    }

    #[test]
    fn works_through_dyn_and_across_threads() {
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        let k = r("c1", SecretKind::DatabasePassword);
        let writer = {
            let (store, k) = (Arc::clone(&store), k.clone());
            thread::spawn(move || store.set(&k, &Secret::from("shared")).unwrap())
        };
        writer.join().unwrap();
        let reader = {
            let (store, k) = (Arc::clone(&store), k.clone());
            thread::spawn(move || store.get(&k).unwrap().map(|s| s.expose().to_owned()))
        };
        assert_eq!(reader.join().unwrap().as_deref(), Some("shared"));
        let by_ref: &dyn SecretStore = &*store;
        assert!(by_ref.delete(&k).unwrap());
    }

    #[test]
    fn debug_shows_only_the_count() {
        let s = MemoryStore::new();
        s.set(
            &r("c1", SecretKind::DatabasePassword),
            &Secret::from("hunter2"),
        )
        .unwrap();
        let text = format!("{s:?}");
        assert!(!text.contains("hunter2"));
        assert!(text.contains("len: 1"));
    }

    #[test]
    fn poisoned_lock_is_recovered() {
        let s = Arc::new(MemoryStore::new());
        let s2 = Arc::clone(&s);
        let _ = thread::spawn(move || {
            let _guard = s2.entries.lock().unwrap();
            panic!("poison");
        })
        .join();
        let k = r("c1", SecretKind::DatabasePassword);
        s.set(&k, &Secret::from("x")).unwrap();
        assert!(s.get(&k).unwrap().is_some());
    }
}
