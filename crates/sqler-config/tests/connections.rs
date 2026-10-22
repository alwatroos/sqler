// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Connection store round-trip, migration, atomicity and secret-hygiene tests.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use sqler_config::{
    ConfigDir, ConnectionSpec, ConnectionStore, GroupId, ProfileSecretKind, SshAuth, SshProfile,
    StoreError, TlsMode, TlsProfile,
};
use sqler_core::{DriverId, Environment};

/// Fresh per-test directory (not created), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p =
            std::env::temp_dir().join(format!("sqler-config-{}-store-{test}", std::process::id()));
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

fn driver(id: &str) -> DriverId {
    DriverId::new(id).unwrap()
}

/// A store with nested groups, every field, TLS and all three SSH auths.
fn rich_store() -> ConnectionStore {
    let mut s = ConnectionStore::new();
    let clients = s.add_group("Clients", None).unwrap();
    let work: GroupId = s.add_group("Work", Some(&clients)).unwrap();
    s.add_group("Archive", None).unwrap();

    let mut tls = TlsProfile::new(TlsMode::VerifyFull);
    tls.ca_cert = Some("/etc/ssl/ca.pem".into());
    tls.client_cert = Some("/etc/ssl/client.pem".into());
    tls.client_key = Some("/etc/ssl/client.key".into());
    let mut ssh = SshProfile::new(
        "bastion.example.com",
        "deploy",
        SshAuth::KeyFile {
            path: "/home/me/.ssh/id_ed25519".into(),
            passphrase_in_keychain: true,
        },
    );
    ssh.port = 2222;
    s.add_connection(
        ConnectionSpec::new("Billing", driver("postgres"), "postgres://db/x?user=app")
            .with_color("#22aa66")
            .with_environment(Environment::Prod)
            .with_tls(tls)
            .with_ssh(ssh)
            .with_password_in_keychain(true),
        Some(&work),
    )
    .unwrap();
    s.add_connection(
        ConnectionSpec::new("Staging", driver("postgres"), "postgres://stg/x?user=app")
            .with_environment(Environment::Staging)
            .with_tls(TlsProfile::new(TlsMode::Require))
            .with_ssh(SshProfile::new("jump", "me", SshAuth::Password)),
        Some(&work),
    )
    .unwrap();
    s.add_connection(
        ConnectionSpec::new("Dev", driver("postgres"), "postgres://dev/x")
            .with_environment(Environment::Dev)
            .with_ssh(SshProfile::new("jump2", "me", SshAuth::Agent)),
        Some(&clients),
    )
    .unwrap();
    s.add_connection(
        ConnectionSpec::new("Local", driver("sqlite"), "app.db"),
        None,
    )
    .unwrap();
    s
}

const M1_FILE: &str = "[[connection]]\nname = \"Local DB\"\ndriver = \"sqlite\"\ntarget = \"app.db\"\ncolor = \"#22AA66\"\nenvironment = \"dev\"\n\n[[connection]]\nname = \"Other\"\ndriver = \"sqlite\"\ntarget = \":memory:\"\n";

#[test]
fn missing_file_is_empty_store() {
    let s = Scratch::new("missing");
    assert_eq!(
        s.dir().load_connection_store().unwrap(),
        ConnectionStore::new()
    );
}

#[test]
fn full_round_trip_keeps_everything_and_order() {
    let sc = Scratch::new("roundtrip");
    let d = sc.dir();
    let store = rich_store();
    assert!(!sc.0.exists());
    d.save_connection_store(&store).unwrap();
    let text = fs::read_to_string(d.connections_path()).unwrap();
    assert!(text.starts_with("version = 1"), "{text}");
    let loaded = d.load_connection_store().unwrap();
    assert_eq!(loaded, store);
    let names: Vec<_> = loaded
        .connections()
        .iter()
        .map(|c| c.spec().name.as_str())
        .collect();
    assert_eq!(names, ["Billing", "Staging", "Dev", "Local"]);
    let groups: Vec<_> = loaded
        .groups()
        .iter()
        .map(sqler_config::Group::name)
        .collect();
    assert_eq!(groups, ["Clients", "Work", "Archive"]);
    // Saving the reloaded store is byte-stable.
    d.save_connection_store(&loaded).unwrap();
    assert_eq!(fs::read_to_string(d.connections_path()).unwrap(), text);
}

#[test]
fn edits_survive_save_and_load() {
    let sc = Scratch::new("edits");
    let d = sc.dir();
    let mut store = rich_store();
    let local = store.connections()[3].id().clone();
    store.move_connection(&local, None, 0).unwrap();
    store.rename_connection(&local, "Laptop").unwrap();
    d.save_connection_store(&store).unwrap();
    let loaded = d.load_connection_store().unwrap();
    assert_eq!(loaded, store);
    assert_eq!(
        loaded.child_connections(None).next().unwrap().spec().name,
        "Laptop"
    );
    assert_eq!(loaded.connection(&local).unwrap().id(), &local);
}

#[test]
fn m1_file_loads_both_ways_then_saves_as_v1_and_reloads_equal() {
    let sc = Scratch::new("m1");
    let d = sc.dir();
    fs::create_dir_all(&sc.0).unwrap();
    fs::write(d.connections_path(), M1_FILE).unwrap();

    let flat = d.load_connections().unwrap();
    assert_eq!(flat.len(), 2);
    assert_eq!(flat[0].name, "Local DB");
    assert_eq!(flat[0].color.as_deref(), Some("#22aa66"));
    assert_eq!(flat[0].environment, Some(Environment::Dev));

    let store = d.load_connection_store().unwrap();
    assert_eq!(store.connections()[0].id().as_str(), "local-db");
    assert_eq!(store.groups().len(), 0);
    // Load never rewrites the file.
    assert_eq!(fs::read_to_string(d.connections_path()).unwrap(), M1_FILE);

    d.save_connection_store(&store).unwrap();
    let text = fs::read_to_string(d.connections_path()).unwrap();
    assert!(text.starts_with("version = 1"), "{text}");
    assert_eq!(d.load_connection_store().unwrap(), store);
    assert_eq!(d.load_connections().unwrap(), flat);
}

#[test]
fn newer_version_is_refused_with_a_clear_message() {
    let sc = Scratch::new("newer");
    let d = sc.dir();
    fs::create_dir_all(&sc.0).unwrap();
    fs::write(d.connections_path(), "version = 3\n").unwrap();
    let e = d.load_connection_store().unwrap_err().to_string();
    assert!(e.contains("newer SQLer") && e.contains("version 3"), "{e}");
}

#[test]
fn save_leaves_no_tmp_and_stale_tmp_is_ignored() {
    let sc = Scratch::new("atomic");
    let d = sc.dir();
    fs::create_dir_all(&sc.0).unwrap();
    let tmp = sc.0.join("connections.toml.tmp");
    fs::write(&tmp, "garbage = [[[").unwrap();
    // A stale temporary file does not affect load.
    assert_eq!(d.load_connection_store().unwrap(), ConnectionStore::new());
    d.save_connection_store(&rich_store()).unwrap();
    // The planted file is not ours and stays; the save left no temporary file of its own.
    assert!(tmp.exists());
    let tmps = fs::read_dir(&sc.0)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        })
        .count();
    assert_eq!(tmps, 1);
    assert!(d.connections_path().is_file());
    assert_eq!(d.load_connection_store().unwrap(), rich_store());
}

fn is_secret_key(key: &str) -> bool {
    const FORBIDDEN: [&str; 4] = ["password", "passphrase", "secret", "token"];
    // `password_in_keychain` and `passphrase_in_keychain` are flags, not secrets.
    let k = key.to_ascii_lowercase();
    FORBIDDEN.iter().any(|f| k.contains(f)) && !k.ends_with("_in_keychain")
}

fn walk(value: &toml::Value, found: &mut Vec<String>) {
    match value {
        toml::Value::Table(t) => {
            for (k, v) in t {
                if is_secret_key(k) {
                    found.push(k.clone());
                }
                walk(v, found);
            }
        }
        toml::Value::Array(a) => a.iter().for_each(|v| walk(v, found)),
        _ => {}
    }
}

#[test]
fn no_secret_field_reaches_the_toml() {
    let sc = Scratch::new("hygiene");
    let d = sc.dir();
    d.save_connection_store(&rich_store()).unwrap();
    let text = fs::read_to_string(d.connections_path()).unwrap();
    let table: toml::Table = toml::from_str(&text).unwrap();
    let mut found = Vec::new();
    walk(&toml::Value::Table(table), &mut found);
    assert!(found.is_empty(), "secret-like keys written: {found:?}");
    // Only references and flags exist: the flags are booleans, never values.
    assert!(text.contains("password_in_keychain = true"));
    assert!(text.contains("passphrase_in_keychain = true"));
}

#[test]
fn secret_target_is_rejected_and_never_echoed() {
    let mut s = ConnectionStore::new();
    let spec = ConnectionSpec::new("p", driver("postgres"), "postgres://u:hunter2@h/d");
    assert!(!format!("{spec:?}").contains("hunter2"));
    let err = s.add_connection(spec, None).unwrap_err();
    assert_eq!(err, StoreError::TargetHasSecret);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
    assert_eq!(s.connections().len(), 0);
}

#[test]
fn secret_refs_are_derived_from_stable_ids() {
    let mut s = rich_store();
    let id = s.connections()[0].id().clone();
    let before: Vec<_> = s
        .connection(&id)
        .unwrap()
        .secret_refs()
        .iter()
        .map(|r| (r.kind(), r.account()))
        .collect();
    assert_eq!(
        before,
        [
            (
                ProfileSecretKind::DbPassword,
                "billing:db-password".to_owned()
            ),
            (
                ProfileSecretKind::SshPassphrase,
                "billing:ssh-key-passphrase".to_owned()
            ),
        ]
    );
    s.rename_connection(&id, "Invoicing").unwrap();
    let after: Vec<_> = s
        .connection(&id)
        .unwrap()
        .secret_refs()
        .iter()
        .map(|r| (r.kind(), r.account()))
        .collect();
    assert_eq!(before, after);
}

#[test]
fn profile_converts_to_core_config() {
    let s = rich_store();
    let cfg = s.connections()[0].to_connection_config();
    assert_eq!(cfg.name, "Billing");
    assert_eq!(cfg.driver.as_str(), "postgres");
    assert_eq!(cfg.target, "postgres://db/x?user=app");
    assert_eq!(cfg.color.as_deref(), Some("#22aa66"));
    assert_eq!(cfg.environment, Some(Environment::Prod));
    let all = s.to_connection_configs();
    assert_eq!(all.len(), 4);
    assert_eq!(all[3].name, "Local");
}
