// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin drivers for the CLI (feature `plugins`, ADR-0061).
//!
//! The CLI looks in `<config>/plugins` and nowhere else (there is no `--plugin-dir`), honours the grants stored in
//! `plugin-grants.toml` and the plugins switched off in `plugin-prefs.toml`, and never prompts: a plugin the user has
//! not approved offers no driver. The host runs on the runtime the command already has.

use std::str::FromStr as _;
use std::sync::Arc;

use sqler_config::{ConfigDir, GrantDecision, GrantsLoad, PluginGrants, PrefsLoad};
use sqler_plugin_api::{Capability, CapabilitySet, GrantKey, GrantSource, Limits, PluginId};
use sqler_plugin_host::net::NetEgressFactory;
use sqler_plugin_host::{
    HostEngines, HostOptions, MemorySettings, PluginDirs, PluginDrivers, PluginHost, Services,
    discover,
};

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
    // No native engine is linked into the CLI yet: a plugin that needs one is listed as unavailable.
    let source = PluginDrivers::new(HostEngines::new());
    source.attach(&host);
    Some(source)
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
