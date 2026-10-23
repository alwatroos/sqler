// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! One export format of a plugin as a [`Format`].

use sqler_export::{ExportError, ExportOptions, Format, RowWriter, Sink};
use sqler_plugin_api::PluginId;

use super::writer::PluginWriter;
use crate::runtime::PluginHost;

/// A format a plugin declares under `[[exporters]]`. The strings are interned by the source that made it, because
/// [`Format`] hands out `&'static str`.
pub(crate) struct PluginFormat {
    pub host: PluginHost,
    pub plugin: PluginId,
    /// The exporter id in the manifest.
    pub exporter: String,
    pub id: &'static str,
    pub label: &'static str,
    pub extension: &'static str,
}

impl Format for PluginFormat {
    fn id(&self) -> &'static str {
        self.id
    }

    fn label(&self) -> &'static str {
        self.label
    }

    fn extension(&self) -> &'static str {
        self.extension
    }

    /// Plugin formats take no options: they are ignored.
    fn create<'a>(
        &self,
        _options: &ExportOptions,
        sink: Sink<'a>,
    ) -> Result<Box<dyn RowWriter + 'a>, ExportError> {
        Ok(Box::new(PluginWriter::new(
            self.host.clone(),
            self.plugin.clone(),
            self.exporter.clone(),
            self.label,
            sink,
        )))
    }
}
