// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Limits as policy values (ADR-0056). The host reads them; this crate only enforces the manifest and widget ones.

use std::time::Duration;

/// Manifest bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestLimits {
    /// Maximum manifest size in bytes.
    pub max_bytes: usize,
    /// Maximum nesting depth of tables and arrays.
    pub max_depth: usize,
    /// Maximum number of capabilities.
    pub max_capabilities: usize,
    /// Maximum entries per contribution list.
    pub max_list_len: usize,
    /// Maximum name, title and label length in characters.
    pub max_name_chars: usize,
    /// Maximum tooltip length in characters.
    pub max_tooltip_chars: usize,
    /// Maximum description length in bytes.
    pub max_description_bytes: usize,
    /// Maximum size of an AI tool input schema (as JSON) in bytes.
    pub max_schema_bytes: usize,
}

/// Wall-clock deadlines per call kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    /// `lifecycle.init`.
    pub init: Duration,
    /// `render-panel` and `panel-event`.
    pub render: Duration,
    /// `status`.
    pub status: Duration,
    /// `completion.complete`.
    pub completion: Duration,
    /// `run-command`.
    pub command: Duration,
    /// One exporter batch.
    pub export_batch: Duration,
    /// `ai-tools.invoke`.
    pub ai_tool: Duration,
    /// One guest driver call.
    pub driver_call: Duration,
}

/// Bounds of plugin queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLimits {
    /// Row cap when the request has none.
    pub max_rows_default: u32,
    /// The largest row cap a request may ask for.
    pub max_rows_cap: u32,
    /// Timeout when the request has none.
    pub timeout_default: Duration,
    /// The largest timeout a request may ask for.
    pub timeout_cap: Duration,
}

/// Bounds of a widget tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetLimits {
    /// Maximum node count.
    pub max_nodes: usize,
    /// Maximum depth (the root is depth 1).
    pub max_depth: usize,
    /// Maximum bytes per string.
    pub max_string_bytes: usize,
    /// Maximum table rows.
    pub max_table_rows: usize,
    /// Maximum table columns.
    pub max_table_columns: usize,
    /// Maximum select options.
    pub max_select_options: usize,
}

/// All limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Manifest bounds.
    pub manifest: ManifestLimits,
    /// Linear memory per instance in bytes.
    pub memory_bytes: u64,
    /// Linear memory per guest driver instance in bytes.
    pub driver_memory_bytes: u64,
    /// Epoch tick length.
    pub epoch_tick: Duration,
    /// Deadlines.
    pub deadlines: Deadlines,
    /// Query bounds.
    pub query: QueryLimits,
    /// Widget bounds.
    pub widgets: WidgetLimits,
}

impl Limits {
    /// The policy defaults.
    pub const DEFAULT: Limits = Limits {
        manifest: ManifestLimits {
            max_bytes: 64 * 1024,
            max_depth: 16,
            max_capabilities: 32,
            max_list_len: 64,
            max_name_chars: 80,
            max_tooltip_chars: 200,
            max_description_bytes: 1024,
            max_schema_bytes: 16 * 1024,
        },
        memory_bytes: 64 * 1024 * 1024,
        driver_memory_bytes: 256 * 1024 * 1024,
        epoch_tick: Duration::from_millis(10),
        deadlines: Deadlines {
            init: Duration::from_secs(5),
            render: Duration::from_secs(1),
            status: Duration::from_secs(1),
            completion: Duration::from_millis(250),
            command: Duration::from_secs(30),
            export_batch: Duration::from_secs(5),
            ai_tool: Duration::from_secs(30),
            driver_call: Duration::from_secs(300),
        },
        query: QueryLimits {
            max_rows_default: 1000,
            max_rows_cap: 10_000,
            timeout_default: Duration::from_secs(30),
            timeout_cap: Duration::from_secs(300),
        },
        widgets: WidgetLimits {
            max_nodes: 2000,
            max_depth: 32,
            max_string_bytes: 16 * 1024,
            max_table_rows: 1000,
            max_table_columns: 50,
            max_select_options: 64,
        },
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}
