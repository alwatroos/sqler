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

fn escapes(s: &str) -> ScopeIssue {
    ScopeIssue::Escapes { scope: scope(s) }
}

#[test]
fn a_data_root_that_is_itself_a_symlink_is_refused() {
    // A package ships `data -> <somewhere else>`; `fs.read:data` must not expose it.
    let s = Scratch::new("rootlink");
    let mut r = roots(&s);
    fs::create_dir_all(s.0.join("real-data/cache")).unwrap();
    if symlink(&s.0.join("real-data"), &s.0.join("linked-data")).is_none() {
        return;
    }
    r.data_dir = s.0.join("linked-data");
    let (ok, issues) = resolve_preopens(&caps(&["data", "data/cache"]), &r);
    assert_eq!(ok, []);
    assert_eq!(issues, [escapes("data"), escapes("data/cache")]);
}

#[test]
fn data_pointing_at_an_unrelated_directory_is_refused() {
    // `data -> /etc` in spirit: a sibling tree the package has no business with. Uses a real second tree, `/etc` itself
    // on Unix, so the test also holds for a directory that certainly exists.
    let s = Scratch::new("data-etc");
    let mut r = roots(&s);
    fs::remove_dir_all(s.0.join("data")).unwrap();
    let target = if cfg!(unix) {
        PathBuf::from("/etc")
    } else {
        fs::create_dir_all(s.0.join("elsewhere")).unwrap();
        s.0.join("elsewhere")
    };
    if symlink(&target, &s.0.join("data")).is_none() {
        return;
    }
    r.data_dir = s.0.join("data");
    let (ok, issues) = resolve_preopens(&caps(&["data"]), &r);
    assert_eq!(ok, []);
    assert_eq!(issues, [escapes("data")]);
}

#[test]
fn a_plugin_root_that_is_a_symlink_is_refused() {
    let s = Scratch::new("pluginlink");
    let mut r = roots(&s);
    fs::create_dir_all(s.0.join("other")).unwrap();
    if symlink(&s.0.join("other"), &s.0.join("linked-plugin")).is_none() {
        return;
    }
    r.plugin_dir = s.0.join("linked-plugin");
    let (ok, issues) = resolve_preopens(&caps(&["plugin"]), &r);
    assert_eq!(ok, []);
    assert_eq!(issues, [escapes("plugin")]);
}

#[test]
fn a_subdirectory_of_the_plugin_pointing_to_its_parent_escapes() {
    let s = Scratch::new("plugin-sub");
    let r = roots(&s);
    if symlink(&s.0, &s.0.join("plugin/sub")).is_none() {
        return;
    }
    let (ok, issues) = resolve_preopens(&caps(&["plugin/sub", "plugin"]), &r);
    assert_eq!(issues, [escapes("plugin/sub")]);
    // The plugin directory itself is still fine.
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].host, s.0.join("plugin"));
}

#[test]
fn a_data_root_below_a_symlinked_ancestor_is_fine() {
    // The parent may be reached through a link (macOS `/var`, a user's linked config directory): only the root itself
    // must be a real directory.
    let s = Scratch::new("ancestor");
    let mut r = roots(&s);
    if symlink(&s.0, &s.0.join("alias")).is_none() {
        return;
    }
    r.data_dir = s.0.join("alias/data");
    let (ok, issues) = resolve_preopens(&caps(&["data/cache"]), &r);
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(ok[0].host, s.0.join("data/cache"));
}

#[test]
fn a_data_root_that_is_a_file_is_missing() {
    let s = Scratch::new("data-file");
    let mut r = roots(&s);
    fs::write(s.0.join("data-file"), "x").unwrap();
    r.data_dir = s.0.join("data-file");
    let (ok, issues) = resolve_preopens(&caps(&["data"]), &r);
    assert_eq!(ok, []);
    assert_eq!(
        issues,
        [ScopeIssue::Missing {
            scope: scope("data")
        }]
    );
}

#[test]
fn a_root_without_a_name_is_refused() {
    let s = Scratch::new("dotdot");
    let mut r = roots(&s);
    r.data_dir = s.0.join("data/..");
    let (ok, issues) = resolve_preopens(&caps(&["data"]), &r);
    assert_eq!(ok, []);
    assert_eq!(issues, [escapes("data")]);
}

#[test]
fn a_home_root_may_be_a_link() {
    // `home` is a place the user chose; only plugin and data are the package's or the host's own.
    let s = Scratch::new("homelink");
    let mut r = roots(&s);
    fs::create_dir_all(s.0.join("real-home/docs")).unwrap();
    if symlink(&s.0.join("real-home"), &s.0.join("linked-home")).is_none() {
        return;
    }
    r.home_dir = Some(s.0.join("linked-home"));
    let (ok, issues) = resolve_preopens(&caps(&["home/docs"]), &r);
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(ok[0].host, s.0.join("real-home/docs"));
}

#[cfg(windows)]
#[test]
fn a_junction_as_the_data_root_is_refused() {
    let s = Scratch::new("junction");
    let mut r = roots(&s);
    fs::create_dir_all(s.0.join("elsewhere")).unwrap();
    let junction = s.0.join("junction-data");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(s.0.join("elsewhere"))
        .output();
    if !made.is_ok_and(|o| o.status.success()) {
        println!("skipped: cannot create junctions here");
        return;
    }
    r.data_dir = junction;
    let (ok, issues) = resolve_preopens(&caps(&["data"]), &r);
    assert_eq!(ok, []);
    assert_eq!(issues, [escapes("data")]);
}

#[test]
fn issues_have_a_reason_the_app_can_show() {
    let text = escapes("data").to_string();
    assert!(
        text.contains("fs.read:data") && text.contains("not granted"),
        "{text}"
    );
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
