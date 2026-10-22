// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Finding plugins on disk and validating them. Blocking I/O: never call it on the UI thread.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqler_plugin_api::{
    API_VERSION, ComponentDigest, FsRoots, GrantKey, Limits, Manifest, PluginId,
};

use crate::dirs::PluginDirs;
use crate::error::LoadError;

/// The manifest file name.
pub const MANIFEST_FILE: &str = "sqler.toml";
/// Children looked at per scanned root.
pub const MAX_ENTRIES_PER_ROOT: usize = 256;
/// Largest component the host loads.
pub const MAX_COMPONENT_BYTES: u64 = 64 << 20;

/// Where a plugin came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    /// A development path.
    Dev,
    /// The user's plugin directory.
    Config,
}

/// The component file named by `[entry]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFile {
    /// The canonical path.
    pub path: PathBuf,
    /// SHA-256 of the bytes read at discovery.
    pub digest: ComponentDigest,
    /// Size in bytes.
    pub len: u64,
}

/// A plugin that passed validation.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// The manifest.
    pub manifest: Arc<Manifest>,
    /// The canonical plugin directory.
    pub dir: PathBuf,
    /// Where it came from.
    pub source: PluginSource,
    /// The component, if the manifest has an `[entry]`.
    pub component: Option<ComponentFile>,
    /// The grant key of exactly this manifest and component.
    pub key: GrantKey,
    /// The roots its `fs.read` scopes resolve against.
    pub roots: FsRoots,
}

/// A directory that did not become a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadProblem {
    /// The directory (or the root, for [`LoadError::TooManyEntries`]).
    pub dir: PathBuf,
    /// The id, when the manifest was read far enough to know it.
    pub id: Option<PluginId>,
    /// What went wrong.
    pub error: LoadError,
}

/// The result of [`discover`]. One bad plugin never hides the others.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Valid plugins; dev paths first, then the config directory, each sorted by directory name.
    pub plugins: Vec<DiscoveredPlugin>,
    /// Everything that was refused.
    pub problems: Vec<LoadProblem>,
}

fn io_error(path: &Path, e: &std::io::Error) -> LoadError {
    LoadError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// Reads at most `max + 1` bytes, so a huge or endless file is never held in memory.
fn read_capped(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    File::open(path)?.take(max + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Finds and validates plugins: each `dev` path (a path with `sqler.toml` is one plugin, otherwise its children are
/// scanned), then the children of `config_plugins`. For a duplicate id the first wins, so dev paths beat the config
/// directory. A missing root is not a problem.
#[must_use]
pub fn discover(dirs: &PluginDirs) -> Discovery {
    let mut run = Run {
        dirs,
        out: Discovery::default(),
        seen: HashMap::new(),
    };
    for dev in &dirs.dev {
        run.root(dev, PluginSource::Dev, true);
    }
    if let Some(c) = &dirs.config_plugins {
        run.root(c, PluginSource::Config, false);
    }
    run.out
}

struct Run<'a> {
    dirs: &'a PluginDirs,
    out: Discovery,
    seen: HashMap<PluginId, PathBuf>,
}

impl Run<'_> {
    fn problem(&mut self, dir: &Path, id: Option<PluginId>, error: LoadError) {
        self.out.problems.push(LoadProblem {
            dir: dir.to_path_buf(),
            id,
            error,
        });
    }

    fn root(&mut self, root: &Path, source: PluginSource, allow_single: bool) {
        if allow_single && root.join(MANIFEST_FILE).is_file() {
            self.one(root, source, false);
            return;
        }
        let read = match std::fs::read_dir(root) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                self.problem(root, None, io_error(root, &e));
                return;
            }
        };
        let mut names: Vec<std::ffi::OsString> = Vec::new();
        for entry in read {
            match entry {
                Ok(e) => names.push(e.file_name()),
                Err(e) => self.problem(root, None, io_error(root, &e)),
            }
        }
        names.sort();
        names.retain(|n| !n.to_string_lossy().starts_with('.'));
        if names.len() > MAX_ENTRIES_PER_ROOT {
            names.truncate(MAX_ENTRIES_PER_ROOT);
            self.problem(
                root,
                None,
                LoadError::TooManyEntries {
                    max: MAX_ENTRIES_PER_ROOT,
                },
            );
        }
        for name in names {
            let dir = root.join(name);
            // No manifest: skipped silently (it may be a data directory or a stray file).
            if dir.join(MANIFEST_FILE).is_file() {
                self.one(&dir, source, true);
            }
        }
    }

    fn one(&mut self, dir: &Path, source: PluginSource, name_must_match_id: bool) {
        match load_one(dir, source, name_must_match_id, self.dirs) {
            Ok(p) => {
                let id = p.manifest.id.clone();
                if let Some(first) = self.seen.get(&id) {
                    let first = first.clone();
                    self.problem(dir, Some(id), LoadError::Duplicate { first });
                } else {
                    self.seen.insert(id, p.dir.clone());
                    self.out.plugins.push(p);
                }
            }
            Err((id, e)) => self.problem(dir, id, e),
        }
    }
}

type Failure = (Option<PluginId>, LoadError);

fn load_one(
    dir: &Path,
    source: PluginSource,
    name_must_match_id: bool,
    dirs: &PluginDirs,
) -> Result<DiscoveredPlugin, Failure> {
    let canonical = dir.canonicalize().map_err(|e| (None, io_error(dir, &e)))?;
    let manifest_path = canonical.join(MANIFEST_FILE);
    let max = Limits::DEFAULT.manifest.max_bytes as u64;
    let bytes =
        read_capped(&manifest_path, max).map_err(|e| (None, io_error(&manifest_path, &e)))?;
    let manifest = Manifest::parse_bytes(&bytes).map_err(|e| (None, LoadError::Manifest(e)))?;
    let id = manifest.id.clone();
    let fail = |e: LoadError| (Some(id.clone()), e);

    if name_must_match_id {
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
        if name.as_deref() != Some(manifest.id.as_str()) {
            return Err(fail(LoadError::DirName {
                dir_name: name.unwrap_or_default().chars().take(64).collect(),
            }));
        }
    }
    manifest
        .check_api(API_VERSION)
        .map_err(|e| fail(LoadError::Incompatible(e)))?;

    let component = match &manifest.entry {
        Some(entry) => Some(load_component(&canonical, &entry.component).map_err(fail)?),
        None => None,
    };
    let key = manifest
        .grant_key(component.as_ref().map(|c| &c.digest))
        .map_err(|e| {
            fail(LoadError::Io {
                path: manifest_path.clone(),
                message: format!("no grant key: {e}"),
            })
        })?;
    let roots = FsRoots {
        plugin_dir: canonical.clone(),
        data_dir: dirs.data_dir(manifest.id.as_str(), &canonical),
        home_dir: dirs.home.clone(),
    };
    Ok(DiscoveredPlugin {
        manifest: Arc::new(manifest),
        dir: canonical,
        source,
        component,
        key,
        roots,
    })
}

fn load_component(plugin_dir: &Path, relative: &str) -> Result<ComponentFile, LoadError> {
    let mut named = plugin_dir.to_path_buf();
    named.extend(relative.split('/'));
    let path = match named.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(LoadError::EntryMissing { path: named });
        }
        Err(e) => return Err(io_error(&named, &e)),
    };
    // A symlink may point anywhere; only a file inside the plugin directory is a component.
    if !path.starts_with(plugin_dir) {
        return Err(LoadError::EntryEscapes { path: named });
    }
    let meta = std::fs::metadata(&path).map_err(|e| io_error(&path, &e))?;
    if !meta.is_file() {
        return Err(LoadError::EntryMissing { path: named });
    }
    if meta.len() > MAX_COMPONENT_BYTES {
        return Err(LoadError::ComponentTooLarge {
            len: meta.len(),
            max: MAX_COMPONENT_BYTES,
        });
    }
    // The file may grow between stat and read: the read is capped too.
    let bytes = read_capped(&path, MAX_COMPONENT_BYTES).map_err(|e| io_error(&path, &e))?;
    let len = bytes.len() as u64;
    if len > MAX_COMPONENT_BYTES {
        return Err(LoadError::ComponentTooLarge {
            len,
            max: MAX_COMPONENT_BYTES,
        });
    }
    Ok(ComponentFile {
        digest: ComponentDigest::of(&bytes),
        path,
        len,
    })
}
