// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The data the three import stages pass on: report, plan and apply report.

use sqler_config::{ConnectionId, ConnectionSpec, ProfileSecretKind, Secret};
use sqler_core::DriverId;

/// What a [`Source`](crate::Source) found in one foreign file.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceReport {
    /// The id of the source that made the report.
    pub source: &'static str,
    /// One entry per foreign connection, in file order.
    pub entries: Vec<SourceEntry>,
    /// File-level remarks, for example unknown top-level keys.
    pub notices: Vec<String>,
}

/// One foreign connection.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceEntry {
    /// Position in the report, from 0.
    pub index: usize,
    /// The connection name as the foreign tool shows it.
    pub name: String,
    /// The group path, outermost first; empty at the root.
    pub group: Vec<String>,
    /// The mapped connection or why it was skipped.
    pub outcome: EntryOutcome,
    /// Names (never values) of foreign fields that `SQLer` does not map.
    pub unsupported: Vec<String>,
    /// Remarks about values that were changed or dropped.
    pub notes: Vec<String>,
    /// The secrets the user may enter for this connection. Their values are never read from the file.
    pub secrets: Vec<ProfileSecretKind>,
}

/// The result of mapping one foreign connection.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // the public contract holds the spec by value
pub enum EntryOutcome {
    /// The connection maps to this spec.
    Mapped(ConnectionSpec),
    /// The connection is not imported.
    Skipped(SkipReason),
}

/// Why a connection is not imported.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// A required field is missing or has an unusable value.
    Invalid(String),
    /// The connection uses a feature `SQLer` does not support.
    Unsupported(String),
    /// The engine is not part of this release.
    NotInThisRelease {
        /// The engine name, for example `DuckDB`.
        engine: &'static str,
    },
    /// The driver is not in this build.
    DriverUnavailable {
        /// The driver the connection needs.
        driver: DriverId,
    },
    /// A saved connection already has the same driver, target and SSH tunnel.
    Duplicate {
        /// The saved connection.
        existing: ConnectionId,
    },
    /// An earlier entry of the same file has the same driver, target and SSH tunnel.
    DuplicateInFile {
        /// The index of the first such entry.
        first_index: usize,
    },
}

impl SkipReason {
    /// A short text for the preview. It never holds file content beyond a field name.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Invalid(m) | Self::Unsupported(m) => m.clone(),
            Self::NotInThisRelease { engine } => format!("{engine} is not in SQLer 1.0"),
            Self::DriverUnavailable { .. } => "not available in this build".to_owned(),
            Self::Duplicate { existing } => {
                format!("already saved as connection {existing}")
            }
            Self::DuplicateInFile { first_index } => {
                format!("same server as entry {} of this file", first_index + 1)
            }
        }
    }
}

/// What the caller offers to the planner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportOptions {
    /// The drivers this build has. Filled by the caller from its driver registry.
    pub available_drivers: Vec<DriverId>,
}

/// The dry run: what [`apply`](crate::apply()) would do.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPlan {
    /// The id of the source.
    pub source: &'static str,
    /// One item per report entry. The caller may drop items before applying.
    pub items: Vec<PlanItem>,
    /// File-level remarks of the report.
    pub notices: Vec<String>,
}

/// One planned connection.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    /// The entry index in the report.
    pub index: usize,
    /// The name in the foreign tool.
    pub foreign_name: String,
    /// The group path to create or reuse; empty at the root.
    pub group: Vec<String>,
    /// What happens to the item.
    pub action: PlanAction,
    /// Foreign field names that are not mapped.
    pub unsupported: Vec<String>,
    /// Remarks about changed or dropped values.
    pub notes: Vec<String>,
    /// The secrets the user may enter.
    pub secrets: Vec<ProfileSecretKind>,
}

/// What happens to a planned item.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // the public contract holds the spec by value
pub enum PlanAction {
    /// Add a new connection.
    Add {
        /// The connection; its name is unique in the store and the plan.
        spec: ConnectionSpec,
        /// The original name when it was changed to avoid a collision.
        renamed_from: Option<String>,
    },
    /// Do nothing.
    Skip(SkipReason),
}

/// A secret the user typed in for one planned item.
#[derive(Debug, Clone)]
pub struct ProvidedSecret {
    /// The plan item index.
    pub index: usize,
    /// Which secret it is.
    pub kind: ProfileSecretKind,
    /// The value; its `Debug` output is redacted.
    pub secret: Secret,
}

/// What [`apply`](crate::apply()) did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyReport {
    /// The added connections: plan item index and new id.
    pub added: Vec<(usize, ConnectionId)>,
    /// Items that could not be added, with a text that holds no secret.
    pub failed: Vec<(usize, String)>,
    /// Secrets written to the secret store.
    pub secrets_stored: Vec<(usize, ProfileSecretKind)>,
    /// Secrets of added connections that were not stored: not provided, or the store refused them.
    pub secrets_skipped: Vec<(usize, ProfileSecretKind)>,
}
