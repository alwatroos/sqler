// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin export formats: a [`FormatSource`] that offers the `[[exporters]]` of plugins to an export registry.
//!
//! A plugin declares `[[exporters]]` in its manifest and implements the WIT `exporter` interface. The guest turns
//! rows into bytes and the host writes them to the sink it was given, so the guest never gets a file, a path or the
//! file system, and the partial-file protocol, the cancel checks and the progress reporting of the exporter stay as
//! they are. The `exporter` interface needs no capability.
//!
//! Plugins are discovered after the first frame, so a registry built at startup would always be empty. The source
//! is attached to the host once it exists and works out the list on every call: enable, disable and reload need no
//! hook, the next `formats` call sees the new state.
//!
//! A format id is `plugin-<plugin id>:<exporter id>`, the label `<name> (plugin): <label>`. [`Format`] hands out
//! `&'static str`, so each distinct string is leaked once by the interner; the amount is bounded by the distinct
//! exporter ids, labels and extensions loaded in the process.

mod convert;
mod error;
mod format;
mod writer;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use sqler_export::{Format, FormatSource};

use self::format::PluginFormat;
use crate::runtime::PluginHost;
use crate::ui::limits::one_line;

/// Bytes of each part of a label (plugin name, exporter label).
const LABEL_PART_BYTES: usize = 64;

/// The export formats of plugins as a [`FormatSource`].
///
/// Create it, hand it to the export registry, and [`attach`](Self::attach) the plugin host once it exists. Until
/// then it offers nothing.
pub struct PluginExporters {
    host: OnceLock<PluginHost>,
    interned: Mutex<HashSet<&'static str>>,
}

impl PluginExporters {
    /// A source with no host yet.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            host: OnceLock::new(),
            interned: Mutex::new(HashSet::new()),
        })
    }

    /// Connects the source to the plugin host. The first call wins; later ones are ignored.
    pub fn attach(&self, host: &PluginHost) {
        let _ = self.host.set(host.clone());
    }

    /// The one `&'static str` for `text`. The string is leaked the first time it is seen.
    fn intern(&self, text: &str) -> &'static str {
        let mut set = self
            .interned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(known) = set.get(text) {
            return known;
        }
        let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
        set.insert(leaked);
        leaked
    }

    #[cfg(test)]
    fn interned_count(&self) -> usize {
        self.interned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// The id of the format of exporter `exporter` of plugin `plugin`.
fn format_id(plugin: &str, exporter: &str) -> String {
    format!("plugin-{plugin}:{exporter}")
}

/// The label of a format: `<name> (plugin): <label>`, each part cleaned to one line and cut.
fn format_label(name: &str, label: &str) -> String {
    format!(
        "{} (plugin): {}",
        one_line(name, LABEL_PART_BYTES),
        one_line(label, LABEL_PART_BYTES)
    )
}

impl FormatSource for PluginExporters {
    fn formats(&self) -> Vec<Box<dyn Format>> {
        let Some(host) = self.host.get() else {
            return Vec::new();
        };
        let mut out: Vec<Box<dyn Format>> = Vec::new();
        for (plugin, disabled) in host.exporter_plugins() {
            if disabled {
                continue;
            }
            let manifest = &plugin.manifest;
            for exporter in &manifest.contributes().exporters {
                out.push(Box::new(PluginFormat {
                    host: host.clone(),
                    plugin: manifest.id().clone(),
                    exporter: exporter.id.as_str().to_owned(),
                    id: self.intern(&format_id(manifest.id().as_str(), exporter.id.as_str())),
                    label: self.intern(&format_label(manifest.name(), &exporter.label)),
                    extension: self.intern(&exporter.extension),
                }));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_one_line_and_cut_per_part() {
        assert_eq!(
            format_label("Acme", "Tab-separated"),
            "Acme (plugin): Tab-separated"
        );
        // Line breaks become spaces; controls, bidi marks and U+2028 are dropped.
        assert_eq!(
            format_label("Ac\nme\u{202E}", "Ta\u{2028}b\u{7}s\tx"),
            "Ac me (plugin): Tabs x"
        );
        let long = "é".repeat(100);
        let label = format_label(&long, &long);
        let (name, rest) = label.split_once(" (plugin): ").unwrap();
        assert_eq!(name.len(), 64);
        assert_eq!(rest.len(), 64);
        assert!(name.chars().all(|c| c == 'é'));
    }

    #[test]
    fn ids_follow_the_plugin_form() {
        assert_eq!(format_id("acme", "tsv"), "plugin-acme:tsv");
    }

    #[test]
    fn the_interner_leaks_once_per_distinct_string() {
        let source = PluginExporters::new();
        let a = source.intern("plugin-a:x");
        let b = source.intern(&String::from("plugin-a:x"));
        let c = source.intern("tsv");
        assert!(std::ptr::eq(a, b));
        assert_eq!(c, "tsv");
        assert_eq!(source.interned_count(), 2);
        for _ in 0..10 {
            source.intern("tsv");
        }
        assert_eq!(source.interned_count(), 2);
    }

    #[test]
    fn without_a_host_nothing_is_offered() {
        assert!(PluginExporters::new().formats().is_empty());
    }
}
