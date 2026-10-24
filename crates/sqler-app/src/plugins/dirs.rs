// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Where plugins live (ADR-0057, ADR-0060): the user's plugin folder, the explicit development paths and the data
//! folders the app creates.
//!
//! Development paths come from `--plugin-dir` and nowhere else. In particular `target/plugins`, where the probe
//! component is staged, is never searched, in any build profile.
//!
//! Built-in plugins are a third source after `--plugin-dir` and `<config>/plugins`: a manifest compiled into the app
//! (the SQL Server one, under `driver-mssql`) is offered when no plugin of that id was found on disk. It goes through
//! the normal grant flow and nothing is written for it.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[cfg(feature = "driver-mssql")]
use std::sync::Arc;

#[cfg(feature = "driver-mssql")]
use sqler_plugin_api::{API_VERSION, FsRoots, Manifest};
#[cfg(feature = "driver-mssql")]
use sqler_plugin_host::{DiscoveredPlugin, LoadError, LoadProblem, PluginSource};
use sqler_plugin_host::{Discovery, PluginDirs};

/// The in-repo SQL Server manifest, byte for byte, so its grant key equals the one of a copy staged by `xtask`.
#[cfg(feature = "driver-mssql")]
pub(super) const SQLSERVER_MANIFEST: &str =
    include_str!("../../../../plugins/sqlserver/sqler.toml");

/// Built-in plugins whose id is not already in `found`: none without `config_plugins` or without `driver-mssql`.
///
/// The directory is `<config_plugins>/<id>`, stable across refreshes and never created.
#[cfg_attr(not(feature = "driver-mssql"), allow(unused_variables))]
pub(super) fn builtin_plugins(dirs: &PluginDirs, found: &Discovery) -> Discovery {
    #[cfg_attr(not(feature = "driver-mssql"), allow(unused_mut))]
    let mut out = Discovery::default();
    #[cfg(feature = "driver-mssql")]
    if let Some(root) = &dirs.config_plugins {
        let dir = root.join("sqlserver");
        match build_sqlserver(&dir, dirs) {
            Ok(p) => {
                if !found
                    .plugins
                    .iter()
                    .any(|f| f.manifest.id() == p.manifest.id())
                {
                    out.plugins.push(p);
                }
            }
            Err((id, error)) => out.problems.push(LoadProblem { dir, id, error }),
        }
    }
    out
}

#[cfg(feature = "driver-mssql")]
fn build_sqlserver(
    dir: &Path,
    dirs: &PluginDirs,
) -> Result<DiscoveredPlugin, (Option<sqler_plugin_api::PluginId>, LoadError)> {
    let manifest =
        Manifest::parse(SQLSERVER_MANIFEST).map_err(|e| (None, LoadError::Manifest(e)))?;
    let id = manifest.id().clone();
    manifest
        .check_api(API_VERSION)
        .map_err(|e| (Some(id.clone()), LoadError::Incompatible(e)))?;
    let key = manifest.grant_key(None).map_err(|e| {
        (
            Some(id.clone()),
            LoadError::Io {
                path: dir.to_path_buf(),
                message: format!("no grant key: {e}"),
            },
        )
    })?;
    Ok(DiscoveredPlugin {
        manifest: Arc::new(manifest),
        dir: dir.to_path_buf(),
        source: PluginSource::Config,
        component: None,
        key,
        roots: FsRoots {
            plugin_dir: dir.to_path_buf(),
            data_dir: dir.join("data"),
            home_dir: dirs.home.clone(),
        },
    })
}

/// The directories discovery uses.
///
/// `config_plugins` is `<config>/plugins` made absolute and canonical (the scope rules refuse anything else); `dev`
/// is exactly the paths of `--plugin-dir`, made absolute.
pub(super) fn plugin_dirs(
    config_plugins: Option<&Path>,
    dev: &[PathBuf],
    home: Option<PathBuf>,
) -> PluginDirs {
    PluginDirs {
        config_plugins: config_plugins.and_then(canonical),
        dev: dev
            .iter()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
            .collect(),
        home,
    }
}

/// An absolute, canonical spelling of `path` even when its last components do not exist yet: the longest existing
/// ancestor is canonicalised and the rest is appended. `None` when that is not possible (a `..` in the missing part).
fn canonical(path: &Path) -> Option<PathBuf> {
    let mut current = std::path::absolute(path).ok()?;
    let mut missing = Vec::new();
    loop {
        if let Ok(mut done) = current.canonicalize() {
            done.extend(missing.iter().rev());
            return Some(done);
        }
        missing.push(current.file_name()?.to_owned());
        if !current.pop() {
            return None;
        }
    }
}

/// Makes sure `data` is a real directory: parents are created, the folder itself with `create_dir`, and a
/// `symlink_metadata` check refuses anything that is not a plain directory (a link someone put there beforehand).
/// Nothing is followed or removed.
///
/// # Errors
/// A one-line reason, without the path.
pub(super) fn ensure_data_dir(data: &Path) -> Result<(), String> {
    if let Some(parent) = data.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create the plugin data folder: {}", e.kind()))?;
    }
    match fs::create_dir(data) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(format!(
                "cannot create the plugin data folder: {}",
                e.kind()
            ));
        }
    }
    let meta = fs::symlink_metadata(data)
        .map_err(|e| format!("cannot inspect the plugin data folder: {}", e.kind()))?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(
            "the plugin data folder is a link or not a folder; SQLer does not follow it".to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::scratch::Scratch;

    #[test]
    fn dev_paths_are_only_what_was_given() {
        let none = plugin_dirs(None, &[], None);
        assert_eq!(none.dev, Vec::<PathBuf>::new());
        assert!(none.config_plugins.is_none());

        let s = Scratch::new("dirs-dev");
        let given = vec![s.path().join("one"), s.path().join("two")];
        let dirs = plugin_dirs(Some(&s.path().join("plugins")), &given, None);
        assert_eq!(dirs.dev, given);
        for p in dirs.dev.iter().chain(dirs.config_plugins.iter()) {
            let text = p.to_string_lossy().replace('\\', "/");
            assert!(!text.contains("target/plugins"), "{text}");
        }
    }

    #[test]
    fn a_relative_dev_path_becomes_absolute() {
        let dirs = plugin_dirs(None, &[PathBuf::from("some/where")], None);
        assert!(dirs.dev[0].is_absolute());
    }

    #[test]
    fn config_plugins_is_absolute_and_canonical_even_before_it_exists() {
        let s = Scratch::new("dirs-canon");
        let wanted = s.path().join("cfg").join("plugins");
        let got = plugin_dirs(Some(&wanted), &[], None)
            .config_plugins
            .unwrap();
        assert!(got.is_absolute());
        let root = s.path().canonicalize().unwrap();
        assert_eq!(got, root.join("cfg").join("plugins"));
        // Nothing was created by computing the path.
        assert!(!wanted.exists());
    }

    #[test]
    fn config_plugins_resolves_an_existing_folder() {
        let s = Scratch::new("dirs-exist");
        let wanted = s.path().join("plugins");
        fs::create_dir(&wanted).unwrap();
        let got = plugin_dirs(Some(&wanted), &[], None)
            .config_plugins
            .unwrap();
        assert_eq!(got, wanted.canonicalize().unwrap());
    }

    #[test]
    fn a_dot_dot_in_the_missing_part_gives_no_config_folder() {
        let s = Scratch::new("dirs-dotdot");
        let wanted = s.path().join("missing").join("..").join("plugins");
        let got = plugin_dirs(Some(&wanted), &[], None).config_plugins;
        // Windows folds the `..` away lexically while making the path absolute, so there the answer is a path
        // without any `..`; elsewhere the `..` survives and the path is refused.
        if cfg!(windows) {
            assert!(got.is_none_or(|p| {
                !p.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            }));
        } else {
            assert!(got.is_none());
        }
    }

    #[test]
    fn the_data_dir_is_created_as_a_real_directory() {
        let s = Scratch::new("dirs-data");
        let data = s.path().join("plugins").join("demo").join("data");
        ensure_data_dir(&data).unwrap();
        let meta = fs::symlink_metadata(&data).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
        // Idempotent.
        ensure_data_dir(&data).unwrap();
    }

    #[test]
    fn a_plain_file_in_place_of_the_data_dir_is_refused() {
        let s = Scratch::new("dirs-file");
        let parent = s.path().join("demo");
        fs::create_dir_all(&parent).unwrap();
        let data = parent.join("data");
        fs::write(&data, b"x").unwrap();
        assert!(ensure_data_dir(&data).is_err());
        assert!(data.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_pre_placed_link_is_refused_and_not_followed() {
        let s = Scratch::new("dirs-link");
        let parent = s.path().join("demo");
        let elsewhere = s.path().join("elsewhere");
        fs::create_dir_all(&parent).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        let data = parent.join("data");
        std::os::unix::fs::symlink(&elsewhere, &data).unwrap();
        let err = ensure_data_dir(&data).unwrap_err();
        assert!(err.contains("does not follow"), "{err}");
        // The link is still there and the target is untouched.
        assert!(
            fs::symlink_metadata(&data)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[cfg(feature = "driver-mssql")]
    mod builtin {
        use super::*;

        #[test]
        fn the_embedded_manifest_is_the_sqlserver_shell() {
            let m = Manifest::parse(SQLSERVER_MANIFEST).unwrap();
            m.check_api(API_VERSION).unwrap();
            assert_eq!(m.id().as_str(), "sqlserver");
            let caps: Vec<String> = m.capabilities().iter().map(ToString::to_string).collect();
            assert_eq!(caps, vec!["driver.host:mssql".to_owned()]);
            assert!(m.entry().is_none());
        }

        #[test]
        fn the_embedded_manifest_equals_the_file_in_the_repo() {
            let path =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/sqlserver/sqler.toml");
            assert_eq!(SQLSERVER_MANIFEST, fs::read_to_string(path).unwrap());
        }

        #[test]
        fn nothing_is_offered_without_a_config_folder() {
            let dirs = plugin_dirs(None, &[], None);
            let got = builtin_plugins(&dirs, &Discovery::default());
            assert!(got.plugins.is_empty() && got.problems.is_empty());
        }

        #[test]
        fn nothing_is_offered_when_sqlserver_was_found() {
            let s = Scratch::new("dirs-builtin-found");
            let dirs = plugin_dirs(Some(&s.path().join("plugins")), &[], None);
            let first = builtin_plugins(&dirs, &Discovery::default());
            assert_eq!(first.plugins.len(), 1);
            let p = &first.plugins[0];
            assert_eq!(
                p.dir,
                dirs.config_plugins.as_ref().unwrap().join("sqlserver")
            );
            assert!(p.component.is_none());
            assert!(!s.path().join("plugins").exists());
            let again = builtin_plugins(&dirs, &first);
            assert!(again.plugins.is_empty() && again.problems.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_is_refused_too() {
        let s = Scratch::new("dirs-dangling");
        let parent = s.path().join("demo");
        fs::create_dir_all(&parent).unwrap();
        let data = parent.join("data");
        std::os::unix::fs::symlink(s.path().join("nowhere"), &data).unwrap();
        assert!(ensure_data_dir(&data).is_err());
        assert!(!s.path().join("nowhere").exists());
    }
}
