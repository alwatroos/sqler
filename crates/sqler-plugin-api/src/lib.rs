// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The WIT package `sqler:plugin`, the plugin manifest and the capability model (ADR-0055, ADR-0056).
//!
//! Plain data and validation only: no `wasmtime`, no I/O. The host (`sqler-plugin-host`) generates bindings from
//! the `wit/` directory and reads the policy values from [`Limits`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::module_name_repetitions
)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod api;
mod capability;
mod completion;
mod contribution;
mod fs_scope;
mod grant;
mod hash;
mod host_pattern;
mod id;
mod limits;
pub mod manifest;
mod widget;

pub use api::{API_VERSION, ApiVersion, ApiVersionError, CompatError, WIT_PACKAGE};
pub use capability::{
    Capability, CapabilityDiff, CapabilityError, CapabilityKind, CapabilitySet, Gate,
    HostInterface, gate,
};
pub use completion::{
    CompletionItem, CompletionKind, CompletionRequest, MAX_COMPLETION_DETAIL_BYTES,
    MAX_COMPLETION_INSERT_BYTES, MAX_COMPLETION_ITEMS, MAX_COMPLETION_LABEL_BYTES,
    MAX_COMPLETION_STATEMENT_BYTES,
};
pub use contribution::{
    AiTool, Alignment, Command, CompletionContribution, Contributions, DriverContribution,
    DriverKind, Exporter, MenuEntry, MenuLocation, Panel, SettingSpec, SettingValue, StatusItem,
    command_id,
};
pub use fs_scope::{FsRoot, FsRoots, FsScope, ScopeError};
pub use grant::{GrantKey, GrantKeyBuildError, GrantKeyError, GrantSource, ParsedGrantKey};
pub use hash::{ComponentDigest, HashParseError, ManifestHash};
pub use host_pattern::{HostPattern, HostPatternError};
pub use id::{ContributionId, EngineId, IdError, PluginId, SettingKey, ToolName};
pub use limits::{Deadlines, Limits, ManifestLimits, QueryLimits, WidgetLimits};
pub use manifest::{Entry, Manifest, ManifestError};
pub use widget::{
    Button, ButtonStyle, Checkbox, ColumnRef, CommandContext, Direction, Gap, ObjectPathRef,
    ObjectRef, Select, Stack, Table, Text, TextInput, TextStyle, WIDGET_CASES, Widget, WidgetError,
    WidgetEvent, WidgetNode, WidgetTree,
};
