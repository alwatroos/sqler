// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Error types of the host: loading a plugin, calling into it, and its settings.

use std::path::PathBuf;
use std::time::Duration;

use sqler_plugin_api::{CompatError, ManifestError};
use thiserror::Error;

/// Why a plugin could not be loaded (discovery, validation, compile or link).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum LoadError {
    /// A file or directory could not be read.
    #[error("cannot read {}: {message}", path.display())]
    Io {
        /// The path.
        path: PathBuf,
        /// The OS error text.
        message: String,
    },
    /// `sqler.toml` is not a valid manifest.
    #[error("invalid manifest: {0}")]
    Manifest(ManifestError),
    /// The plugin needs another API version than the host provides.
    #[error("incompatible plugin: {0}")]
    Incompatible(CompatError),
    /// A scanned directory is not named after the plugin id.
    #[error("directory `{dir_name}` does not match the plugin id")]
    DirName {
        /// The directory name.
        dir_name: String,
    },
    /// Another plugin with the same id was found first.
    #[error("duplicate plugin id; {} was found first", first.display())]
    Duplicate {
        /// The directory of the plugin that won.
        first: PathBuf,
    },
    /// A scanned root has more entries than the host looks at.
    #[error("more than {max} entries; the rest are ignored")]
    TooManyEntries {
        /// The cap.
        max: usize,
    },
    /// The component named by `[entry]` does not exist or is not a file.
    #[error("component {} is missing", path.display())]
    EntryMissing {
        /// The path the manifest names.
        path: PathBuf,
    },
    /// The component resolves outside the plugin directory.
    #[error("component {} is outside the plugin directory", path.display())]
    EntryEscapes {
        /// The path the manifest names.
        path: PathBuf,
    },
    /// The component is larger than the host loads.
    #[error("component is {len} bytes, the limit is {max}")]
    ComponentTooLarge {
        /// Actual size.
        len: u64,
        /// Limit.
        max: u64,
    },
    /// The component changed between discovery and compile.
    #[error("component changed since it was discovered")]
    ComponentChanged,
    /// This build has no compiler (feature `cl` is off).
    #[error("this build cannot compile components (feature `cl` is off)")]
    NoCompiler,
    /// The component does not compile.
    #[error("component does not compile: {message}")]
    Compile {
        /// The engine's message.
        message: String,
    },
    /// The component does not link against this host (unknown import, wrong version).
    #[error("component does not link: {message}")]
    Link {
        /// The engine's message.
        message: String,
    },
}

impl LoadError {
    /// Whether the plugin is the wrong kind for this host (ADR-0055 "same kind"): a different API or an import the
    /// host does not provide. Such a plugin is shown as incompatible rather than broken.
    #[must_use]
    pub fn is_incompatible(&self) -> bool {
        matches!(self, Self::Incompatible(_) | Self::Link { .. })
    }
}

/// Mirrors the WIT enum `types.error-code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The plugin lacks a capability.
    CapabilityDenied,
    /// The user declined.
    Declined,
    /// An argument is invalid.
    InvalidArgument,
    /// Not found.
    NotFound,
    /// A limit was exceeded.
    LimitExceeded,
    /// Timed out.
    Timeout,
    /// Any other failure.
    Failed,
}

/// The error a guest returned (WIT `plugin-error`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code:?}: {message}")]
pub struct GuestError {
    /// The code.
    pub code: ErrorCode,
    /// The guest's message.
    pub message: String,
}

/// Why a call into a plugin failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CallError {
    /// The guest trapped or panicked.
    #[error("plugin trapped: {message}")]
    Trap {
        /// The engine's message.
        message: String,
    },
    /// The guest tried to grow past its memory cap.
    #[error("plugin exceeded its memory limit of {limit} bytes")]
    MemoryLimit {
        /// The cap in bytes.
        limit: u64,
    },
    /// The call ran past its deadline.
    #[error("plugin call timed out after {after:?}")]
    Timeout {
        /// The deadline.
        after: Duration,
    },
    /// The caller cancelled the call.
    #[error("call cancelled")]
    Cancelled,
    /// The guest returned an error.
    #[error("plugin returned an error: {0}")]
    Guest(GuestError),
    /// The component does not export the interface.
    #[error("plugin does not export `{interface}`")]
    MissingExport {
        /// The interface name.
        interface: &'static str,
    },
    /// The plugin is disabled.
    #[error("plugin is disabled")]
    Disabled,
    /// No such plugin.
    #[error("no such plugin")]
    NotFound,
    /// The plugin could not be loaded.
    #[error("plugin could not be loaded: {0}")]
    Load(LoadError),
    /// The host panicked while serving the call.
    #[error("internal host panic")]
    HostPanic,
    /// The engine failed for another reason.
    #[error("engine error: {message}")]
    Engine {
        /// The engine's message.
        message: String,
    },
}

impl CallError {
    /// Whether the error counts toward the three-faults-and-disabled rule: traps, limits, timeouts and host panics.
    /// (A guest error returned by `init` also counts; the caller of `init` knows that and adds it.)
    #[must_use]
    pub fn is_fault(&self) -> bool {
        matches!(
            self,
            Self::Trap { .. } | Self::MemoryLimit { .. } | Self::Timeout { .. } | Self::HostPanic
        )
    }
}

/// Why a setting could not be written.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SettingsError {
    /// The manifest declares no such setting.
    #[error("setting `{key}` is not declared")]
    Undeclared {
        /// The key, truncated.
        key: String,
    },
    /// The value has another type than the declared default.
    #[error("setting `{key}` must be {expected}")]
    TypeMismatch {
        /// The key.
        key: String,
        /// The declared type.
        expected: &'static str,
    },
    /// The store failed.
    #[error("settings store failed: {message}")]
    Store {
        /// The store's message.
        message: String,
    },
}
