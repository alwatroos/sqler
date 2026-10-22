// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Discovery: directory rules, validation, digest and grant key, hostile manifests.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use sqler_plugin_api::ComponentDigest;
use sqler_plugin_host::{
    LoadError, MAX_COMPONENT_BYTES, MAX_ENTRIES_PER_ROOT, PluginDirs, PluginSource, discover,
};

/// Fresh per-test directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p =
            std::env::temp_dir().join(format!("sqler-plugin-host-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manifest(id: &str, entry: Option<&str>) -> String {
    let mut s = format!(
        "[plugin]\nid = \"{id}\"\nname = \"Test\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = []\n"
    );
    if let Some(e) = entry {
        s.push_str("\n[entry]\ncomponent = \"");
        s.push_str(e);
        s.push_str("\"\n");
    }
    s
}

/// Writes `<root>/<dir>/sqler.toml` (and the component when given).
fn plugin(root: &Path, dir: &str, id: &str, entry: Option<(&str, &[u8])>) -> PathBuf {
    let d = root.join(dir);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("sqler.toml"), manifest(id, entry.map(|e| e.0))).unwrap();
    if let Some((name, bytes)) = entry {
        fs::write(d.join(name), bytes).unwrap();
    }
    d
}

fn config(root: &Path) -> PluginDirs {
    PluginDirs {
        config_plugins: Some(root.to_path_buf()),
        ..PluginDirs::default()
    }
}

/// Creates a symlink; `None` (with a message) where the OS forbids it.
fn symlink(target: &Path, link: &Path) -> Option<()> {
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let r = if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match r {
        Ok(()) => Some(()),
        Err(e) => {
            println!("skipped: cannot create symlinks here ({e})");
            None
        }
    }
}

#[test]
fn valid_plugins_with_and_without_component() {
    let s = Scratch::new("valid");
    plugin(&s.0, "alpha", "alpha", None);
    plugin(&s.0, "beta", "beta", Some(("beta.wasm", b"\0asm-bytes")));
    let d = discover(&config(&s.0));
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let ids: Vec<_> = d.plugins.iter().map(|p| p.manifest.id.as_str()).collect();
    assert_eq!(ids, ["alpha", "beta"]);
    let alpha = &d.plugins[0];
    assert!(alpha.component.is_none());
    assert_eq!(alpha.key.component, None);
    assert_eq!(alpha.source, PluginSource::Config);
    assert_eq!(alpha.dir, s.path("alpha").canonicalize().unwrap());
    let beta = &d.plugins[1];
    let c = beta.component.as_ref().unwrap();
    assert_eq!(c.digest, ComponentDigest::of(b"\0asm-bytes"));
    assert_eq!(c.len, 10);
    assert_eq!(beta.key.component, Some(c.digest));
    assert_eq!(beta.roots.plugin_dir, beta.dir);
    assert_eq!(beta.roots.data_dir, s.0.join("beta").join("data"));
}

#[test]
fn digest_and_key_change_with_the_bytes() {
    let s = Scratch::new("digest");
    plugin(&s.0, "beta", "beta", Some(("beta.wasm", b"one")));
    let first = discover(&config(&s.0)).plugins.remove(0);
    fs::write(s.path("beta/beta.wasm"), b"two").unwrap();
    let second = discover(&config(&s.0)).plugins.remove(0);
    assert_ne!(first.key, second.key);
    assert_eq!(first.key.hash, second.key.hash);
    assert_eq!(first.manifest.hash(), second.manifest.hash());
}

#[test]
fn bad_manifest_next_to_good_ones() {
    let s = Scratch::new("badnext");
    plugin(&s.0, "aaa-good", "aaa-good", None);
    let bad = s.path("bbb-bad");
    fs::create_dir_all(&bad).unwrap();
    fs::write(bad.join("sqler.toml"), "this is = not [valid").unwrap();
    plugin(&s.0, "ccc-good", "ccc-good", None);
    let d = discover(&config(&s.0));
    assert_eq!(d.plugins.len(), 2);
    assert_eq!(d.problems.len(), 1);
    assert!(matches!(d.problems[0].error, LoadError::Manifest(_)));
    assert!(d.problems[0].dir.ends_with("bbb-bad"));
    assert_eq!(d.problems[0].id, None);
}

#[test]
fn incompatible_api_is_reported() {
    let s = Scratch::new("api");
    let d = plugin(&s.0, "newer", "newer", None);
    let text = manifest("newer", None).replace("api = \"0.1\"", "api = \"0.2\"");
    fs::write(d.join("sqler.toml"), text).unwrap();
    let out = discover(&config(&s.0));
    assert!(out.plugins.is_empty());
    let p = &out.problems[0];
    assert!(matches!(p.error, LoadError::Incompatible(_)));
    assert!(p.error.is_incompatible());
    assert_eq!(p.id.as_ref().unwrap().as_str(), "newer");
}

#[test]
fn directory_name_must_equal_the_id() {
    let s = Scratch::new("dirname");
    plugin(&s.0, "wrong-name", "right-name", None);
    let d = discover(&config(&s.0));
    assert!(d.plugins.is_empty());
    assert!(
        matches!(&d.problems[0].error, LoadError::DirName { dir_name } if dir_name == "wrong-name")
    );
}

#[test]
fn a_single_dev_path_is_exempt_from_the_name_rule() {
    let s = Scratch::new("devsingle");
    plugin(&s.0, "checkout", "my-plugin", None);
    let d = discover(&PluginDirs {
        dev: vec![s.path("checkout")],
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    assert_eq!(d.plugins[0].manifest.id.as_str(), "my-plugin");
    assert_eq!(d.plugins[0].source, PluginSource::Dev);
}

#[test]
fn a_dev_path_without_manifest_is_scanned_for_children() {
    let s = Scratch::new("devscan");
    plugin(&s.0, "one", "one", None);
    plugin(&s.0, "two", "two", None);
    let d = discover(&PluginDirs {
        dev: vec![s.0.clone()],
        ..PluginDirs::default()
    });
    assert_eq!(d.plugins.len(), 2);
    // A scanned child must match its id.
    plugin(&s.0, "three", "not-three", None);
    let d = discover(&PluginDirs {
        dev: vec![s.0.clone()],
        ..PluginDirs::default()
    });
    assert_eq!(d.plugins.len(), 2);
    assert_eq!(d.problems.len(), 1);
}

#[test]
fn dev_paths_win_over_the_config_dir_for_duplicate_ids() {
    let s = Scratch::new("dup");
    let dev = plugin(&s.0, "dev/work", "same", None);
    plugin(&s.0, "cfg/same", "same", None);
    plugin(&s.0, "cfg/other", "other", None);
    let d = discover(&PluginDirs {
        config_plugins: Some(s.path("cfg")),
        dev: vec![dev.clone()],
        home: None,
    });
    let same: Vec<_> = d
        .plugins
        .iter()
        .filter(|p| p.manifest.id.as_str() == "same")
        .collect();
    assert_eq!(same.len(), 1);
    assert_eq!(same[0].source, PluginSource::Dev);
    assert_eq!(d.plugins.len(), 2);
    assert_eq!(d.problems.len(), 1);
    match &d.problems[0].error {
        LoadError::Duplicate { first } => assert_eq!(first, &dev.canonicalize().unwrap()),
        e => panic!("{e:?}"),
    }
    // Two dev paths: the earlier one wins.
    let other = plugin(&s.0, "dev2/work", "same", None);
    let d = discover(&PluginDirs {
        dev: vec![other.clone(), dev],
        ..PluginDirs::default()
    });
    assert_eq!(d.plugins[0].dir, other.canonicalize().unwrap());
    assert_eq!(d.problems.len(), 1);
}

#[test]
fn data_dirs_hidden_entries_and_files_are_skipped_silently() {
    let s = Scratch::new("skip");
    plugin(&s.0, "real", "real", None);
    fs::create_dir_all(s.path("real/data")).unwrap();
    fs::create_dir_all(s.path("just-data/data")).unwrap();
    plugin(&s.0, ".hidden", ".hidden", None);
    plugin(&s.0, ".git", "gitplug", None);
    fs::write(s.path("README.txt"), "x").unwrap();
    let d = discover(&config(&s.0));
    assert_eq!(d.plugins.len(), 1);
    assert!(d.problems.is_empty(), "{:?}", d.problems);
}

#[test]
fn missing_root_is_not_a_problem_but_a_file_root_is() {
    let s = Scratch::new("roots");
    let d = discover(&PluginDirs {
        config_plugins: Some(s.path("nope")),
        dev: vec![s.path("also-nope")],
        home: None,
    });
    assert!(d.plugins.is_empty() && d.problems.is_empty());
    fs::write(s.path("file"), "x").unwrap();
    let d = discover(&config(&s.path("file")));
    assert_eq!(d.problems.len(), 1);
    assert!(matches!(d.problems[0].error, LoadError::Io { .. }));
}

#[test]
fn too_many_entries_reports_once_and_stops_at_the_cap() {
    let s = Scratch::new("many");
    for i in 0..(MAX_ENTRIES_PER_ROOT + 5) {
        let id = format!("p{i:04}");
        plugin(&s.0, &id, &id, None);
    }
    let d = discover(&config(&s.0));
    assert_eq!(d.plugins.len(), MAX_ENTRIES_PER_ROOT);
    assert_eq!(d.problems.len(), 1);
    assert!(matches!(
        d.problems[0].error,
        LoadError::TooManyEntries {
            max: MAX_ENTRIES_PER_ROOT
        }
    ));
}

#[test]
fn entry_missing() {
    let s = Scratch::new("entrymissing");
    let d = plugin(&s.0, "gone", "gone", Some(("gone.wasm", b"x")));
    fs::remove_file(d.join("gone.wasm")).unwrap();
    let out = discover(&config(&s.0));
    assert!(matches!(
        out.problems[0].error,
        LoadError::EntryMissing { .. }
    ));
    // A directory is not a component.
    fs::create_dir(d.join("gone.wasm")).unwrap();
    let out = discover(&config(&s.0));
    assert!(matches!(
        out.problems[0].error,
        LoadError::EntryMissing { .. }
    ));
}

#[test]
fn entry_escaping_through_a_symlink() {
    let s = Scratch::new("entryescape");
    let outside = s.path("outside.wasm");
    fs::write(&outside, b"secret").unwrap();
    let d = plugin(&s.0, "cfg/evil", "evil", Some(("evil.wasm", b"x")));
    fs::remove_file(d.join("evil.wasm")).unwrap();
    if symlink(&outside, &d.join("evil.wasm")).is_none() {
        return;
    }
    let out = discover(&config(&s.path("cfg")));
    assert!(out.plugins.is_empty());
    assert!(matches!(
        out.problems[0].error,
        LoadError::EntryEscapes { .. }
    ));
}

#[test]
fn entry_through_a_symlink_inside_the_plugin_dir_is_fine() {
    let s = Scratch::new("entryinside");
    let d = plugin(&s.0, "ok", "ok", Some(("real.wasm", b"x")));
    fs::write(d.join("alias-target.wasm"), b"inner").unwrap();
    fs::write(d.join("sqler.toml"), manifest("ok", Some("alias.wasm"))).unwrap();
    if symlink(&d.join("alias-target.wasm"), &d.join("alias.wasm")).is_none() {
        return;
    }
    let out = discover(&config(&s.0));
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    assert_eq!(
        out.plugins[0].component.as_ref().unwrap().digest,
        ComponentDigest::of(b"inner")
    );
}

#[test]
fn symlinked_plugin_directory_is_canonicalised() {
    let s = Scratch::new("dirlink");
    let real = plugin(&s.0, "store/real", "linked", None);
    fs::create_dir_all(s.path("cfg")).unwrap();
    if symlink(&real, &s.path("cfg/linked")).is_none() {
        return;
    }
    let out = discover(&config(&s.path("cfg")));
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    assert_eq!(out.plugins[0].dir, real.canonicalize().unwrap());
}

#[test]
fn component_too_large() {
    let s = Scratch::new("big");
    let d = plugin(&s.0, "big", "big", Some(("big.wasm", b"")));
    // A sparse file: no disk is used where the file system supports holes.
    let f = fs::OpenOptions::new()
        .write(true)
        .open(d.join("big.wasm"))
        .unwrap();
    f.set_len(MAX_COMPONENT_BYTES + 1).unwrap();
    drop(f);
    let out = discover(&config(&s.0));
    assert!(out.plugins.is_empty());
    assert_eq!(
        out.problems[0].error,
        LoadError::ComponentTooLarge {
            len: MAX_COMPONENT_BYTES + 1,
            max: MAX_COMPONENT_BYTES
        }
    );
}

#[test]
fn component_at_the_limit_is_accepted_only_if_not_over() {
    let s = Scratch::new("edge");
    let d = plugin(&s.0, "edge", "edge", Some(("edge.wasm", b"")));
    let f = fs::OpenOptions::new()
        .write(true)
        .open(d.join("edge.wasm"))
        .unwrap();
    f.set_len(1 << 20).unwrap();
    drop(f);
    let out = discover(&config(&s.0));
    assert_eq!(out.plugins.len(), 1);
    assert_eq!(out.plugins[0].component.as_ref().unwrap().len, 1 << 20);
}

#[test]
fn hostile_manifests() {
    let s = Scratch::new("hostile");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("huge", vec![b'#'; 70_000]),
        ("binary", vec![0xff, 0xfe, 0x00, 0x01]),
        (
            "deep",
            format!("{}1{}", "[".repeat(200), "]".repeat(200)).into_bytes(),
        ),
        ("empty", Vec::new()),
        (
            "traversal",
            manifest("traversal", Some("../x"))
                .replace("../x", "../x.wasm")
                .into_bytes(),
        ),
        (
            "absolute",
            manifest("absolute", Some("/etc/passwd.wasm")).into_bytes(),
        ),
        (
            "backslash",
            manifest("backslash", Some("..\\\\x.wasm")).into_bytes(),
        ),
        (
            "unknown",
            format!("{}\n[evil]\nx = 1\n", manifest("unknown", None)).into_bytes(),
        ),
        ("nul", b"[plugin]\nid = \"nul\"\x00\n".to_vec()),
    ];
    for (name, bytes) in &cases {
        let d = s.path(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("sqler.toml"), bytes).unwrap();
    }
    plugin(&s.0, "zzz-ok", "zzz-ok", None);
    let out = discover(&config(&s.0));
    assert_eq!(out.plugins.len(), 1, "{:?}", out.plugins.len());
    assert_eq!(out.problems.len(), cases.len());
    for p in &out.problems {
        assert!(
            matches!(p.error, LoadError::Manifest(_)),
            "{}: {:?}",
            p.dir.display(),
            p.error
        );
    }
}

#[test]
fn a_manifest_symlinked_to_an_endless_file_is_bounded() {
    #[cfg(unix)]
    {
        let s = Scratch::new("endless");
        let d = s.path("endless");
        fs::create_dir_all(&d).unwrap();
        if symlink(Path::new("/dev/zero"), &d.join("sqler.toml")).is_none() {
            return;
        }
        let out = discover(&config(&s.0));
        // /dev/zero is not a regular file: skipped like any directory without a manifest, never read to the end.
        assert!(out.plugins.is_empty());
    }
}
