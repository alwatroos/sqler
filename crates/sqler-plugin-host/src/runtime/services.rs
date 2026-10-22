// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A type map for the host traits later items need (query, catalog, UI sink, ...), so adding one does not change
//! `HostOptions`.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Host services keyed by type. Later items insert their trait objects here and read them from `HostState`.
#[derive(Clone, Default)]
pub struct Services {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl Services {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `value` under its type, replacing an earlier one.
    pub fn insert<T: Any + Send + Sync>(&mut self, value: Arc<T>) {
        self.map.insert(TypeId::of::<T>(), value);
    }

    /// The service of type `T`, if one was inserted.
    #[must_use]
    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|v| Arc::clone(v).downcast::<T>().ok())
    }
}

impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Services")
            .field("len", &self.map.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get_by_type() {
        let mut s = Services::new();
        assert!(s.get::<u32>().is_none());
        s.insert(Arc::new(7_u32));
        s.insert(Arc::new(String::from("x")));
        assert_eq!(s.get::<u32>().as_deref(), Some(&7));
        assert_eq!(s.get::<String>().as_deref().map(String::as_str), Some("x"));
        s.insert(Arc::new(8_u32));
        assert_eq!(s.get::<u32>().as_deref(), Some(&8));
        assert!(s.clone().get::<u32>().is_some());
    }
}
