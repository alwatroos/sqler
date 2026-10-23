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
/// Each engine sits behind its own feature: `driver-mssql` links SQL Server (ADR-0071). The `DuckDB` plugin adds its
/// engine here the same way.
fn engines() -> HostEngines {
    #[allow(unused_mut)]
    let mut engines = HostEngines::new();
    #[cfg(feature = "driver-mssql")]
    {
        use sqler_driver_mssql::{ENGINE_ID, MssqlDriver, install_ring_provider};
        // rustls has both providers compiled in (tiberius pulls aws-lc-rs); ring is the one in effect.
        install_ring_provider();
        if let Ok(id) = ENGINE_ID.parse::<sqler_plugin_api::EngineId>() {
            let _ = engines.register(id, Arc::new(MssqlDriver::new()));
        }
    }
    engines
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::DriverId;
    use sqler_driver_api::{DriverSource, SourceLookup};

    #[cfg(feature = "driver-mssql")]
    #[test]
    fn the_sql_server_engine_is_linked_under_the_feature() {
        assert!(format!("{:?}", engines()).contains("mssql"));
    }

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
