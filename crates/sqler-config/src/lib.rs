// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Settings, keybindings and the saved-connection list, stored as TOML files in
//! the platform configuration directory.
//!
//! The query history (`history.jsonl`), saved queries (`queries.toml`) and the AI settings
//! (`ai.toml`) and the plugin permission grants (`plugin-grants.toml`) and preferences (`plugin-prefs.toml`) and the update-check state (`update-state.toml`) live next to them.
//!
//! `session.json` (the open editor tabs, the active tab and the unsent AI chat draft) lives there too. It holds SQL text, so it is user-only (mode `0600` on Unix) and stores connection ids, never names, URLs or secrets.
//!
//! Secrets (passwords, passphrases, AI provider API keys) live in the OS keychain through [`SecretStore`]; never in these files.

#![forbid(unsafe_code)]

mod ai;
mod connections;
mod error;
mod fs;
mod history;
mod keymap;
mod paths;
mod plugins;
mod saved_queries;
mod secrets;
mod session;
mod settings;
mod update_state;
mod workbench;

pub use ai::{
    AI_CONFIG_FORMAT_VERSION, AI_MAX_ROWS_RANGE, AI_TIMEOUT_SECS_RANGE, AiConfig, AiConfigError,
    AiConfigProblem, AiLimits, DEFAULT_AI_MAX_ROWS, DEFAULT_AI_TIMEOUT_SECS,
    DEFAULT_REDACTED_COLUMNS, ModelAssignments, ModelChoice, ProviderConfig, ProviderId,
    ProviderKind, QueryCaps, RedactionList, UseCase,
};
pub use connections::{
    CONNECTIONS_FORMAT_VERSION, ConnectionAi, ConnectionId, ConnectionProfile, ConnectionSpec,
    ConnectionStore, Group, GroupId, ProfileSecret, ProfileSecretKind, SshAuth, SshProfile,
    StoreError, TlsMode, TlsProfile,
};
pub use error::ConfigError;
pub use history::{
    DEFAULT_HISTORY_CAP, HISTORY_FORMAT_VERSION, HistoryEntry, HistoryIssue, HistoryIssueKind,
    HistoryOptions, HistoryOutcome, HistoryQuery, HistoryStore, MAX_HISTORY_SQL_BYTES,
    NewHistoryEntry, contains_redaction, redact_credentials,
};
pub use keymap::{CommandId, Conflict, KeyChord, KeybindingOverrides, Keymap, Modifiers, Platform};
pub use paths::ConfigDir;
pub use plugins::{
    GrantDecision, GrantRecord, GrantRecordError, GrantsLoad, MAX_GRANT_CAPABILITIES,
    MAX_PLUGIN_GRANTS, MAX_PREF_PLUGINS, MAX_PREF_SETTINGS, MAX_PREF_TEXT_BYTES,
    PLUGIN_GRANTS_FORMAT_VERSION, PLUGIN_PREFS_FORMAT_VERSION, PluginGrants, PluginPrefs,
    PrefValue, PrefsLoad,
};
pub use saved_queries::{
    SAVED_QUERIES_FORMAT_VERSION, SavedQueries, SavedQuery, SavedQueryDraft, SavedQueryError,
    SavedQueryId,
};
pub use secrets::{
    KEYCHAIN_SERVICE, KeychainStore, MemoryStore, ProviderKeyRef, Secret, SecretError, SecretKind,
    SecretRef, SecretStore,
};
pub use session::{
    MAX_CHAT_DRAFT_BYTES, MAX_SESSION_BYTES, MAX_SESSION_TAB_BYTES, MAX_SESSION_TABS,
    SESSION_VERSION, SessionLoad, SessionState, SessionTab, fit_session,
};
pub use settings::{
    DEFAULT_RESULT_PAGE_ROWS, FONT_SIZE_RANGE, RESULT_PAGE_ROWS_RANGE, Settings, ThemePreference,
};
pub use update_state::{MAX_DISMISSED_VERSION_LEN, UpdateState};
pub use workbench::{
    BOTTOM_PANEL_SIZE_RANGE, BOTTOM_VIEWS, CHAT_PANEL_SIZE_RANGE, CHAT_VIEWS, MAX_RECENT_COMMANDS,
    MAX_RECENT_CONNECTIONS, PanelState, SIDE_PANEL_SIZE_RANGE, SIDE_VIEWS, WINDOW_SIZE_RANGE,
    WORKBENCH_VERSION, WindowState, WorkbenchLoad, WorkbenchState,
};
