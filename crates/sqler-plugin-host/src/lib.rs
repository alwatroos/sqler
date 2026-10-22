// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin loading, sandboxing, capability grants and lifecycle.
//!
//! Two layers. The engine-free core (default features) discovers plugins, validates manifests, computes the
//! component digest and grant key, resolves `fs.read` scopes and defines the traits the runtime consumes. The
//! `runtime` layer (features `cl` / `pulley-rt`) adds the wasmtime engine; `net` adds the HTTP egress.

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

#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "runtime")]
mod runtime;

pub use dirs::PluginDirs;
pub use discovery::{
    ComponentFile, DiscoveredPlugin, Discovery, LoadProblem, MANIFEST_FILE, MAX_COMPONENT_BYTES,
    MAX_ENTRIES_PER_ROOT, PluginSource, discover,
};
pub use egress::{
    DenyReason, Egress, EgressError, EgressFactory, EgressFuture, EgressRequest, EgressResponse,
    MAX_REQUEST_BODY, MAX_RESPONSE_BODY, NoEgress,
};
pub use error::{CallError, ErrorCode, GuestError, LoadError, SettingsError};
pub use grant::{DenyAll, GrantSource, effective};
pub use log::LogLevel;
pub use scope::{Preopen, ScopeIssue, resolve_preopens};
pub use settings::{MemorySettings, SettingsStore, read_setting, write_setting};
pub use status::{CallKind, DisableReason, PluginState, PluginStatus};

#[cfg(feature = "runtime")]
pub use runtime::{HostOptions, PendingCall, PluginHost, Services};
