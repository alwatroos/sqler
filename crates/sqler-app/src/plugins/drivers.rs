// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The drivers of plugins (ADR-0061): one process-wide [`PluginDrivers`] source.
//!
//! `registry()` adds the source before the plugin host exists, and the plugin backend attaches the host to it as soon
//! as the host is built (on the first refresh, after the first frame). Until then the source offers nothing. A driver
//! of a plugin appears in the connection form and connects through the same pipeline as a built-in one.

use std::sync::{Arc, OnceLock};

use sqler_plugin_host::{HostEngines, PluginDrivers};

static SOURCE: OnceLock<Arc<PluginDrivers>> = OnceLock::new();

/// The source every registry of this process shares.
pub(crate) fn source() -> Arc<PluginDrivers> {
    Arc::clone(SOURCE.get_or_init(|| PluginDrivers::new(engines())))
}

/// The native engines this build links, for plugins that declare `driver.host:<engine>`.
///
/// None yet: the `DuckDB` and SQL Server plugins add their engines here, each behind its own feature.
fn engines() -> HostEngines {
    HostEngines::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::DriverId;
    use sqler_driver_api::{DriverSource, SourceLookup};

    #[test]
    fn the_source_is_shared_and_offers_nothing_without_a_host() {
        assert!(Arc::ptr_eq(&source(), &source()));
        let s = source();
        assert_eq!(s.ids().len(), 0);
        assert!(matches!(
            s.lookup(&DriverId::new("anything").unwrap()),
            SourceLookup::Unknown
        ));
    }
}
