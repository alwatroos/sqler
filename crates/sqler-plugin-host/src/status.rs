// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Call kinds, plugin states and the status the app shows.

use std::time::Duration;

use sqler_plugin_api::{CapabilitySet, Deadlines, GrantKey, PluginId};

use crate::error::{CallError, LoadError};

/// What a call into a plugin is for; it picks the wall-clock deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CallKind {
    /// `lifecycle.init`.
    Init,
    /// Rendering a panel.
    Render,
    /// Producing a status item.
    Status,
    /// A completion request.
    Completion,
    /// Running a command or handling a widget event.
    Command,
    /// One export batch.
    ExportBatch,
    /// An AI tool call.
    AiTool,
    /// A guest driver call.
    DriverCall,
}

impl CallKind {
    /// The deadline for this kind.
    #[must_use]
    pub fn deadline(self, d: &Deadlines) -> Duration {
        match self {
            Self::Init => d.init,
            Self::Render => d.render,
            Self::Status => d.status,
            Self::Completion => d.completion,
            Self::Command => d.command,
            Self::ExportBatch => d.export_batch,
            Self::AiTool => d.ai_tool,
            Self::DriverCall => d.driver_call,
        }
    }
}

/// Why a plugin is disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DisableReason {
    /// Too many consecutive faults.
    Faults {
        /// How many in a row.
        count: u32,
        /// The last one.
        last: CallError,
    },
    /// It could not be loaded.
    Load(LoadError),
    /// The user turned it off.
    User,
}

/// Lifecycle state of a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginState {
    /// Registered, no instance.
    Idle,
    /// Instantiated.
    Live,
    /// Not running.
    Disabled {
        /// Why.
        reason: DisableReason,
    },
}

/// One row of the plugin list.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginStatus {
    /// Plugin id.
    pub id: PluginId,
    /// State.
    pub state: PluginState,
    /// Declared and approved.
    pub effective: CapabilitySet,
    /// The grant key of this build.
    pub key: GrantKey,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_plugin_api::Limits;

    #[test]
    fn deadlines_follow_the_limits() {
        let d = Limits::DEFAULT.deadlines;
        assert_eq!(CallKind::Init.deadline(&d), d.init);
        assert_eq!(CallKind::Render.deadline(&d), d.render);
        assert_eq!(CallKind::Status.deadline(&d), d.status);
        assert_eq!(CallKind::Completion.deadline(&d), d.completion);
        assert_eq!(CallKind::Command.deadline(&d), d.command);
        assert_eq!(CallKind::ExportBatch.deadline(&d), d.export_batch);
        assert_eq!(CallKind::AiTool.deadline(&d), d.ai_tool);
        assert_eq!(CallKind::DriverCall.deadline(&d), d.driver_call);
    }
}
