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
    let root = root.canonicalize().map_err(|_| missing())?;
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

fn is_dir(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_dir())
}

/// The preopens for the effective `fs.read` scopes, and the scopes that got none. Blocking I/O.
///
/// Inside a preopen the WASI layer (cap-std) refuses symlinks that leave it; this check covers the preopen root.
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
