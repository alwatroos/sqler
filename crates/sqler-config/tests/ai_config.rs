// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! AI configuration round trip: no secret ever reaches a config file or a Debug string.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqler_config::{
    AiConfig, ConfigDir, ConnectionAi, ConnectionSpec, ConnectionStore, MemoryStore, ModelChoice,
    ProviderConfig, ProviderId, ProviderKeyRef, ProviderKind, RedactionList, Secret, SecretStore,
};
use sqler_core::{DriverId, Environment};

const KEY: &str = "sk-test-SECRET-123";

/// Fresh per-test directory (not created), removed on drop.
static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "sqler-config-{}-{}-{:?}-ai-{test}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            std::thread::current().id()
        ));
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

fn config() -> AiConfig {
    let id = ProviderId::new("claude").unwrap();
    let mut c = AiConfig::default();
    c.providers.push(
        ProviderConfig::new(id.clone(), ProviderKind::Anthropic)
            .with_models(["sonnet"])
            .with_api_key_in_keychain(true),
    );
    c.models.default = Some(ModelChoice::new(id, "sonnet"));
    c.redaction = RedactionList::new(["iban".to_owned()]).unwrap();
    c.limits.max_rows = 250;
    c
}

#[test]
fn no_secret_text_in_files_or_debug_output() {
    let sc = Scratch::new("no-secret");
    let dir = sc.dir();
    let cfg = config();

    let store = MemoryStore::new();
    let key_ref = cfg.providers[0].key_ref().unwrap();
    assert_eq!(key_ref, ProviderKeyRef::new("claude").unwrap());
    store.set(key_ref.secret_ref(), &Secret::from(KEY)).unwrap();

    let mut conns = ConnectionStore::new();
    let spec = ConnectionSpec::new("Billing", DriverId::new("sqlite").unwrap(), "billing.db")
        .with_environment(Environment::Prod)
        .with_ai(ConnectionAi::new(true));
    conns.add_connection(spec.clone(), None).unwrap();

    dir.save_ai_config(&cfg).unwrap();
    dir.save_connection_store(&conns).unwrap();

    let ai_text = fs::read_to_string(dir.ai_config_path()).unwrap();
    let conn_text = fs::read_to_string(dir.connections_path()).unwrap();
    assert!(ai_text.starts_with("version = 1\n"), "{ai_text}");
    assert!(conn_text.starts_with("version = 2\n"), "{conn_text}");
    assert!(ai_text.contains("api_key_in_keychain = true"), "{ai_text}");
    for text in [&ai_text, &conn_text] {
        assert!(!text.contains(KEY), "{text}");
    }

    assert_eq!(dir.load_ai_config().unwrap(), cfg);
    let reloaded = dir.load_connection_store().unwrap();
    assert_eq!(reloaded, conns);
    assert!(reloaded.connections()[0].ai_enabled());

    // The key is still where it was put.
    assert_eq!(
        store.get(key_ref.secret_ref()).unwrap().unwrap().expose(),
        KEY
    );

    let secret = store.get(key_ref.secret_ref()).unwrap().unwrap();
    for debug in [
        format!("{cfg:?}"),
        format!("{key_ref:?}"),
        format!("{spec:?}"),
        format!("{secret:?}"),
        format!("{:#?}", dir.load_ai_config().unwrap()),
    ] {
        assert!(!debug.contains(KEY), "{debug}");
    }
}

#[test]
fn missing_files_load_as_defaults_and_ai_off_keeps_version_1() {
    let sc = Scratch::new("defaults");
    let dir = sc.dir();
    assert_eq!(dir.load_ai_config().unwrap(), AiConfig::default());

    let mut conns = ConnectionStore::new();
    let spec = ConnectionSpec::new("Local", DriverId::new("sqlite").unwrap(), "a.db");
    conns.add_connection(spec, None).unwrap();
    dir.save_connection_store(&conns).unwrap();
    let text = fs::read_to_string(dir.connections_path()).unwrap();
    assert!(
        text.starts_with("version = 1\n") && !text.contains("ai"),
        "{text}"
    );
}

#[test]
fn saving_an_invalid_config_is_refused_and_writes_nothing() {
    let sc = Scratch::new("invalid");
    let dir = sc.dir();
    let mut cfg = config();
    cfg.limits.max_rows = 0;
    assert!(dir.save_ai_config(&cfg).is_err());
    assert!(!dir.ai_config_path().exists());
}
