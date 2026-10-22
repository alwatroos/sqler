// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `fs.read` scopes turned into read-only preopens, with canonicalisation and a symlink-escape check.

use std::path::{Path, PathBuf};

use sqler_plugin_api::{CapabilitySet, FsRoot, FsRoots, FsScope};

/// A directory the guest may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preopen {
    /// The scope.
    pub scope: FsScope,
    /// The canonical host directory.
    pub host: PathBuf,
    /// The path the guest sees: `/` + the scope text (`/data/cache`).
    pub guest: String,
}

/// Why a granted scope has no preopen.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScopeIssue {
    /// The target does not exist, is not a directory, or cannot be resolved.
    Missing {
        /// The scope.
        scope: FsScope,
    },
    /// The target resolves (through a symlink or a junction) outside its root.
    Escapes {
        /// The scope.
        scope: FsScope,
    },
    /// A `home/...` scope without a known home directory.
    NoHome {
        /// The scope.
        scope: FsScope,
    },
}

impl std::fmt::Display for ScopeIssue {
    /// The reason the app shows next to the plugin: what is wrong, and with which scope.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { scope } => write!(
                f,
                "fs.read:{scope} was not granted: the directory does not exist"
            ),
            Self::Escapes { scope } => write!(
                f,
                "fs.read:{scope} was not granted: the directory is a link that leaves its owner"
            ),
            Self::NoHome { scope } => {
                write!(f, "fs.read:{scope} was not granted: no home directory")
            }
        }
    }
}

fn root_dir(scope: &FsScope, roots: &FsRoots) -> Option<PathBuf> {
    match scope.root() {
        FsRoot::Plugin => Some(roots.plugin_dir.clone()),
        FsRoot::Data => Some(roots.data_dir.clone()),
        FsRoot::Home => roots.home_dir.clone(),
        // `FsRoot` is non-exhaustive: an unknown root has no directory, so it never resolves.
        _ => None,
    }
}

fn resolve_one(scope: &FsScope, roots: &FsRoots) -> Result<Preopen, ScopeIssue> {
    let missing = || ScopeIssue::Missing {
        scope: scope.clone(),
    };
    let (Some(target), Some(root)) = (scope.resolve(roots), root_dir(scope, roots)) else {
        return Err(ScopeIssue::NoHome {
            scope: scope.clone(),
        });
    };
    let root = if matches!(scope.root(), FsRoot::Home) {
        root.canonicalize().map_err(|_| missing())?
    } else {
        owned_root(&root, scope)?
    };
    let target = target.canonicalize().map_err(|_| missing())?;
    // Component-wise: `/a/bc` is not inside `/a/b`.
    if !target.starts_with(&root) {
        return Err(ScopeIssue::Escapes {
            scope: scope.clone(),
        });
    }
    if !is_dir(&target) {
        return Err(missing());
    }
    Ok(Preopen {
        scope: scope.clone(),
        host: target,
        guest: format!("/{scope}"),
    })
}

/// The canonical form of a `plugin` or `data` root, which must be a real directory sitting directly in its (canonical)
/// parent: the plugin's own directory, or the data directory the host owns for that plugin id. A root that is itself a
/// symlink or a junction, or whose canonical path is not `<canonical parent>/<name>`, is refused: a package must not
/// be able to ship `data -> ~/.ssh` and have the grant `fs.read:data` expose it. Only `home` (a place the user chose)
/// may be a link.
fn owned_root(root: &Path, scope: &FsScope) -> Result<PathBuf, ScopeIssue> {
    let missing = || ScopeIssue::Missing {
        scope: scope.clone(),
    };
    let escapes = || ScopeIssue::Escapes {
        scope: scope.clone(),
    };
    // `symlink_metadata` does not follow the last component; std reports junctions as links on Windows too.
    let meta = std::fs::symlink_metadata(root).map_err(|_| missing())?;
    if meta.file_type().is_symlink() {
        return Err(escapes());
    }
    let canonical = root.canonicalize().map_err(|_| missing())?;
    // A bare `..` or `/` has no name to compare: it is not a directory the host or the package put there.
    let (Some(parent), Some(_)) = (root.parent(), root.file_name()) else {
        return Err(escapes());
    };
    let parent = parent.canonicalize().map_err(|_| missing())?;
    if canonical.parent() != Some(parent.as_path()) {
        return Err(escapes());
    }
    Ok(canonical)
}

fn is_dir(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_dir())
}

/// The preopens for the effective `fs.read` scopes, and the scopes that got none. Blocking I/O.
///
/// Inside a preopen the WASI layer (cap-std) refuses symlinks that leave it; this check covers the preopen root and
/// the roots themselves: a `plugin` or `data` root that is a link, or that does not sit directly in its parent, is
/// [`ScopeIssue::Escapes`] and the scope gets no preopen.
#[must_use]
pub fn resolve_preopens(
    effective: &CapabilitySet,
    roots: &FsRoots,
) -> (Vec<Preopen>, Vec<ScopeIssue>) {
    let (mut ok, mut issues) = (Vec::new(), Vec::new());
    for scope in effective.fs_scopes() {
        match resolve_one(scope, roots) {
            Ok(p) => ok.push(p),
            Err(i) => issues.push(i),
        }
    }
    (ok, issues)
}
