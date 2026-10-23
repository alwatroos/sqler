// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The app's side of the Plugins view: [`AppPlugins`] implements the UI's [`PluginsBackend`] over the plugin host.
//!
//! Nothing happens at construction. The first `refresh` (requested after the first frame) builds the plugins'
//! own tokio runtime and the host; discovery, compile and the epoch thread start from there, on demand. The
//! runtime is separate from the database runtime: a guest spinning on a worker holds it until its deadline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use sqler_config::ConfigDir;
use sqler_driver_api::BoxFuture;
use sqler_plugin_api::{
    CapabilitySet, CommandContext, ContributionId, FsRoot, GrantKey, GrantSource, Limits, PluginId,
    SettingValue, WidgetEvent, WidgetTree,
};
use sqler_plugin_host::net::NetEgressFactory;
use sqler_plugin_host::{
    DisableReason, DiscoveredPlugin, HostOptions, LoadProblem, PluginHost, PluginSource,
    PluginState, PluginStatus, Services, discover, read_setting,
};
use sqler_ui::plugins::backend::PanelRender;
use sqler_ui::plugins::grant::GrantBook;
use sqler_ui::plugins::status::is_unsafe_char;
use sqler_ui::plugins::{PluginProblem, PluginRow, PluginSnapshot, PluginsBackend, RowState};

use super::dirs::{ensure_data_dir, plugin_dirs};
use super::settings::Prefs;
use super::ui;

/// Longest error line shown in the manager, in characters.
const MAX_LINE: usize = 300;

/// A [`GrantSource`] that re-reads `plugin-grants.toml` before every answer, so a revoke made in another `SQLer`
/// instance applies on the plugin's next call. Blocking file I/O of at most 1 MiB: the host asks off the UI thread.
struct ReloadingGrants(Arc<GrantBook>);

impl GrantSource for ReloadingGrants {
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet> {
        self.0.reload();
        self.0.approved(key)
    }
}

/// The runtime and the host, created together on first use.
struct Engine {
    runtime: tokio::runtime::Runtime,
    host: PluginHost,
}

/// What the last refresh found, and what went wrong since.
#[derive(Default)]
struct Cache {
    discovered: Vec<DiscoveredPlugin>,
    problems: Vec<PluginProblem>,
    /// Plugins whose data folder could not be made safe: not started.
    data_errors: HashMap<PluginId, String>,
    /// The last failure of `start`, per plugin.
    last_errors: HashMap<PluginId, String>,
    /// The effective capabilities the host was last told about, to refresh a plugin whose grants changed.
    seen: HashMap<PluginId, CapabilitySet>,
}

/// Where plugins are looked for, kept until the first refresh computes the real paths.
struct Inputs {
    /// `<config>/plugins`.
    config_plugins: Option<PathBuf>,
    /// `--plugin-dir`, nothing else.
    dev: Vec<PathBuf>,
    home: Option<PathBuf>,
}

/// The plugin host as the UI uses it.
pub(super) struct AppPlugins {
    inputs: Inputs,
    grants: Arc<GrantBook>,
    reloading: Arc<ReloadingGrants>,
    prefs: Arc<Prefs>,
    engine: OnceLock<Result<Engine, String>>,
    cache: Arc<Mutex<Cache>>,
    /// What the host lends to plugins (the databases and the write approver).
    services: Services,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One printable line of at most [`MAX_LINE`] characters.
pub(super) fn one_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if is_unsafe_char(c) { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_LINE {
        flat
    } else {
        let mut cut: String = flat.chars().take(MAX_LINE - 1).collect();
        cut.push('…');
        cut
    }
}

impl AppPlugins {
    /// A backend that does nothing yet: no I/O, no thread, no runtime, no engine.
    pub(super) fn new(
        grants: Arc<GrantBook>,
        prefs: Arc<Prefs>,
        config: Option<&ConfigDir>,
        dev: Vec<PathBuf>,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            inputs: Inputs {
                config_plugins: config.map(|c| c.root().join("plugins")),
                dev,
                home,
            },
            reloading: Arc::new(ReloadingGrants(Arc::clone(&grants))),
            grants,
            prefs,
            engine: OnceLock::new(),
            cache: Arc::new(Mutex::new(Cache::default())),
            services: Services::new(),
        }
    }

    /// Sets the services the host gives plugins.
    #[must_use]
    pub(super) fn with_services(mut self, services: Services) -> Self {
        self.services = services;
        self
    }

    /// True while no runtime and no host exist (nothing was ever refreshed).
    #[cfg(test)]
    pub(super) fn is_idle(&self) -> bool {
        self.engine.get().is_none()
    }

    /// The host, built on first use.
    fn host(&self) -> Result<PluginHost, String> {
        self.engine
            .get_or_init(|| self.build())
            .as_ref()
            .map(|e| e.host.clone())
            .map_err(Clone::clone)
    }

    /// The host if a refresh built it; never builds.
    fn built(&self) -> Option<PluginHost> {
        match self.engine.get() {
            Some(Ok(e)) => Some(e.host.clone()),
            _ => None,
        }
    }

    fn build(&self) -> Result<Engine, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("sqler-plugins")
            .enable_all()
            .build()
            .map_err(|e| format!("cannot start the plugin runtime: {}", e.kind()))?;
        let host = PluginHost::new(HostOptions {
            runtime: runtime.handle().clone(),
            grants: Arc::clone(&self.reloading) as Arc<dyn GrantSource>,
            settings: Arc::clone(&self.prefs) as Arc<dyn sqler_plugin_host::SettingsStore>,
            egress: Arc::new(NetEgressFactory::new()),
            limits: Limits::DEFAULT,
            services: self.services.clone(),
        });
        Ok(Engine { runtime, host })
    }

    fn snapshot_of(&self, host: &PluginHost) -> PluginSnapshot {
        let statuses: HashMap<PluginId, PluginStatus> = host
            .plugins()
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        let cache = lock(&self.cache);
        let rows = cache
            .discovered
            .iter()
            .map(|p| self.row(p, statuses.get(p.manifest.id()), &cache))
            .collect();
        PluginSnapshot {
            rows,
            problems: cache.problems.clone(),
        }
    }

    fn row(&self, p: &DiscoveredPlugin, status: Option<&PluginStatus>, cache: &Cache) -> PluginRow {
        let id = p.manifest.id();
        let (mut state, mut host_error) = match status.map(|s| &s.state) {
            Some(PluginState::Live) => (RowState::Live, None),
            Some(PluginState::Disabled { reason }) => match reason {
                DisableReason::Faults { count, last } => {
                    (RowState::Faulted { count: *count }, Some(last.to_string()))
                }
                DisableReason::Load(e) => (RowState::LoadFailed, Some(e.to_string())),
                _ => (RowState::Off, None),
            },
            _ => (RowState::Idle, None),
        };
        let enabled = !self.prefs.is_disabled(id.as_str());
        if !enabled {
            state = RowState::Off;
            host_error = None;
        }
        let data_error = cache.data_errors.get(id);
        if data_error.is_some() {
            state = RowState::LoadFailed;
        }
        let last_error = data_error
            .cloned()
            .or_else(|| cache.last_errors.get(id).cloned())
            .or(host_error)
            .map(|m| one_line(&m));
        let settings = p
            .manifest
            .contributes()
            .settings
            .iter()
            .filter_map(|spec| {
                let value = read_setting(&p.manifest, &*self.prefs, spec.key.as_ref())?;
                Some((spec.clone(), value))
            })
            .collect();
        PluginRow {
            manifest: Arc::clone(&p.manifest),
            component: p.component.as_ref().map(|c| c.digest),
            dev: p.source == PluginSource::Dev,
            state,
            enabled,
            effective: status.map(|s| s.effective.clone()).unwrap_or_default(),
            last_error,
            scope_issues: status
                .map(|s| {
                    s.scope_issues
                        .iter()
                        .map(|i| one_line(&i.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            settings,
        }
    }

    /// Brings the host's enabled/disabled state in line with the preferences and the data-folder checks.
    fn apply_states(&self, host: &PluginHost, cache: &Cache) {
        let statuses: HashMap<PluginId, PluginStatus> = host
            .plugins()
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        for p in &cache.discovered {
            let id = p.manifest.id();
            let want_off =
                self.prefs.is_disabled(id.as_str()) || cache.data_errors.contains_key(id);
            let state = statuses.get(id).map(|s| &s.state);
            let user_off = matches!(
                state,
                Some(PluginState::Disabled {
                    reason: DisableReason::User
                })
            );
            let any_off = matches!(state, Some(PluginState::Disabled { .. }));
            if want_off && !any_off {
                host.disable(id);
            } else if !want_off && user_off {
                host.enable(id);
            }
        }
    }

    /// Creates the data folder of every plugin that asks to read below `data`; a folder that cannot be made safe
    /// keeps its plugin from starting.
    fn check_data_dirs(plugins: &[DiscoveredPlugin]) -> HashMap<PluginId, String> {
        let mut errors = HashMap::new();
        for p in plugins {
            let wants_data = p.manifest.capabilities().iter().any(|c| {
                matches!(c, sqler_plugin_api::Capability::FsRead(s) if s.root() == FsRoot::Data)
            });
            if wants_data && let Err(why) = ensure_data_dir(&p.roots.data_dir) {
                errors.insert(p.manifest.id().clone(), why);
            }
        }
        errors
    }

    /// Remembers the effective capabilities of every registered plugin.
    fn note_grants(&self, host: &PluginHost, refresh_changed: bool) {
        let mut cache = lock(&self.cache);
        for s in host.plugins() {
            let changed = cache.seen.get(&s.id) != Some(&s.effective);
            if changed && refresh_changed && cache.seen.contains_key(&s.id) {
                host.refresh_grants(&s.id);
            }
            if changed {
                cache.seen.insert(s.id, s.effective);
            }
        }
    }

    fn problem(p: &LoadProblem) -> PluginProblem {
        let dir = p.dir.file_name().map_or_else(
            || p.dir.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        PluginProblem {
            dir: one_line(&dir),
            id: p.id.as_ref().map(ToString::to_string),
            message: one_line(&p.error.to_string()),
        }
    }
}

impl PluginsBackend for AppPlugins {
    fn refresh(&self) -> PluginSnapshot {
        let host = match self.host() {
            Ok(h) => h,
            Err(message) => {
                return PluginSnapshot {
                    rows: Vec::new(),
                    problems: vec![PluginProblem {
                        dir: "plugins".to_owned(),
                        id: None,
                        message,
                    }],
                };
            }
        };
        self.grants.reload();
        let prefs_problem = self.prefs.reload();
        let dirs = plugin_dirs(
            self.inputs.config_plugins.as_deref(),
            &self.inputs.dev,
            self.inputs.home.clone(),
        );
        let found = discover(&dirs);
        let data_errors = Self::check_data_dirs(&found.plugins);
        let discovered = found.plugins.clone();
        let mut problems: Vec<PluginProblem> = host.load(found).iter().map(Self::problem).collect();
        if let Some(message) = prefs_problem {
            problems.push(PluginProblem {
                dir: "plugin-prefs.toml".to_owned(),
                id: None,
                message,
            });
        }
        {
            let mut cache = lock(&self.cache);
            let ids: Vec<&PluginId> = discovered.iter().map(|p| p.manifest.id()).collect();
            cache.last_errors.retain(|id, _| ids.contains(&id));
            cache.seen.retain(|id, _| ids.contains(&id));
            cache.discovered = discovered;
            cache.problems = problems;
            cache.data_errors = data_errors;
            self.apply_states(&host, &cache);
        }
        self.note_grants(&host, false);
        self.snapshot_of(&host)
    }

    fn snapshot(&self) -> PluginSnapshot {
        let Some(host) = self.built() else {
            return PluginSnapshot::default();
        };
        self.grants.reload();
        self.note_grants(&host, true);
        self.snapshot_of(&host)
    }

    fn set_enabled(&self, id: &PluginId, on: bool) -> Result<(), String> {
        if on && let Some(why) = lock(&self.cache).data_errors.get(id) {
            return Err(why.clone());
        }
        self.prefs
            .set_disabled(id.as_str(), !on)
            .map_err(|m| one_line(&m))?;
        if let Some(host) = self.built() {
            if on {
                host.enable(id);
            } else {
                host.disable(id);
            }
        }
        Ok(())
    }

    fn grants_changed(&self, id: &PluginId) {
        let Some(host) = self.built() else {
            return;
        };
        self.grants.reload();
        host.refresh_grants(id);
        if let Some(s) = host.plugins().into_iter().find(|s| &s.id == id) {
            lock(&self.cache).seen.insert(s.id, s.effective);
        }
    }

    fn set_setting(&self, id: &PluginId, key: &str, value: SettingValue) -> Result<(), String> {
        let host = self
            .built()
            .ok_or_else(|| "plugins are not loaded yet".to_owned())?;
        host.set_setting(id, key, value)
            .map_err(|e| one_line(&e.to_string()))
    }

    fn start(&self, id: &PluginId) -> BoxFuture<'static, Result<(), String>> {
        let refuse = |m: &str| -> BoxFuture<'static, Result<(), String>> {
            let m = m.to_owned();
            Box::pin(std::future::ready(Err(m)))
        };
        let Some(host) = self.built() else {
            return refuse("plugins are not loaded yet");
        };
        let key = {
            let cache = lock(&self.cache);
            if let Some(why) = cache.data_errors.get(id) {
                return refuse(&one_line(why));
            }
            let Some(p) = cache.discovered.iter().find(|p| p.manifest.id() == id) else {
                return refuse("this plugin is not loaded");
            };
            if p.component.is_none() {
                // Nothing to run: a plugin without a component only contributes declarations.
                return Box::pin(std::future::ready(Ok(())));
            }
            p.key.clone()
        };
        if self.prefs.is_disabled(id.as_str()) {
            return refuse("this plugin is switched off");
        }
        if self.reloading.approved(&key).is_none() {
            return refuse("this plugin has no approved permissions");
        }
        let pending = host.init(id);
        let cache = Arc::clone(&self.cache);
        let id = id.clone();
        Box::pin(async move {
            match pending.await {
                Ok(()) => {
                    lock(&cache).last_errors.remove(&id);
                    Ok(())
                }
                Err(e) => {
                    let message = one_line(&e.to_string());
                    lock(&cache).last_errors.insert(id, message.clone());
                    Err(message)
                }
            }
        })
    }

    fn run_command(
        &self,
        id: &PluginId,
        command: &ContributionId,
        context: CommandContext,
    ) -> BoxFuture<'static, Result<(), String>> {
        ui::run_command(self.built(), id, command, context)
    }

    fn render_panel(
        &self,
        id: &PluginId,
        panel: &ContributionId,
    ) -> BoxFuture<'static, PanelRender> {
        ui::render_panel(self.built(), id, panel)
    }

    fn panel_event(
        &self,
        id: &PluginId,
        panel: &ContributionId,
        event: WidgetEvent,
    ) -> BoxFuture<'static, Result<Option<WidgetTree>, String>> {
        ui::panel_event(self.built(), id, panel, event)
    }

    fn status(
        &self,
        id: &PluginId,
        item: &ContributionId,
    ) -> BoxFuture<'static, Result<Option<String>, String>> {
        ui::status(self.built(), id, item)
    }
}

impl Drop for AppPlugins {
    fn drop(&mut self) {
        if let Some(Ok(engine)) = self.engine.take() {
            engine.host.shutdown();
            // Never blocks, and is safe to run from inside another runtime's thread.
            engine.runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::scratch::Scratch;
    use sqler_plugin_api::Manifest;
    use sqler_ui::plugins::grant::GrantCommand;

    const MANIFEST: &str = r#"
[plugin]
id = "demo"
name = "Demo"
version = "1.0.0"
api = "0.1"
capabilities = ["db.read"]

[[settings]]
key = "limit"
label = "Limit"
default = 5
"#;

    fn backend(s: &Scratch) -> (AppPlugins, Arc<GrantBook>, ConfigDir) {
        let dir = ConfigDir::new(s.path());
        let grants = Arc::new(GrantBook::open(dir.clone()));
        let prefs = Arc::new(Prefs::new(Some(dir.clone())));
        let b = AppPlugins::new(Arc::clone(&grants), prefs, Some(&dir), Vec::new(), None);
        (b, grants, dir)
    }

    fn install(s: &Scratch, manifest: &str) {
        let dir = s.path().join("plugins").join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sqler.toml"), manifest).unwrap();
    }

    fn id() -> PluginId {
        "demo".parse().unwrap()
    }

    #[test]
    fn construction_is_idle_and_snapshots_stay_empty_before_the_first_refresh() {
        let s = Scratch::new("be-idle");
        let (b, _grants, _dir) = backend(&s);
        assert!(b.is_idle());
        let snap = b.snapshot();
        assert!(snap.rows.is_empty() && snap.problems.is_empty());
        assert!(b.is_idle(), "a snapshot must not build the host");
        assert!(
            b.set_setting(&id(), "limit", SettingValue::Integer(1))
                .is_err()
        );
        b.grants_changed(&id());
        assert!(b.is_idle());
    }

    #[test]
    fn an_empty_plugin_folder_discovers_nothing_and_compiles_nothing() {
        let s = Scratch::new("be-empty");
        let (b, _grants, _dir) = backend(&s);
        let snap = b.refresh();
        assert!(snap.rows.is_empty() && snap.problems.is_empty());
        let host = b.built().expect("refresh builds the host");
        assert!(host.is_idle(), "no engine, no epoch thread");
        // The plugin folder is not created by looking at it.
        assert!(!s.path().join("plugins").exists());
    }

    #[test]
    fn a_manifest_only_plugin_is_listed_with_its_setting() {
        let s = Scratch::new("be-list");
        install(&s, MANIFEST);
        let (b, _grants, _dir) = backend(&s);
        let snap = b.refresh();
        assert_eq!(snap.rows.len(), 1, "{:?}", snap.problems);
        let row = &snap.rows[0];
        assert_eq!(row.manifest.id(), &id());
        assert!(row.enabled && !row.dev);
        assert_eq!(row.state, RowState::Idle);
        assert!(row.effective.is_empty(), "nothing is approved yet");
        assert_eq!(row.settings.len(), 1);
        assert_eq!(row.settings[0].1, SettingValue::Integer(5));
        assert!(b.built().unwrap().is_idle());
    }

    #[test]
    fn a_bad_folder_is_a_problem_not_a_failure() {
        let s = Scratch::new("be-problem");
        let bad = s.path().join("plugins").join("broken");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("sqler.toml"), "not a manifest").unwrap();
        install(&s, MANIFEST);
        let (b, _grants, _dir) = backend(&s);
        let snap = b.refresh();
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.problems.len(), 1);
        assert_eq!(snap.problems[0].dir, "broken");
    }

    #[test]
    fn a_revoke_in_another_book_is_seen_after_a_snapshot() {
        let s = Scratch::new("be-revoke");
        install(&s, MANIFEST);
        let (b, grants, dir) = backend(&s);
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let prompt = grants.prompt(&manifest, None).unwrap();
        grants.decide(&prompt, GrantCommand::Approve).unwrap();

        let snap = b.refresh();
        assert_eq!(snap.rows[0].effective.len(), 1);

        // Another instance revokes.
        let other = GrantBook::open(dir);
        assert!(other.revoke(&id()).unwrap());

        let snap = b.snapshot();
        assert!(snap.rows[0].effective.is_empty());
    }

    #[test]
    fn disabling_persists_and_a_refresh_keeps_it() {
        let s = Scratch::new("be-disable");
        install(&s, MANIFEST);
        let (b, _grants, dir) = backend(&s);
        b.refresh();
        b.set_enabled(&id(), false).unwrap();
        let snap = b.refresh();
        assert!(!snap.rows[0].enabled);
        assert_eq!(snap.rows[0].state, RowState::Off);
        // A new backend (a restart) sees the choice.
        drop(b);
        let (b2, _g, _d) = backend(&s);
        let _ = dir;
        let snap = b2.refresh();
        assert_eq!(snap.rows[0].state, RowState::Off);
        b2.set_enabled(&id(), true).unwrap();
        assert_eq!(b2.snapshot().rows[0].state, RowState::Idle);
    }

    #[test]
    fn a_setting_is_stored_through_the_host() {
        let s = Scratch::new("be-setting");
        install(&s, MANIFEST);
        let (b, _grants, _dir) = backend(&s);
        b.refresh();
        b.set_setting(&id(), "limit", SettingValue::Integer(9))
            .unwrap();
        assert_eq!(b.snapshot().rows[0].settings[0].1, SettingValue::Integer(9));
        assert!(
            b.set_setting(&id(), "limit", SettingValue::Text("x".into()))
                .is_err()
        );
        assert!(
            b.set_setting(&id(), "nope", SettingValue::Integer(1))
                .is_err()
        );
    }

    #[test]
    fn start_refuses_what_is_not_approved_or_unknown() {
        let s = Scratch::new("be-start");
        install(&s, MANIFEST);
        let (b, _grants, _dir) = backend(&s);
        let block = |f: BoxFuture<'static, Result<(), String>>| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(f)
        };
        assert!(block(b.start(&id())).is_err(), "before the first refresh");
        b.refresh();
        // Declarative plugins have nothing to run, so they start trivially.
        assert!(block(b.start(&id())).is_ok());
        let other: PluginId = "other".parse().unwrap();
        assert!(block(b.start(&other)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_data_folder_link_keeps_the_plugin_from_starting() {
        let manifest = MANIFEST.replace(r#"["db.read"]"#, r#"["db.read", "fs.read:data"]"#);
        let s = Scratch::new("be-datalink");
        install(&s, &manifest);
        let elsewhere = s.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        // `plugins/demo/data` is a link placed beforehand.
        let data = s.path().join("plugins").join("demo").join("data");
        std::os::unix::fs::symlink(&elsewhere, &data).unwrap();
        let (b, _grants, _dir) = backend(&s);
        let snap = b.refresh();
        assert_eq!(snap.rows[0].state, RowState::LoadFailed);
        assert!(
            snap.rows[0]
                .last_error
                .as_deref()
                .unwrap()
                .contains("data folder")
        );
        assert!(b.set_enabled(&id(), true).is_err());
        assert!(
            std::fs::symlink_metadata(&data)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn the_data_folder_is_created_for_a_plugin_that_reads_it() {
        let manifest = MANIFEST.replace(r#"["db.read"]"#, r#"["db.read", "fs.read:data"]"#);
        let s = Scratch::new("be-datadir");
        install(&s, &manifest);
        let (b, _grants, _dir) = backend(&s);
        let snap = b.refresh();
        assert_eq!(
            snap.rows[0].state,
            RowState::Idle,
            "{:?}",
            snap.rows[0].last_error
        );
        let data = s.path().join("plugins").join("demo").join("data");
        let meta = std::fs::symlink_metadata(data).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
    }

    #[test]
    fn no_data_folder_without_a_data_scope() {
        let s = Scratch::new("be-nodata");
        install(&s, MANIFEST);
        let (b, _grants, _dir) = backend(&s);
        b.refresh();
        assert!(!s.path().join("plugins").join("demo").join("data").exists());
    }

    /// The staged smoke plugin when `SQLER_PLUGIN_FIXTURES` points at `target/plugins/<profile>` (as for the host's
    /// guest tests); `None` skips the test.
    fn smoke_fixture() -> Option<PathBuf> {
        let dir = PathBuf::from(std::env::var_os("SQLER_PLUGIN_FIXTURES")?);
        let dir = if dir.is_absolute() {
            dir
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join(dir)
        };
        let smoke = dir.join("smoke");
        smoke.join("sqler.toml").is_file().then_some(smoke)
    }

    #[test]
    fn end_to_end_with_the_smoke_plugin() {
        let Some(smoke) = smoke_fixture() else {
            eprintln!(
                "skipped: set SQLER_PLUGIN_FIXTURES=target/plugins/<profile> to run this test"
            );
            return;
        };
        let s = Scratch::new("be-e2e");
        let dir = ConfigDir::new(s.path());
        let grants = Arc::new(GrantBook::open(dir.clone()));
        let prefs = Arc::new(Prefs::new(Some(dir.clone())));
        let b = AppPlugins::new(
            Arc::clone(&grants),
            prefs,
            Some(&dir),
            vec![smoke.clone()],
            None,
        );
        let smoke_id: PluginId = "smoke".parse().unwrap();
        let block = |f: BoxFuture<'static, Result<(), String>>| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(f)
        };

        let snap = b.refresh();
        assert_eq!(snap.rows.len(), 1, "{:?}", snap.problems);
        assert!(snap.rows[0].dev && snap.rows[0].component.is_some());
        assert!(b.built().unwrap().is_idle(), "refresh compiles nothing");
        // Not approved: refused, still nothing compiled.
        assert!(block(b.start(&smoke_id)).is_err());
        assert!(b.built().unwrap().is_idle());

        let manifest = Arc::clone(&snap.rows[0].manifest);
        let digest = snap.rows[0].component;
        let prompt = grants.prompt(&manifest, digest.as_ref()).unwrap();
        grants.decide(&prompt, GrantCommand::Approve).unwrap();
        b.grants_changed(&smoke_id);

        block(b.start(&smoke_id)).expect("the smoke plugin starts");
        assert_eq!(b.snapshot().rows[0].state, RowState::Live);

        // Disabling stops it; a later start is refused.
        b.set_enabled(&smoke_id, false).unwrap();
        assert_eq!(b.snapshot().rows[0].state, RowState::Off);
        assert!(block(b.start(&smoke_id)).is_err());
    }

    #[test]
    fn one_line_flattens_and_cuts() {
        assert_eq!(one_line("a\nb\t c"), "a b c");
        assert_eq!(one_line("a\u{202e}b\u{2028}c"), "a b c");
        let long = "x".repeat(1000);
        assert_eq!(one_line(&long).chars().count(), MAX_LINE);
    }
}
