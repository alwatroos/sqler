// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The per-store resource limiter: linear memory cap, table size and instance counts.

use wasmtime::ResourceLimiter;

/// Table elements a guest may grow to.
pub(crate) const MAX_TABLE_ELEMENTS: usize = 1_000_000;
const MAX_INSTANCES: usize = 64;
const MAX_TABLES: usize = 64;
const MAX_MEMORIES: usize = 16;

/// Enforces the memory cap of one plugin: the total of every linear memory in the store, and the total of every table
/// element. A request past either sets `hit` and fails with an error (which surfaces as a trap), so the host can tell
/// an out-of-memory fault from any other trap.
#[derive(Debug)]
pub(crate) struct PluginLimiter {
    cap: u64,
    hit: bool,
    /// Bytes of linear memory granted so far, across all memories of the store.
    memory: u64,
    /// Table elements granted so far, across all tables of the store.
    elements: usize,
}

impl PluginLimiter {
    pub(crate) fn new(cap: u64) -> Self {
        Self {
            cap,
            hit: false,
            memory: 0,
            elements: 0,
        }
    }

    /// Whether a growth request was refused for exceeding the cap.
    pub(crate) fn hit(&self) -> bool {
        self.hit
    }

    /// The cap in bytes.
    pub(crate) fn cap(&self) -> u64 {
        self.cap
    }

    fn refuse(&mut self, what: &str) -> wasmtime::Error {
        self.hit = true;
        wasmtime::format_err!("{what} exceeded")
    }
}

impl ResourceLimiter for PluginLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // `current` is already part of the total: only the difference is new.
        let grown = u64::try_from(desired.saturating_sub(current)).unwrap_or(u64::MAX);
        match self.memory.checked_add(grown) {
            Some(total) if total <= self.cap => {
                self.memory = total;
                Ok(true)
            }
            _ => Err(self.refuse(&format!("memory limit of {} bytes", self.cap))),
        }
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let grown = desired.saturating_sub(current);
        match self.elements.checked_add(grown) {
            Some(total) if total <= MAX_TABLE_ELEMENTS => {
                self.elements = total;
                Ok(true)
            }
            _ => Err(self.refuse(&format!("table limit of {MAX_TABLE_ELEMENTS} elements"))),
        }
    }

    fn instances(&self) -> usize {
        MAX_INSTANCES
    }

    fn tables(&self) -> usize {
        MAX_TABLES
    }

    fn memories(&self) -> usize {
        MAX_MEMORIES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_over_the_cap_is_an_error_and_sets_hit() {
        let mut l = PluginLimiter::new(1024);
        assert!(l.memory_growing(0, 1024, None).unwrap());
        assert!(!l.hit());
        assert!(l.memory_growing(1024, 1025, None).is_err());
        assert!(l.hit());
    }

    #[test]
    fn memories_count_together() {
        let mut l = PluginLimiter::new(1024);
        // Two memories that each stay under the cap but not together.
        assert!(l.memory_growing(0, 600, None).unwrap());
        assert!(l.memory_growing(0, 600, None).is_err());
        assert!(l.hit());
    }

    #[test]
    fn growing_a_memory_adds_only_the_difference() {
        let mut l = PluginLimiter::new(1000);
        assert!(l.memory_growing(0, 400, None).unwrap());
        assert!(l.memory_growing(400, 700, None).unwrap());
        assert!(l.memory_growing(0, 300, None).unwrap());
        assert!(l.memory_growing(700, 701, None).is_err());
    }

    #[test]
    fn tables_count_together_and_are_an_error() {
        let mut l = PluginLimiter::new(1);
        assert!(l.table_growing(0, MAX_TABLE_ELEMENTS / 2, None).unwrap());
        assert!(
            l.table_growing(0, MAX_TABLE_ELEMENTS / 2, None).unwrap(),
            "exactly at the cap"
        );
        assert!(!l.hit());
        assert!(l.table_growing(0, 1, None).is_err());
        assert!(l.hit());
        assert_eq!((l.instances(), l.tables(), l.memories()), (64, 64, 16));
    }

    #[test]
    fn one_table_over_the_cap_is_refused() {
        let mut l = PluginLimiter::new(1);
        assert!(l.table_growing(0, MAX_TABLE_ELEMENTS + 1, None).is_err());
    }
}
