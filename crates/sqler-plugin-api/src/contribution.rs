// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What a plugin contributes, as declared in its manifest.

use sqler_core::ObjectKind;

use crate::id::{ContributionId, EngineId, PluginId, SettingKey, ToolName};

/// A command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// Id, unique among commands.
    pub id: ContributionId,
    /// Title shown in the command palette.
    pub title: String,
}

/// A panel (needs `ui.panel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    /// Id.
    pub id: ContributionId,
    /// Title.
    pub title: String,
}

/// Which side of the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Alignment {
    /// Left side.
    Left,
    /// Right side (the default).
    #[default]
    Right,
}

/// A status bar item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusItem {
    /// Id.
    pub id: ContributionId,
    /// Tooltip.
    pub tooltip: Option<String>,
    /// Side.
    pub alignment: Alignment,
}

/// Where a menu entry appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MenuLocation {
    /// The editor context menu.
    Editor,
    /// The results grid context menu.
    Results,
    /// The object browser context menu.
    ObjectBrowser,
}

/// A menu entry that runs a declared command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// The command id (declared in `[[commands]]`).
    pub command: ContributionId,
    /// Where it appears.
    pub location: MenuLocation,
    /// For the object browser: the kinds it applies to (empty = all).
    pub object_kinds: Vec<ObjectKind>,
}

/// An export format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exporter {
    /// Id, passed to `exporter.start` as the format.
    pub id: ContributionId,
    /// Label.
    pub label: String,
    /// File extension, `[a-z0-9]{1,16}`.
    pub extension: String,
}

/// Completion items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionContribution {
    /// The engines it applies to (empty = all).
    pub drivers: Vec<EngineId>,
}

/// An AI tool.
#[derive(Debug, Clone, PartialEq)]
pub struct AiTool {
    /// Tool name.
    pub name: ToolName,
    /// Description for the model.
    pub description: String,
    /// JSON schema of the arguments.
    pub input_schema: serde_json::Value,
}

/// How a driver is provided.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DriverKind {
    /// The host provides the engine (needs `driver.host:<engine>`).
    Host {
        /// The host engine.
        engine: EngineId,
    },
    /// The guest implements the `driver` interface (needs `driver.guest`).
    Guest,
}

/// A driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverContribution {
    /// Id.
    pub id: ContributionId,
    /// Display name.
    pub name: String,
    /// Host or guest.
    pub kind: DriverKind,
    /// SQL dialect name.
    pub dialect: Option<String>,
}

/// The type and value of a plugin setting.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SettingValue {
    /// Boolean.
    Boolean(bool),
    /// Integer.
    Integer(i64),
    /// Float.
    Float(f64),
    /// Text.
    Text(String),
}

/// A plugin setting.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingSpec {
    /// Key.
    pub key: SettingKey,
    /// Label.
    pub label: String,
    /// Description.
    pub description: Option<String>,
    /// Default value; it also fixes the type.
    pub default: SettingValue,
}

/// Everything a manifest contributes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Contributions {
    /// Commands.
    pub commands: Vec<Command>,
    /// Panels.
    pub panels: Vec<Panel>,
    /// Status items.
    pub status_items: Vec<StatusItem>,
    /// Menu entries.
    pub menus: Vec<MenuEntry>,
    /// Exporters.
    pub exporters: Vec<Exporter>,
    /// Completion, if declared.
    pub completion: Option<CompletionContribution>,
    /// AI tools.
    pub ai_tools: Vec<AiTool>,
    /// Drivers.
    pub drivers: Vec<DriverContribution>,
    /// Settings.
    pub settings: Vec<SettingSpec>,
}

/// The host-wide id of a command: `plugin-<plugin>:<command>` (ADR-0060).
#[must_use]
pub fn command_id(plugin: &PluginId, command: &ContributionId) -> String {
    format!("plugin-{plugin}:{command}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_id_format() {
        let p: PluginId = "my-plugin".parse().unwrap();
        let c: ContributionId = "run".parse().unwrap();
        assert_eq!(command_id(&p, &c), "plugin-my-plugin:run");
    }
}
