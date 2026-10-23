// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The export formats of plugins (ADR-0072): one process-wide [`PluginExporters`] source.
//!
//! `main` hands the source to the window, and the plugin backend attaches the host to it as soon as the host is built
//! (on the first refresh, after the first frame). Until then the source offers nothing. A format of a plugin appears in
//! the export dialog after the built-in ones.

use std::sync::{Arc, OnceLock};

use sqler_plugin_host::PluginExporters;

static SOURCE: OnceLock<Arc<PluginExporters>> = OnceLock::new();

/// The source every export dialog of this process shares.
pub(crate) fn source() -> Arc<PluginExporters> {
    Arc::clone(SOURCE.get_or_init(PluginExporters::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_ui::export::FormatSource;

    #[test]
    fn the_source_is_shared_and_offers_nothing_without_a_host() {
        assert!(Arc::ptr_eq(&source(), &source()));
        assert!(source().formats().is_empty());
    }
}
