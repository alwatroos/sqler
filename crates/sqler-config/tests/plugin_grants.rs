// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! File, permission, corruption and property tests for the plugin grant store.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use proptest::prelude::*;
use sqler_config::{
    ConfigDir, ConfigError, GrantDecision, GrantRecord, GrantsLoad, MAX_PLUGIN_GRANTS, PluginGrants,
};

/// Fresh per-test directory (not created), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p =
            std::env::temp_dir().join(format!("sqler-config-{}-grants-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        Self(p)
    }
    fn dir(&self) -> ConfigDir {
        ConfigDir::new(&self.0)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const H1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const H2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn key(id: &str, version: &str, component: &str) -> String {
    format!("{id}@{version}#{H1}+{component}")
}

fn sample() -> PluginGrants {
    let mut g = PluginGrants::new();
    g.record(
        GrantRecord::approved(
            key("acme-sync", "1.2.0", H2),
            vec!["net.http:*.example.com".into(), "db.read".into()],
        )
        .unwrap(),
    );
    g.record(GrantRecord::denied(key("other", "0.1.0", H2)).unwrap());
    g
}

fn write_raw(s: &Scratch, bytes: &[u8]) {
    fs::create_dir_all(&s.0).unwrap();
    fs::write(s.dir().plugin_grants_path(), bytes).unwrap();
}

#[test]
fn missing_then_round_trip() {
    let s = Scratch::new("round");
    let d = s.dir();
    assert_eq!(d.load_plugin_grants(), GrantsLoad::Missing);
    d.save_plugin_grants(&sample()).unwrap();
    assert_eq!(d.load_plugin_grants(), GrantsLoad::Loaded(sample()));
    assert!(
        fs::read_to_string(d.plugin_grants_path())
            .unwrap()
            .starts_with("version = 1\n")
    );
}

#[test]
fn swapped_component_or_changed_capabilities_miss_the_grant() {
    let s = Scratch::new("miss");
    let d = s.dir();
    d.save_plugin_grants(&sample()).unwrap();
    let GrantsLoad::Loaded(g) = d.load_plugin_grants() else {
        panic!("should load")
    };
    let rec = g.for_plugin("acme-sync").unwrap();
    assert_eq!(rec.key(), key("acme-sync", "1.2.0", H2));
    assert_eq!(rec.decision(), GrantDecision::Approved);
    // A swapped component digest is another key.
    assert_ne!(rec.key(), key("acme-sync", "1.2.0", H1));
    // A changed capability set is another manifest hash, hence another key.
    let changed = format!("acme-sync@1.2.0#{H2}+{H2}");
    assert_ne!(rec.key(), changed);
    // The old approval is replaced, never merged, by the new record.
    let mut g2 = g.clone();
    g2.record(GrantRecord::denied(changed.clone()).unwrap());
    assert_eq!(g2.len(), g.len());
    assert_eq!(g2.for_plugin("acme-sync").unwrap().key(), changed);
    let denied = g2.for_plugin("acme-sync").unwrap();
    assert_eq!(denied.capabilities().len(), 0);
}

#[test]
fn revoke_denies_under_the_same_key() {
    let mut g = sample();
    assert!(g.revoke("acme-sync"));
    let r = g.for_plugin("acme-sync").unwrap();
    assert_eq!(r.decision(), GrantDecision::Denied);
    assert_eq!(r.capabilities(), [] as [String; 0]);
    assert_eq!(r.key(), key("acme-sync", "1.2.0", H2));
    assert!(!g.revoke("acme-sync"));
    assert!(!g.revoke("unknown"));
}

#[cfg(unix)]
#[test]
fn file_is_private_and_other_writable_is_invalid() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("mode");
    let d = s.dir();
    d.save_plugin_grants(&sample()).unwrap();
    let path = d.plugin_grants_path();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
    assert!(matches!(d.load_plugin_grants(), GrantsLoad::Invalid(m) if m.contains("writable")));
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        d.save_plugin_grants(&PluginGrants::new()),
        Err(ConfigError::Invalid { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(d.load_plugin_grants(), GrantsLoad::Loaded(_)));
}

#[test]
fn oversized_file_is_invalid() {
    let s = Scratch::new("big");
    let mut bytes = b"version = 1\n# ".to_vec();
    bytes.resize(1024 * 1024 + 1, b'x');
    write_raw(&s, &bytes);
    assert!(matches!(
        s.dir().load_plugin_grants(),
        GrantsLoad::Invalid(_)
    ));
}

#[test]
fn damaged_newer_and_binary_files_are_never_overwritten() {
    let cases: [(&str, &[u8]); 5] = [
        ("garbage", b"this is [[ not toml"),
        ("newer", b"version = 2\n[[grant]]\nweird = 1\n"),
        ("binary", &[0xff, 0xfe, 0x00, 0x9f]),
        (
            "noversion",
            b"[[grant]]\nkey = \"a-b@1#c\"\ndecision = \"denied\"\n",
        ),
        ("unknown", b"version = 1\nsurprise = true\n"),
    ];
    for (name, bytes) in cases {
        let s = Scratch::new(&format!("bad-{name}"));
        write_raw(&s, bytes);
        let d = s.dir();
        match d.load_plugin_grants() {
            GrantsLoad::Invalid(_) | GrantsLoad::Newer(_) => {}
            other => panic!("{name}: {other:?}"),
        }
        let err = d.save_plugin_grants(&sample()).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{name}");
        assert!(err.to_string().contains("refusing to overwrite"), "{name}");
        assert_eq!(fs::read(d.plugin_grants_path()).unwrap(), bytes, "{name}");
        assert!(
            !d.plugin_grants_path()
                .with_file_name("plugin-grants.toml.tmp")
                .exists()
        );
    }
}

#[test]
fn newer_file_reports_its_version() {
    let s = Scratch::new("newer");
    write_raw(&s, b"version = 7\n");
    assert_eq!(s.dir().load_plugin_grants(), GrantsLoad::Newer(7));
}

#[test]
fn a_directory_in_the_way_is_invalid() {
    let s = Scratch::new("dir");
    fs::create_dir_all(s.dir().plugin_grants_path()).unwrap();
    assert!(matches!(
        s.dir().load_plugin_grants(),
        GrantsLoad::Invalid(_)
    ));
    assert!(s.dir().save_plugin_grants(&sample()).is_err());
}

#[test]
fn more_than_the_cap_is_refused_on_save() {
    let s = Scratch::new("cap");
    let mut g = PluginGrants::new();
    for i in 0..=MAX_PLUGIN_GRANTS {
        g.record(GrantRecord::denied(format!("p-{i}@1#a")).unwrap());
    }
    assert_eq!(g.len(), MAX_PLUGIN_GRANTS + 1);
    assert!(s.dir().save_plugin_grants(&g).is_err());
    assert_eq!(s.dir().load_plugin_grants(), GrantsLoad::Missing);
}

fn plugin_id() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9]{1,6}(-[a-z0-9]{1,4}){0,2}"
}

fn capability() -> impl Strategy<Value = String> {
    "[!-~]{1,40}"
}

fn record() -> impl Strategy<Value = GrantRecord> {
    (
        plugin_id(),
        "[0-9a-f]{1,70}",
        any::<bool>(),
        prop::collection::vec(capability(), 0..8),
    )
        .prop_map(|(id, hash, approved, caps)| {
            let k = format!("{id}@1.0.0#{hash}");
            if approved {
                GrantRecord::approved(k, caps).unwrap()
            } else {
                GrantRecord::denied(k).unwrap()
            }
        })
}

fn grants() -> impl Strategy<Value = PluginGrants> {
    prop::collection::vec(record(), 0..12).prop_map(|rs| {
        let mut g = PluginGrants::new();
        for r in rs {
            g.record(r);
        }
        g
    })
}

fn load_text(bytes: &[u8]) -> GrantsLoad {
    // One scratch dir per call keeps the property tests independent of each other.
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let s = Scratch::new(&format!("prop-{}", N.fetch_add(1, Ordering::Relaxed)));
    write_raw(&s, bytes);
    s.dir().load_plugin_grants()
}

proptest! {
    #[test]
    fn round_trip(g in grants()) {
        let s = Scratch::new("prop-rt");
        s.dir().save_plugin_grants(&g).unwrap();
        prop_assert_eq!(s.dir().load_plugin_grants(), GrantsLoad::Loaded(g));
    }

    #[test]
    fn hostile_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..400)) {
        let _ = load_text(&bytes);
    }

    #[test]
    fn mutated_valid_files_never_panic(
        g in grants(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>(), 0u8..3), 0..6),
    ) {
        let s = Scratch::new("prop-mut-src");
        s.dir().save_plugin_grants(&g).unwrap();
        let mut bytes = fs::read(s.dir().plugin_grants_path()).unwrap();
        for (at, byte, op) in edits {
            if bytes.is_empty() { break; }
            let i = at.index(bytes.len());
            match op {
                0 => bytes[i] = byte,
                1 => bytes.insert(i, byte),
                _ => { bytes.remove(i); }
            }
        }
        let _ = load_text(&bytes);
    }

    #[test]
    fn model_based_record_and_revoke(
        ops in prop::collection::vec((0usize..4, 0u8..3, any::<bool>()), 0..40),
    ) {
        let ids = ["alpha", "beta", "gamma", "delta"];
        let mut g = PluginGrants::new();
        // reference: plugin -> (key, approved)
        let mut model: BTreeMap<&str, (String, bool)> = BTreeMap::new();
        for (n, (p, ver, approve)) in ops.into_iter().enumerate() {
            let id = ids[p];
            if n % 5 == 4 {
                let was = model.get(id).is_some_and(|m| m.1);
                prop_assert_eq!(g.revoke(id), was);
                if let Some(m) = model.get_mut(id) { m.1 = false; }
            } else {
                let k = format!("{id}@{ver}.0.0#{H1}");
                let r = if approve {
                    GrantRecord::approved(k.clone(), vec!["db.read".into()]).unwrap()
                } else {
                    GrantRecord::denied(k.clone()).unwrap()
                };
                g.record(r);
                model.insert(id, (k, approve));
            }
            prop_assert!(g.len() <= ids.len());
        }
        prop_assert_eq!(g.len(), model.len());
        for id in ids {
            let got = g.for_plugin(id);
            match model.get(id) {
                None => prop_assert!(got.is_none()),
                Some((k, approved)) => {
                    let r = got.unwrap();
                    prop_assert_eq!(r.key(), k.as_str());
                    prop_assert_eq!(r.decision() == GrantDecision::Approved, *approved);
                    prop_assert_eq!(r.capabilities().is_empty(), !*approved);
                }
            }
        }
    }
}
