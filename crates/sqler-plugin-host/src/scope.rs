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
        simplify(&root.canonicalize().map_err(|_| missing())?)
    } else {
        owned_root(&root, scope)?
    };
    let target = target.canonicalize().map_err(|_| missing())?;
    // Both sides in the same (non-verbatim) spelling. Component-wise: `/a/bc` is not inside `/a/b`. The comparison is
    // exact, also on Windows: both sides come from `canonicalize`, which reports the on-disk case and long names, and a
    // case-insensitive match could equate two directories of a case-sensitive Windows directory.
    if !simplify(&target).starts_with(&root) {
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

/// Strips the Windows verbatim prefix (`\\?\C:\x` -> `C:\x`, `\\?\UNC\h\s` -> `\\h\s`) so two spellings of one path compare
/// equal. `Err` for the other namespaces (`\\.\` devices, `\\?\GLOBALROOT`, `\\?\Volume{..}`, `//?/`, `//./`): a root in
/// one of those is not a directory the host built. Pure text, so it behaves the same on every OS (on Unix such a
/// path is refused too: no real root starts like that).
fn strip_verbatim(s: &str) -> Result<String, ()> {
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return Ok(format!(r"\\{unc}"));
        }
        let b = rest.as_bytes();
        let disk = b.len() >= 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b.len() == 2 || b[2] == b'\\');
        return if disk { Ok(rest.to_owned()) } else { Err(()) };
    }
    if s.starts_with(r"\\.\") || s.starts_with("//?/") || s.starts_with("//./") {
        return Err(());
    }
    Ok(s.to_owned())
}

/// `p` without a verbatim prefix; unchanged if it has none, is not UTF-8, or is in another namespace.
fn simplify(p: &Path) -> PathBuf {
    p.to_str()
        .and_then(|s| strip_verbatim(s).ok())
        .map_or_else(|| p.to_path_buf(), PathBuf::from)
}

fn is_drive(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// DOS device names, which Windows resolves in any directory, with or without an extension.
fn is_device_name(seg: &str) -> bool {
    let stem = seg
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|p| {
        stem.strip_prefix(p).is_some_and(|n| {
            matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                || matches!(n, "\u{b9}" | "\u{b2}" | "\u{b3}")
        })
    })
}

/// The lexical rules for a `plugin` or `data` root, checked on the text before any file system call and the same on
/// every OS (`windows` selects the Windows-only rules so they can be tested everywhere). Returns the root with a
/// verbatim prefix stripped, or `None` if refused:
/// - the text splits at `/` and `\` alike (a verbatim path treats `/` as a name character, so `\\?\C:\d/..` is not
///   what it seems to `Path::components`);
/// - no `..` segment anywhere, and a last segment that is a real name (not `.`, not a bare drive or the file system
///   root, not empty);
/// - on Windows also: no `:` in a name (alternate data streams), no trailing dot or space (Windows drops them, so
///   `data.` is `data`), no DOS device name, no `\\.\`/`\\?\GLOBALROOT` namespaces.
fn lexical_root(root: &str, windows: bool) -> Option<String> {
    let simple = strip_verbatim(root).ok()?;
    let segs: Vec<&str> = simple
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect();
    let last = *segs.last()?;
    if segs.contains(&"..") || last == "." || (windows && is_drive(last)) {
        return None;
    }
    if windows {
        for (i, seg) in segs.iter().enumerate() {
            let drive = i == 0 && is_drive(seg);
            if (!drive && seg.contains(':')) || is_device_name(seg) {
                return None;
            }
            if *seg != "." && (seg.ends_with('.') || seg.ends_with(' ')) {
                return None;
            }
        }
    }
    Some(simple)
}

/// The canonical form of a `plugin` or `data` root, which must be a real directory sitting directly in its (canonical)
/// parent: the plugin's own directory, or the data directory the host owns for that plugin id. A root that is itself a
/// symlink, a junction or any other reparse point, that fails the [`lexical_root`] rules, or whose canonical path is
/// not `<canonical parent>/<name>`, is refused: a package must not be able to ship `data -> ~/.ssh` and have the grant
/// `fs.read:data` expose it. Only `home` (a place the user chose) may be a link. The returned path has no verbatim
/// prefix.
fn owned_root(root: &Path, scope: &FsScope) -> Result<PathBuf, ScopeIssue> {
    let missing = || ScopeIssue::Missing {
        scope: scope.clone(),
    };
    let escapes = || ScopeIssue::Escapes {
        scope: scope.clone(),
    };
    // A path without a name, with `..`, or in a foreign namespace is not a directory the host or the package put there.
    let text = root.to_str().ok_or_else(escapes)?;
    let simple = PathBuf::from(lexical_root(text, cfg!(windows)).ok_or_else(escapes)?);
    // `symlink_metadata` does not follow the last component; std reports junctions as links on Windows too.
    let meta = std::fs::symlink_metadata(&simple).map_err(|_| missing())?;
    if meta.file_type().is_symlink() || is_reparse_point(&meta) {
        return Err(escapes());
    }
    let canonical = simplify(&simple.canonicalize().map_err(|_| missing())?);
    let parent = simple.parent().ok_or_else(escapes)?;
    let parent = simplify(&parent.canonicalize().map_err(|_| missing())?);
    if canonical.parent() != Some(parent.as_path()) {
        return Err(escapes());
    }
    Ok(canonical)
}

/// Windows: any reparse point (symlink, junction, mount point, cloud placeholder) is a link as far as the sandbox cares.
#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_stripped_or_refused() {
        assert_eq!(strip_verbatim(r"\\?\C:\a\b").as_deref(), Ok(r"C:\a\b"));
        assert_eq!(strip_verbatim(r"\\?\UNC\h\s\a").as_deref(), Ok(r"\\h\s\a"));
        assert_eq!(strip_verbatim("/tmp/a").as_deref(), Ok("/tmp/a"));
        for bad in [
            r"\\.\C:\a",
            r"\\?\GLOBALROOT\Device\x",
            r"\\?\Volume{1}\a",
            "//?/C:/a",
            "//./C:/a",
        ] {
            assert!(strip_verbatim(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn roots_without_a_name_or_with_dotdot_are_refused_on_every_os() {
        for bad in [
            "/tmp/x/..",
            "/tmp/x/../y",
            "/tmp/x/.",
            "/",
            "",
            r"C:\x\..",
            r"\\?\C:\x/..",
            r"\\?\C:\x\..",
            r"\\?\C:\x/../y",
        ] {
            assert_eq!(lexical_root(bad, false), None, "{bad}");
            assert_eq!(lexical_root(bad, true), None, "{bad} (windows rules)");
        }
        assert_eq!(
            lexical_root("/tmp/x/data", false).as_deref(),
            Some("/tmp/x/data")
        );
        assert_eq!(
            lexical_root(r"\\?\C:\x\data", true).as_deref(),
            Some(r"C:\x\data")
        );
    }

    #[test]
    fn windows_rules_refuse_streams_trailing_dots_and_devices() {
        for bad in [
            r"C:\x\data:stream",
            r"C:\x\data::$DATA",
            r"C:\x\data.",
            r"C:\x\data ",
            r"C:\x\NUL",
            r"C:\x\nul.txt",
            r"C:\x\COM1",
            r"C:\x\con \data",
            r"C:\x\LPT9.d",
        ] {
            assert_eq!(lexical_root(bad, true), None, "{bad}");
        }
        for ok in [r"C:\x\data", r"C:\x\com10", r"C:\x\console", r"D:/x/data"] {
            assert!(lexical_root(ok, true).is_some(), "{ok}");
        }
        // Colons and trailing dots are plain names elsewhere.
        assert!(lexical_root("/x/a:b", false).is_some());
        assert!(lexical_root("/x/a.", false).is_some());
    }
}
