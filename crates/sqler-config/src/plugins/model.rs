// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The grant records and the in-memory book of them. Keys and capabilities are validated
//! *strings*: the typed parse belongs to the layer above (this crate cannot see the plugin API).

use std::collections::BTreeMap;

use thiserror::Error;

/// Longest accepted grant key, in bytes.
const MAX_KEY_BYTES: usize = 340;
/// Longest accepted capability text, in bytes.
const MAX_CAPABILITY_BYTES: usize = 300;

/// Most records the file may hold (one per plugin id).
pub const MAX_PLUGIN_GRANTS: usize = 1024;
/// Most capabilities one record may list.
pub const MAX_GRANT_CAPABILITIES: usize = 32;

/// What the user decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GrantDecision {
    /// The user approved the listed capabilities.
    Approved,
    /// The user denied the plugin; nothing is granted.
    Denied,
}

/// A grant record did not pass the shape rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum GrantRecordError {
    /// The key is not `<id>@<...>#<...>` in printable ASCII within the size limit.
    #[error("not a valid grant key")]
    BadKey,
    /// The capability at `index` is empty, too long or not printable ASCII without spaces.
    #[error("capability {index} is not valid")]
    BadCapability {
        /// Position in the list given.
        index: usize,
    },
    /// More than [`MAX_GRANT_CAPABILITIES`] capabilities.
    #[error("too many capabilities")]
    TooManyCapabilities,
}

fn printable(text: &str) -> bool {
    text.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn valid_plugin_id(id: &str) -> bool {
    if !(2..=64).contains(&id.len()) {
        return false;
    }
    let mut chars = id.chars().peekable();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return false;
    }
    let mut prev_dash = false;
    for c in chars {
        match c {
            'a'..='z' | '0'..='9' => prev_dash = false,
            '-' if !prev_dash => prev_dash = true,
            _ => return false,
        }
    }
    !prev_dash
}

/// Returns the plugin id when `key` has the shape `<id>@...#...`.
fn key_plugin(key: &str) -> Option<&str> {
    if key.len() > MAX_KEY_BYTES || !printable(key) {
        return None;
    }
    let (id, rest) = key.split_once('@')?;
    (valid_plugin_id(id) && rest.contains('#')).then_some(id)
}

pub(super) fn check_capability(text: &str) -> bool {
    (1..=MAX_CAPABILITY_BYTES).contains(&text.len()) && printable(text)
}

/// The decision stored for one plugin id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRecord {
    key: String,
    plugin: String,
    decision: GrantDecision,
    capabilities: Vec<String>,
}

impl GrantRecord {
    /// An approval of `caps` under `key`. The capabilities are sorted and deduplicated.
    ///
    /// # Errors
    /// [`GrantRecordError`] when the key or a capability breaks the shape rules, or when there
    /// are more than [`MAX_GRANT_CAPABILITIES`] distinct capabilities.
    pub fn approved(key: String, caps: Vec<String>) -> Result<Self, GrantRecordError> {
        let plugin = key_plugin(&key).ok_or(GrantRecordError::BadKey)?.to_owned();
        if let Some(index) = caps.iter().position(|c| !check_capability(c)) {
            return Err(GrantRecordError::BadCapability { index });
        }
        let mut capabilities = caps;
        capabilities.sort();
        capabilities.dedup();
        if capabilities.len() > MAX_GRANT_CAPABILITIES {
            return Err(GrantRecordError::TooManyCapabilities);
        }
        Ok(Self {
            key,
            plugin,
            decision: GrantDecision::Approved,
            capabilities,
        })
    }

    /// A denial under `key`; it grants nothing.
    ///
    /// # Errors
    /// [`GrantRecordError::BadKey`] when the key breaks the shape rules.
    pub fn denied(key: String) -> Result<Self, GrantRecordError> {
        let plugin = key_plugin(&key).ok_or(GrantRecordError::BadKey)?.to_owned();
        Ok(Self {
            key,
            plugin,
            decision: GrantDecision::Denied,
            capabilities: Vec::new(),
        })
    }

    /// The grant key text, verbatim.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The plugin id (the part of the key before `@`).
    #[must_use]
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// What the user decided.
    #[must_use]
    pub fn decision(&self) -> GrantDecision {
        self.decision
    }

    /// The approved capabilities, sorted; always empty for a denial.
    #[must_use]
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }
}

/// All stored decisions, one record per plugin id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginGrants {
    records: BTreeMap<String, GrantRecord>,
}

impl PluginGrants {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The record for a plugin id.
    #[must_use]
    pub fn for_plugin(&self, plugin: &str) -> Option<&GrantRecord> {
        self.records.get(plugin)
    }

    /// Stores `record`, replacing any earlier record of the same plugin.
    pub fn record(&mut self, record: GrantRecord) {
        self.records.insert(record.plugin.clone(), record);
    }

    /// Turns an approval of `plugin` into a denial under the same key. Returns `false` when
    /// there was no approval.
    pub fn revoke(&mut self, plugin: &str) -> bool {
        match self.records.get_mut(plugin) {
            Some(r) if r.decision == GrantDecision::Approved => {
                r.decision = GrantDecision::Denied;
                r.capabilities.clear();
                true
            }
            _ => false,
        }
    }

    /// The records in plugin-id order.
    pub fn iter(&self) -> impl Iterator<Item = &GrantRecord> {
        self.records.values()
    }

    /// The number of records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether there are no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}
