// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `fs.read` scope resolution: roots, canonicalisation, symlink escapes.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use sqler_plugin_api::{Capability, CapabilitySet, FsRoots, FsScope};
use sqler_plugin_host::{ScopeIssue, resolve_preopens};

/// Fresh per-test directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "sqler-plugin-host-scope-{}-{test}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        // Canonical, so comparisons hold where the temp dir is itself a link (macOS /var, Windows 8.3 names).
        Self(p.canonicalize().unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn symlink(target: &Path, link: &Path) -> Option<()> {
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_dir(target, link);
    match r {
        Ok(()) => Some(()),
        Err(e) => {
            println!("skipped: cannot create symlinks here ({e})");
            None
        }
    }
}

fn caps(scopes: &[&str]) -> CapabilitySet {
    let mut c = CapabilitySet::new();
    for s in scopes {
        c.insert(Capability::FsRead(s.parse().unwrap()));
    }
    c
}

fn scope(s: &str) -> FsScope {
    s.parse().unwrap()
}

fn roots(s: &Scratch) -> FsRoots {
    for d in ["plugin", "data/cache", "home/docs"] {
        fs::create_dir_all(s.0.join(d)).unwrap();
    }
    FsRoots {
        plugin_dir: s.0.join("plugin"),
        data_dir: s.0.join("data"),
        home_dir: Some(s.0.join("home")),
    }
}

#[test]
fn in_root_scopes_resolve_to_canonical_dirs_with_guest_paths() {
    let s = Scratch::new("inroot");
    let r = roots(&s);
    let (ok, issues) = resolve_preopens(&caps(&["data", "data/cache", "plugin", "home/docs"]), &r);
    assert!(issues.is_empty(), "{issues:?}");
    let by_guest: Vec<_> = ok
        .iter()
        .map(|p| (p.guest.as_str(), p.host.clone()))
        .collect();
    assert!(by_guest.contains(&("/data", s.0.join("data"))));
    assert!(by_guest.contains(&("/data/cache", s.0.join("data/cache"))));
    assert!(by_guest.contains(&("/plugin", s.0.join("plugin"))));
    assert!(by_guest.contains(&("/home/docs", s.0.join("home/docs"))));
    assert_eq!(ok.len(), 4);
    assert!(ok.iter().any(|p| p.scope == scope("data/cache")));
}

#[test]
fn missing_targets_and_files_are_missing() {
    let s = Scratch::new("missing");
    let r = roots(&s);
    fs::write(s.0.join("data/file.txt"), "x").unwrap();
    let (ok, issues) = resolve_preopens(&caps(&["data/nope", "data/file.txt"]), &r);
    assert_eq!(ok, []);
    assert_eq!(issues.len(), 2);
    assert!(
        issues
            .iter()
            .all(|i| matches!(i, ScopeIssue::Missing { .. }))
    );
    // A missing root is missing too.
    let mut r2 = r;
    r2.data_dir = s.0.join("no-such-data");
    let (ok, issues) = resolve_preopens(&caps(&["data"]), &r2);
    assert_eq!(ok, []);
    assert_eq!(
        issues,
        [ScopeIssue::Missing {
            scope: scope("data")
        }]
    );
}

#[test]
fn home_scope_without_a_home_dir() {
    let s = Scratch::new("nohome");
    let mut r = roots(&s);
    r.home_dir = None;
    let (ok, issues) = resolve_preopens(&caps(&["home/docs", "data"]), &r);
    assert_eq!(ok.len(), 1);
    assert_eq!(
        issues,
        [ScopeIssue::NoHome {
            scope: scope("home/docs")
        }]
    );
}

#[test]
fn symlink_inside_the_root_pointing_outside_escapes() {
    let s = Scratch::new("escape");
    let r = roots(&s);
    fs::create_dir_all(s.0.join("outside")).unwrap();
    if symlink(&s.0.join("outside"), &s.0.join("data/link")).is_none() {
        return;
    }
    let (ok, issues) = resolve_preopens(&caps(&["data/link"]), &r);
    assert_eq!(ok, []);
    assert_eq!(
        issues,
        [ScopeIssue::Escapes {
            scope: scope("data/link")
        }]
    );
}

#[test]
fn symlink_to_the_parent_escapes() {
    let s = Scratch::new("parent");
    let r = roots(&s);
    if symlink(&s.0, &s.0.join("data/up")).is_none() {
        return;
    }
    let (_, issues) = resolve_preopens(&caps(&["data/up"]), &r);
    assert_eq!(
        issues,
        [ScopeIssue::Escapes {
            scope: scope("data/up")
        }]
    );
}

#[test]
fn symlink_inside_the_root_pointing_inside_is_fine() {
    let s = Scratch::new("inside");
    let r = roots(&s);
    if symlink(&s.0.join("data/cache"), &s.0.join("data/alias")).is_none() {
        return;
    }
    let (ok, issues) = resolve_preopens(&caps(&["data/alias"]), &r);
    assert!(issues.is_empty(), "{issues:?}");
    // The preopen is the canonical target, not the link.
    assert_eq!(ok[0].host, s.0.join("data/cache"));
    assert_eq!(ok[0].guest, "/data/alias");
}

#[test]
fn a_root_that_is_itself_a_symlink_is_followed() {
    let s = Scratch::new("rootlink");
    let mut r = roots(&s);
    fs::create_dir_all(s.0.join("real-data/cache")).unwrap();
    if symlink(&s.0.join("real-data"), &s.0.join("linked-data")).is_none() {
        return;
    }
    r.data_dir = s.0.join("linked-data");
    let (ok, issues) = resolve_preopens(&caps(&["data/cache"]), &r);
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(ok[0].host, s.0.join("real-data/cache"));
}

#[test]
fn a_sibling_with_a_common_prefix_is_not_inside_the_root() {
    let s = Scratch::new("prefix");
    let r = roots(&s);
    // `data-evil` starts with the text of `data` but is not a path component below it.
    fs::create_dir_all(s.0.join("data-evil")).unwrap();
    if symlink(&s.0.join("data-evil"), &s.0.join("data/evil")).is_none() {
        return;
    }
    let (_, issues) = resolve_preopens(&caps(&["data/evil"]), &r);
    assert_eq!(
        issues,
        [ScopeIssue::Escapes {
            scope: scope("data/evil")
        }]
    );
}

#[test]
fn no_fs_capability_means_no_preopen() {
    let s = Scratch::new("none");
    let r = roots(&s);
    let mut c = CapabilitySet::new();
    c.insert(Capability::DbRead);
    let (ok, issues) = resolve_preopens(&c, &r);
    assert!(ok.is_empty() && issues.is_empty());
}
