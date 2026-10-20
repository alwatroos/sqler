// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A copy-on-write hash map split into shards.
//!
//! Cloning the map clones one `Arc`. A mutation clones the shard vector (64 pointers) and the one
//! shard it touches, so a snapshot that still holds the old map keeps reading unchanged data
//! while a writer builds the next one. Everything else stays shared.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Number of shards.
pub(crate) const SHARDS: usize = 64;

/// A copy-on-write map of `SHARDS` shards.
#[derive(Debug, Clone)]
pub(crate) struct ShardedMap<K, V> {
    shards: Arc<Vec<Arc<HashMap<K, V>>>>,
}

impl<K, V> Default for ShardedMap<K, V> {
    fn default() -> Self {
        Self {
            shards: Arc::new((0..SHARDS).map(|_| Arc::new(HashMap::new())).collect()),
        }
    }
}

/// The shard a key lives in. `DefaultHasher::new` uses fixed keys, so this is stable.
fn shard_of<K: Hash>(key: &K) -> usize {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    // The remainder is below `SHARDS`, so the conversion cannot fail.
    usize::try_from(hasher.finish() % SHARDS as u64).unwrap_or(0)
}

impl<K: Eq + Hash + Clone, V: Clone> ShardedMap<K, V> {
    /// The value for `key`.
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.shards.get(shard_of(key))?.get(key)
    }

    /// Whether `key` is present.
    pub(crate) fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// Inserts a value and returns the previous one. Clones only the touched shard.
    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let idx = shard_of(&key);
        let shards = Arc::make_mut(&mut self.shards);
        Arc::make_mut(shards.get_mut(idx)?).insert(key, value)
    }

    /// Removes a key and returns its value. Clones a shard only if the key is present.
    pub(crate) fn remove(&mut self, key: &K) -> Option<V> {
        if !self.contains_key(key) {
            return None;
        }
        let idx = shard_of(key);
        let shards = Arc::make_mut(&mut self.shards);
        Arc::make_mut(shards.get_mut(idx)?).remove(key)
    }

    /// Keeps the entries for which `keep` returns `true`.
    ///
    /// `keep` runs once per entry. A shard is cloned only if something is removed from it.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        let doomed: Vec<(usize, Vec<K>)> = self
            .shards
            .iter()
            .enumerate()
            .filter_map(|(idx, shard)| {
                let keys: Vec<K> = shard
                    .iter()
                    .filter(|(k, v)| !keep(k, v))
                    .map(|(k, _)| k.clone())
                    .collect();
                (!keys.is_empty()).then_some((idx, keys))
            })
            .collect();
        if doomed.is_empty() {
            return;
        }
        let shards = Arc::make_mut(&mut self.shards);
        for (idx, keys) in doomed {
            if let Some(shard) = shards.get_mut(idx) {
                let shard = Arc::make_mut(shard);
                for key in &keys {
                    shard.remove(key);
                }
            }
        }
    }

    /// Number of entries.
    pub(crate) fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    /// Whether the map has no entries.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.is_empty())
    }

    /// Every entry, in no particular order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.shards.iter().flat_map(|s| s.iter())
    }

    /// Every key, in no particular order.
    pub(crate) fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(n: u32) -> ShardedMap<u32, String> {
        let mut map = ShardedMap::default();
        for i in 0..n {
            map.insert(i, format!("v{i}"));
        }
        map
    }

    #[test]
    fn insert_get_remove() {
        let mut map = ShardedMap::default();
        assert!(map.is_empty());
        assert_eq!(map.insert(1_u32, "a".to_owned()), None);
        assert_eq!(map.insert(1, "b".to_owned()), Some("a".to_owned()));
        assert_eq!(map.get(&1).map(String::as_str), Some("b"));
        assert!(map.contains_key(&1));
        assert_eq!(map.len(), 1);
        assert_eq!(map.remove(&2), None);
        assert_eq!(map.remove(&1), Some("b".to_owned()));
        assert!(map.is_empty());
    }

    #[test]
    fn clone_shares_and_mutation_does_not_leak() {
        let original = filled(500);
        let mut copy = original.clone();
        assert!(Arc::ptr_eq(&original.shards, &copy.shards));

        copy.insert(10_000, "new".to_owned());
        copy.remove(&3);
        assert_eq!(original.len(), 500);
        assert!(original.contains_key(&3));
        assert!(!original.contains_key(&10_000));
        assert_eq!(copy.len(), 500);
        assert!(!copy.contains_key(&3));

        let shared = original
            .shards
            .iter()
            .zip(copy.shards.iter())
            .filter(|(a, b)| Arc::ptr_eq(a, b))
            .count();
        assert!(shared >= SHARDS - 2, "only touched shards are cloned");
    }

    #[test]
    fn retain_without_removals_keeps_shards() {
        let original = filled(300);
        let mut copy = original.clone();
        copy.retain(|_, _| true);
        assert!(Arc::ptr_eq(&original.shards, &copy.shards));
        assert!(
            original
                .shards
                .iter()
                .zip(copy.shards.iter())
                .all(|(a, b)| Arc::ptr_eq(a, b))
        );
    }

    #[test]
    fn retain_removes_and_clones_only_touched_shards() {
        let original = filled(300);
        let mut copy = original.clone();
        copy.retain(|k, _| *k != 7);
        assert_eq!(copy.len(), 299);
        assert!(!copy.contains_key(&7));
        assert!(original.contains_key(&7));
        let cloned = original
            .shards
            .iter()
            .zip(copy.shards.iter())
            .filter(|(a, b)| !Arc::ptr_eq(a, b))
            .count();
        assert_eq!(cloned, 1);
    }

    #[test]
    fn iteration_covers_all() {
        let map = filled(1000);
        assert_eq!(map.len(), 1000);
        assert_eq!(map.iter().count(), 1000);
        let mut keys: Vec<u32> = map.keys().copied().collect();
        keys.sort_unstable();
        assert_eq!(keys, (0..1000).collect::<Vec<_>>());
    }

    #[test]
    fn keys_spread_over_shards() {
        let map = filled(1000);
        let used = map.shards.iter().filter(|s| !s.is_empty()).count();
        assert!(used > SHARDS / 2);
    }
}
