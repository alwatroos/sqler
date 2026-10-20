// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Settings, keybindings and the saved-connection list, stored as TOML files in
//! the platform configuration directory.
//!
//! The query history (`history.jsonl`) and saved queries (`queries.toml`) live next to them.
//!
//! Secrets (passwords, passphrases) live in the OS keychain through [`SecretStore`]; never in these files.

#![forbid(unsafe_code)]

mod connections;
mod error;
mod fs;
mod history;
mod keymap;
mod paths;
mod saved_queries;
mod secrets;
mod settings;

pub use connections::{
    CONNECTIONS_FORMAT_VERSION, ConnectionId, ConnectionProfile, ConnectionSpec, ConnectionStore,
    Group, GroupId, ProfileSecret, ProfileSecretKind, SshAuth, SshProfile, StoreError, TlsMode,
    TlsProfile,
};
pub use error::ConfigError;
pub use history::{
    DEFAULT_HISTORY_CAP, HISTORY_FORMAT_VERSION, HistoryEntry, HistoryIssue, HistoryIssueKind,
    HistoryOptions, HistoryOutcome, HistoryQuery, HistoryStore, MAX_HISTORY_SQL_BYTES,
    NewHistoryEntry, redact_credentials,
};
pub use keymap::{CommandId, Conflict, KeyChord, KeybindingOverrides, Keymap, Modifiers, Platform};
pub use paths::ConfigDir;
pub use saved_queries::{
    SAVED_QUERIES_FORMAT_VERSION, SavedQueries, SavedQuery, SavedQueryDraft, SavedQueryError,
    SavedQueryId,
};
pub use secrets::{
    KEYCHAIN_SERVICE, KeychainStore, MemoryStore, Secret, SecretError, SecretKind, SecretRef,
    SecretStore,
};
pub use settings::{FONT_SIZE_RANGE, Settings, ThemePreference};
