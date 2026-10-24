// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin drivers and export formats for the CLI (feature `plugins`, ADR-0061, ADR-0072).
//!
//! The CLI looks in `<config>/plugins` and nowhere else (there is no `--plugin-dir`), honours the grants stored in
//! `plugin-grants.toml` and the plugins switched off in `plugin-prefs.toml`, and never prompts: a plugin the user has
//! not approved offers no driver. The driver host runs on the runtime the command already has; the export formats
//! get a runtime of their own.

use std::str::FromStr as _;
use std::sync::{Arc, OnceLock};

use sqler_config::{ConfigDir, GrantDecision, GrantsLoad, PluginGrants, PrefsLoad};
use sqler_driver_api::sqler_core::DriverId;
use sqler_plugin_api::{Capability, CapabilitySet, GrantKey, GrantSource, Limits, PluginId};
use sqler_plugin_host::net::NetEgressFactory;
use sqler_plugin_host::{
    HostEngines, HostOptions, MemorySettings, PluginDirs, PluginDrivers, PluginExporters,
    PluginHost, Services, discover,
};
use sqler_sql::Dialect;

/// The grants as they were on disk when the command started. Read-only: an unknown key, a denial, a damaged or newer
/// file all approve nothing.
struct StoredGrants(PluginGrants);

impl StoredGrants {
    fn load(dir: &ConfigDir) -> Self {
        match dir.load_plugin_grants() {
            GrantsLoad::Loaded(grants) => Self(grants),
            _ => Self(PluginGrants::new()),
        }
    }
}

impl GrantSource for StoredGrants {
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet> {
        let record = self.0.for_plugin(key.id().as_str())?;
        if record.decision() != GrantDecision::Approved || record.key() != key.to_string() {
            return None;
        }
        let mut set = CapabilitySet::new();
        for text in record.capabilities() {
            set.insert(Capability::from_str(text).ok()?);
        }
        Some(set)
    }
}

/// The plugin driver source for the user's configuration folder, or `None` when there is nothing to offer (no
/// folder, no plugin directory, no runtime). Call it inside a tokio runtime.
pub(crate) fn driver_source() -> Option<Arc<PluginDrivers>> {
    let dir = ConfigDir::platform().ok()?;
    driver_source_in(&dir)
}

/// Like [`driver_source`] for an explicit configuration folder.
pub(crate) fn driver_source_in(dir: &ConfigDir) -> Option<Arc<PluginDrivers>> {
    let runtime = tokio::runtime::Handle::try_current().ok()?;
    let host = host_in(dir, runtime)?;
    // A plugin that needs an engine this build does not link is listed as unavailable.
    let source = PluginDrivers::new(engines());
    source.attach(&host);
    Some(source)
}

/// The dialect a plugin manifest declares for `driver`, or `None` without a source or a declaration.
pub(crate) fn dialect_for(source: Option<&PluginDrivers>, driver: &DriverId) -> Option<Dialect> {
    source?.dialect(driver)
}

/// The runtime of the export formats: one worker, started on first use and kept for the life of the process.
///
/// An export writes from the command's current-thread runtime and blocks on every plugin call, so the host must
/// run on a runtime of its own (ADR-0072).
static EXPORT_RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();

/// The plugin export formats for the user's configuration folder, or `None` when there is nothing to offer (no
/// folder, no plugin directory, no runtime).
pub(crate) fn format_source() -> Option<Arc<PluginExporters>> {
    let dir = ConfigDir::platform().ok()?;
    format_source_in(&dir)
}

/// Like [`format_source`] for an explicit configuration folder.
pub(crate) fn format_source_in(dir: &ConfigDir) -> Option<Arc<PluginExporters>> {
    let runtime = EXPORT_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()?;
    let host = host_in(dir, runtime.handle().clone())?;
    let source = PluginExporters::new();
    source.attach(&host);
    Some(source)
}

/// A host on `runtime` with the plugins of `<config>/plugins`, the grants stored on disk and the plugins switched
/// off in the prefs, or `None` when there is no plugin folder.
fn host_in(dir: &ConfigDir, runtime: tokio::runtime::Handle) -> Option<PluginHost> {
    let plugins = dir.root().join("plugins").canonicalize().ok()?;
    let discovery = discover(&PluginDirs {
        config_plugins: Some(plugins),
        dev: Vec::new(),
        home: None,
    });
    let host = PluginHost::new(HostOptions {
        runtime,
        grants: Arc::new(StoredGrants::load(dir)),
        settings: Arc::new(MemorySettings::default()),
        egress: Arc::new(NetEgressFactory::new()),
        limits: Limits::DEFAULT,
        services: Services::new(),
    });
    // Problems (a plugin that did not load) are not shown: the CLI has no place for them, and the plugin offers no
    // driver.
    let _ = host.load(discovery);
    if let PrefsLoad::Loaded(prefs) = dir.load_plugin_prefs() {
        for id in prefs
            .disabled
            .iter()
            .filter_map(|s| PluginId::from_str(s).ok())
        {
            host.disable(&id);
        }
    }
    Some(host)
}

/// The native engines this build links: `driver-mssql` adds SQL Server (ADR-0071), `driver-duckdb` adds `DuckDB`
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
    use std::fs;
    use std::path::PathBuf;

    use sqler_config::GrantRecord;
    use sqler_driver_api::DriverRegistry;
    use sqler_driver_api::sqler_core::DriverId;

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("sqler-cli-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A manifest-only plugin `p1` with a host driver `nothing-db` on the engine `nothing`, which no CLI build links.
    fn write_plugin(config: &std::path::Path) -> PathBuf {
        let dir = config.join("plugins").join("p1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("sqler.toml"),
            "[plugin]\nid = \"p1\"\nname = \"P\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = [\"driver.host:nothing\"]\n\n[[drivers]]\nid = \"nothing-db\"\nname = \"N\"\nkind = \"host\"\nengine = \"nothing\"\n",
        )
        .unwrap();
        dir
    }

    fn approve(dir: &ConfigDir, plugin: &std::path::Path) {
        let found = discover(&PluginDirs {
            dev: vec![plugin.to_path_buf()],
            ..PluginDirs::default()
        });
        let key = found.plugins[0].key.to_string();
        let mut grants = PluginGrants::new();
        grants.record(GrantRecord::approved(key, vec!["driver.host:nothing".to_owned()]).unwrap());
        dir.save_plugin_grants(&grants).unwrap();
    }

    fn config(driver: &str) -> sqler_driver_api::sqler_core::ConnectionConfig {
        sqler_driver_api::sqler_core::ConnectionConfig::new(
            "t",
            DriverId::new(driver).unwrap(),
            "x",
        )
    }

    /// The real `plugins/sqlserver` manifest in a config folder, approved.
    fn sqlserver_source(name: &str) -> (Scratch, Arc<PluginDrivers>) {
        let s = Scratch::new(name);
        let dir = ConfigDir::new(&s.0);
        let target = s.0.join("plugins").join("sqlserver");
        fs::create_dir_all(&target).unwrap();
        let manifest =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/sqlserver/sqler.toml");
        fs::copy(manifest, target.join("sqler.toml")).unwrap();
        let found = discover(&PluginDirs {
            dev: vec![target],
            ..PluginDirs::default()
        });
        let key = found.plugins[0].key.to_string();
        let mut grants = PluginGrants::new();
        grants.record(GrantRecord::approved(key, vec!["driver.host:mssql".to_owned()]).unwrap());
        dir.save_plugin_grants(&grants).unwrap();
        let source = driver_source_in(&dir).unwrap();
        (s, source)
    }

    #[tokio::test]
    async fn the_sqlserver_manifest_declares_the_tsql_dialect() {
        let (_s, source) = sqlserver_source("mssql-dialect");
        let id = DriverId::new("sqlserver").unwrap();
        // Without the engine the driver is unavailable and has no dialect; with it, T-SQL.
        let expected = cfg!(feature = "driver-mssql").then_some(Dialect::TSql);
        assert_eq!(dialect_for(Some(&source), &id), expected);
        assert_eq!(dialect_for(None, &id), None);
        let other = DriverId::new("nothing-db").unwrap();
        assert_eq!(dialect_for(Some(&source), &other), None);
    }

    #[cfg(feature = "driver-mssql")]
    #[tokio::test]
    async fn the_sqlserver_driver_is_available_with_the_feature() {
        use sqler_driver_api::{DriverSource, SourceLookup};
        let (_s, source) = sqlserver_source("mssql-on");
        assert!(matches!(
            source.lookup(&DriverId::new("sqlserver").unwrap()),
            SourceLookup::Available(_)
        ));
    }

    #[cfg(not(feature = "driver-mssql"))]
    #[tokio::test]
    async fn the_sqlserver_driver_is_unavailable_without_the_feature() {
        let (_s, source) = sqlserver_source("mssql-off");
        let mut registry = DriverRegistry::new();
        registry.add_source(source);
        let err = registry.connect(&config("sqlserver")).await.err().unwrap();
        assert!(
            matches!(&err, sqler_driver_api::DriverError::Unavailable(t)
                if t == "driver `sqlserver` needs the `mssql` engine, which this build does not include"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn without_plugins_only_the_built_in_drivers_are_there() {
        let s = Scratch::new("none");
        let dir = ConfigDir::new(&s.0);
        assert!(driver_source_in(&dir).is_none());
        fs::create_dir_all(s.0.join("plugins")).unwrap();
        let source = driver_source_in(&dir).expect("an empty plugin folder still gives a source");
        let mut registry = DriverRegistry::new();
        registry.add_source(source);
        assert_eq!(registry.available_ids(), Vec::<DriverId>::new());
    }

    #[tokio::test]
    async fn an_unapproved_plugin_offers_no_driver() {
        let s = Scratch::new("unapproved");
        let dir = ConfigDir::new(&s.0);
        write_plugin(&s.0);
        let source = driver_source_in(&dir).unwrap();
        let mut registry = DriverRegistry::new();
        registry.add_source(source);
        assert_eq!(registry.available_ids(), Vec::<DriverId>::new());
        let err = registry.connect(&config("nothing-db")).await.err().unwrap();
        assert!(
            matches!(&err, sqler_driver_api::DriverError::Unavailable(t)
                if t.contains("needs the permission `driver.host:nothing`") && t.contains("not approved")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn an_approved_plugin_without_its_engine_is_unavailable_with_the_fixed_text() {
        let s = Scratch::new("approved");
        let dir = ConfigDir::new(&s.0);
        let plugin = write_plugin(&s.0);
        approve(&dir, &plugin);
        let source = driver_source_in(&dir).unwrap();
        let mut registry = DriverRegistry::new();
        registry.add_source(source);
        let err = registry.connect(&config("nothing-db")).await.err().unwrap();
        assert!(
            matches!(&err, sqler_driver_api::DriverError::Unavailable(t)
                if t == "driver `nothing-db` needs the `nothing` engine, which this build does not include"),
            "{err:?}"
        );
        // The CLI prints that text as is: it is built from ids only.
        assert_eq!(
            crate::driver_message(&err),
            "driver `nothing-db` needs the `nothing` engine, which this build does not include"
        );
    }

    /// The shipped `DuckDB` shell, copied into the scratch config.
    #[cfg(feature = "driver-duckdb")]
    fn write_duckdb_plugin(config: &std::path::Path) -> PathBuf {
        let dir = config.join("plugins").join("duckdb");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("sqler.toml"),
            include_str!("../../../plugins/duckdb/sqler.toml"),
        )
        .unwrap();
        dir
    }

    #[cfg(feature = "driver-duckdb")]
    #[tokio::test]
    async fn the_duckdb_plugin_connects_through_the_registry_once_approved() {
        use sqler_driver_api::sqler_core::Query;

        let s = Scratch::new("duckdb");
        let dir = ConfigDir::new(&s.0);
        let plugin = write_duckdb_plugin(&s.0);

        // Without the grant the driver is unavailable with the fixed text.
        let mut registry = DriverRegistry::new();
        registry.add_source(driver_source_in(&dir).unwrap());
        let err = registry.connect(&config("duckdb")).await.err().unwrap();
        assert!(
            matches!(&err, sqler_driver_api::DriverError::Unavailable(t)
                if t.contains("needs the permission `driver.host:duckdb`") && t.contains("not approved")),
            "{err:?}"
        );

        let found = discover(&PluginDirs {
            dev: vec![plugin],
            ..PluginDirs::default()
        });
        let mut grants = PluginGrants::new();
        grants.record(
            GrantRecord::approved(
                found.plugins[0].key.to_string(),
                vec!["driver.host:duckdb".to_owned()],
            )
            .unwrap(),
        );
        dir.save_plugin_grants(&grants).unwrap();

        let mut registry = DriverRegistry::new();
        registry.add_source(driver_source_in(&dir).unwrap());
        assert!(
            registry
                .available_ids()
                .contains(&DriverId::new("duckdb").unwrap())
        );
        let mut session = registry
            .connect(&sqler_driver_api::sqler_core::ConnectionConfig::new(
                "t",
                DriverId::new("duckdb").unwrap(),
                ":memory:",
            ))
            .await
            .unwrap();
        let q = Query::new("select 41 + 1");
        let mut stream = session.execute(&q).await.unwrap();
        let rs = sqler_driver_api::collect(&mut *stream).await.unwrap();
        assert_eq!(
            rs.rows()[0].get(0),
            Some(&sqler_driver_api::sqler_core::Value::Int(42))
        );
    }

    #[tokio::test]
    async fn a_plugin_switched_off_in_the_prefs_is_disabled() {
        let s = Scratch::new("prefs-off");
        let dir = ConfigDir::new(&s.0);
        let plugin = write_plugin(&s.0);
        approve(&dir, &plugin);
        let mut prefs = sqler_config::PluginPrefs::default();
        prefs.disabled.insert("p1".to_owned());
        dir.save_plugin_prefs(&prefs).unwrap();
        let source = driver_source_in(&dir).unwrap();
        let mut registry = DriverRegistry::new();
        registry.add_source(source);
        let err = registry.connect(&config("nothing-db")).await.err().unwrap();
        assert!(
            matches!(&err, sqler_driver_api::DriverError::Unavailable(t)
                if t == "driver `nothing-db` comes from plugin `p1`, which is disabled"),
            "{err:?}"
        );
    }
}
