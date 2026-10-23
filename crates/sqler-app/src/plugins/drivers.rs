// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The drivers of plugins (ADR-0061): one process-wide [`PluginDrivers`] source.
//!
//! `registry()` adds the source before the plugin host exists, and the plugin backend attaches the host to it as soon
//! as the host is built (on the first refresh, after the first frame). Until then the source offers nothing. A driver
//! of a plugin appears in the connection form and connects through the same pipeline as a built-in one.

use std::sync::{Arc, OnceLock};

use sqler_plugin_host::{HostEngines, PluginConnection, PluginDrivers};

static SOURCE: OnceLock<Arc<PluginDrivers>> = OnceLock::new();

/// The source every registry of this process shares.
pub(crate) fn source() -> Arc<PluginDrivers> {
    Arc::clone(SOURCE.get_or_init(|| PluginDrivers::new(engines())))
}

/// `conn` with its SQL dialect completed: a built-in driver or a dialect already set stays, any other driver gets the
/// one its plugin manifest declares (only while the driver is available). May block briefly: it reads the host state.
pub(crate) fn declare_dialect(conn: PluginConnection) -> PluginConnection {
    let dialect = conn.dialect.or_else(|| source().dialect(&conn.driver));
    conn.with_dialect(dialect)
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

    #[test]
    fn a_built_in_driver_keeps_its_dialect_and_an_unattached_plugin_driver_has_none() {
        use sqler_sql::Dialect;
        let of = |driver: &str| {
            declare_dialect(PluginConnection::new(
                "c",
                "C",
                DriverId::new(driver).unwrap(),
                None,
            ))
            .dialect
        };
        assert_eq!(of("postgres"), Some(Dialect::Postgres));
        // No host is attached in a test: the shared source declares nothing.
        assert_eq!(of("sqlserver"), None);
    }

    /// The production gate on a `sqlserver` connection (ADR-0073): the dialect is the one the shipped manifest
    /// declares, so a `prod` run starts a plain read and asks first for a write or an `exec`. No server needed.
    #[test]
    fn prod_gate_follows_the_dialect_the_sqlserver_manifest_declares() {
        use sqler_plugin_host::{PluginDirs, discover};
        use sqler_sql::Dialect;
        use sqler_ui::query::{BufferSnapshot, QueryTab, RunMode, Step, plan};

        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/sqlserver");
        let found = discover(&PluginDirs {
            dev: vec![dir],
            ..PluginDirs::default()
        });
        let manifest = &found.plugins[0].manifest;
        let declared = manifest.contributes().drivers[0].dialect.as_deref();
        let dialect = declared.and_then(Dialect::from_name);
        assert_eq!(dialect, Some(Dialect::TSql));

        let step = |sql: &str| {
            let buffer = BufferSnapshot::at_cursor(sql, 0);
            let p = plan(dialect, &buffer, RunMode::All).unwrap();
            QueryTab::new().request(dialect, p, true)
        };
        assert!(matches!(step("select 1"), Ok(Step::Start(_))));
        assert_eq!(step("update t set a = 1"), Ok(Step::Confirm));
        assert_eq!(step("exec p"), Ok(Step::Confirm));
    }

    /// The same gate against a real SQL Server (skipped without `SQLER_TEST_MSSQL_URL`, ADR-0071): a dismissed
    /// `insert` writes nothing, a confirmed one writes the row.
    #[cfg(feature = "driver-mssql")]
    #[tokio::test]
    async fn prod_gate_confirms_a_sqlserver_write() {
        use sqler_driver_api::sqler_core::{ConnectionConfig, Query, Value};
        use sqler_driver_api::{Driver as _, collect};
        use sqler_driver_mssql::MssqlDriver;
        use sqler_sql::Dialect;
        use sqler_ui::query::{BufferSnapshot, QueryTab, RunMode, Step, plan};

        let Some(url) = std::env::var("SQLER_TEST_MSSQL_URL")
            .ok()
            .filter(|v| !v.is_empty())
        else {
            eprintln!("skipped: SQLER_TEST_MSSQL_URL is not set (see AGENTS.md section 3)");
            return;
        };
        let dialect = Some(Dialect::TSql);
        let table = format!("sqler_gate_{}", std::process::id());
        let config = ConnectionConfig::new("t", DriverId::new("sqlserver").unwrap(), &url);
        let mut session = MssqlDriver::new().connect(&config).await.unwrap();
        let mut run = async |sql: &str| {
            let q = Query::new(sql);
            let mut stream = session.execute(&q).await.unwrap();
            collect(&mut *stream).await.unwrap()
        };
        run(&format!("create table [{table}] (a int)")).await;

        let insert = format!("insert into [{table}] values (1)");
        let buffer = BufferSnapshot::at_cursor(&insert, 0);
        let mut tab = QueryTab::new();
        let p = plan(dialect, &buffer, RunMode::All).unwrap();
        assert_eq!(tab.request(dialect, p, true), Ok(Step::Confirm));
        tab.dismiss();
        let count = format!("select count(*) from [{table}]");
        assert_eq!(
            run(&count).await.rows()[0].get(0),
            Some(&Value::Int(0)),
            "a dismissed insert must write nothing"
        );

        let p = plan(dialect, &buffer, RunMode::All).unwrap();
        assert_eq!(tab.request(dialect, p, true), Ok(Step::Confirm));
        let bound = tab.confirm().unwrap();
        assert_eq!(bound.len(), 1);
        run(&bound[0].query.sql).await;
        let rows = run(&count).await;
        assert_eq!(rows.rows()[0].get(0), Some(&Value::Int(1)));
        run(&format!("drop table [{table}]")).await;
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
