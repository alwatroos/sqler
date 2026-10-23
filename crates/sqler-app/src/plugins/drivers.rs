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
/// Each engine sits behind its own feature: `driver-mssql` links SQL Server (ADR-0071), `driver-duckdb` links `DuckDB`
/// (ADR-0070; not in 1.0, off by default).
fn engines() -> HostEngines {
    #[allow(unused_mut)]
    let mut engines = HostEngines::new();
    #[cfg(feature = "driver-duckdb")]
    if let Ok(id) = sqler_driver_duckdb::ENGINE_ID.parse::<sqler_plugin_api::EngineId>() {
        // A duplicate cannot happen in a fresh table.
        let _ = engines.register(id, Arc::new(sqler_driver_duckdb::DuckDbDriver::new()));
    }
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

    #[cfg(feature = "driver-duckdb")]
    #[test]
    fn the_duckdb_engine_is_registered() {
        assert!(format!("{:?}", engines()).contains("duckdb"));
    }

    /// The production gate on a `DuckDB` connection (ADR-0070, gap G1): no dialect, so every statement is `Unknown`
    /// and a `prod` run asks first; a dismissed confirmation writes nothing, a confirmed one writes the row.
    #[cfg(feature = "driver-duckdb")]
    #[tokio::test]
    async fn prod_gate_confirms_a_duckdb_write() {
        use sqler_driver_api::sqler_core::{ConnectionConfig, Query, Value};
        use sqler_driver_api::{Driver as _, collect};
        use sqler_driver_duckdb::{DRIVER_ID, DuckDbDriver};
        use sqler_sql::Dialect;
        use sqler_ui::query::{BufferSnapshot, QueryTab, RunMode, Step, plan};

        assert!(Dialect::from_driver_id(DRIVER_ID).is_none());
        let path =
            std::env::temp_dir().join(format!("sqler-app-prod-gate-{}.duckdb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let config = ConnectionConfig::new(
            "t",
            DriverId::new(DRIVER_ID).unwrap(),
            path.to_str().unwrap(),
        );
        let driver = DuckDbDriver::new();
        let mut session = driver.connect(&config).await.unwrap();
        let mut run = async |sql: &str| {
            let q = Query::new(sql);
            let mut stream = session.execute(&q).await.unwrap();
            collect(&mut *stream).await.unwrap()
        };
        run("create table t (a integer)").await;

        let buffer = BufferSnapshot::at_cursor("insert into t values (1)", 0);
        let mut tab = QueryTab::new();
        let p = plan(None, &buffer, RunMode::All).unwrap();
        assert_eq!(tab.request(None, p, true), Ok(Step::Confirm));
        tab.dismiss();
        assert_eq!(
            run("select count(*) from t").await.rows()[0].get(0),
            Some(&Value::Int(0))
        );

        let p = plan(None, &buffer, RunMode::All).unwrap();
        assert_eq!(tab.request(None, p, true), Ok(Step::Confirm));
        let bound = tab.confirm().unwrap();
        assert_eq!(bound.len(), 1);
        run(&bound[0].query.sql).await;
        assert_eq!(
            run("select count(*) from t").await.rows()[0].get(0),
            Some(&Value::Int(1))
        );

        drop(session);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));
    }
}
