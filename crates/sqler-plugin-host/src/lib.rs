// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin loading, sandboxing, capability grants and lifecycle.
//!
//! Two layers. The engine-free core (default features) discovers plugins, validates manifests, computes the
//! component digest and grant key, resolves `fs.read` scopes and defines the traits the runtime consumes. The
//! `runtime` layer (features `cl` / `pulley-rt`) adds the wasmtime engine; `net` adds the HTTP egress.
//!
//! Grants are enforced **per call**, not at link time: one shared linker links every interface for every plugin, so an
//! import is present whether or not it is granted, and the checks are the capability gates, the empty preopen set and
//! the plugin's `Egress`. `refresh_grants` cancels the calls in flight and drops the instance, so a revoked capability
//! stops working at once instead of at the call's deadline. The network check refuses every non-public address except
//! the exact name `localhost` when a `net.http` pattern names it.

#![forbid(unsafe_code)]

mod dirs;
mod discovery;
mod egress;
mod error;
mod grant;
// Only the runtime layer writes plugin log lines; the module stays compiled so its tests run everywhere.
#[cfg_attr(not(feature = "runtime"), allow(dead_code))]
mod log;
mod scope;
mod settings;
mod status;

#[cfg(feature = "runtime")]
mod catalog;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "runtime")]
mod query;
#[cfg(feature = "runtime")]
mod runtime;

pub use dirs::PluginDirs;
pub use discovery::{
    ComponentFile, DiscoveredPlugin, Discovery, LoadProblem, MANIFEST_FILE, MAX_COMPONENT_BYTES,
    MAX_ENTRIES_PER_ROOT, PluginSource, discover,
};
pub use egress::{
    DenyReason, EGRESS_MAX_BYTES_GLOBAL, EGRESS_MAX_BYTES_PER_PLUGIN, EGRESS_MAX_REQUESTS_GLOBAL,
    EGRESS_MAX_REQUESTS_PER_PLUGIN, Egress, EgressError, EgressFactory, EgressFuture,
    EgressRequest, EgressResponse, MAX_REQUEST_BODY, MAX_RESPONSE_BODY, NoEgress,
};
pub use error::{CallError, ErrorCode, GuestError, LoadError, SettingsError};
pub use grant::{DenyAll, GrantSource, effective};
pub use log::LogLevel;
pub use scope::{Preopen, ScopeIssue, resolve_preopens};
pub use settings::{MemorySettings, SettingsStore, read_setting, write_setting};
pub use status::{CallKind, DisableReason, PluginState, PluginStatus};

#[cfg(feature = "runtime")]
pub use query::{
    ApprovalStage, DataError, DataService, Decision, MAX_CATALOG_ITEMS, MAX_PARAMS,
    MAX_RESULT_BYTES, MAX_SQL_BYTES, PluginConnection, PluginData, QueryApprover, WriteReason,
    WriteRequest,
};
#[cfg(feature = "runtime")]
pub use runtime::{HostOptions, PendingCall, PluginHost, Services};
