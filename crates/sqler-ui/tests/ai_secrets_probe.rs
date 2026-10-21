// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Secrets probe for the AI save flow: a canary API key goes through the real draft, save and
//! keychain-op path against a `MemoryStore`, and no file the app writes, no `Debug` text may
//! contain it. Also: older `connections.toml` / `workbench.toml` files still load.

#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::default_trait_access,
    clippy::format_push_string
)]

use std::fs;
use std::path::Path;

use sqler_config::MemoryStore;
use sqler_config::{
    AiConfig, ConfigDir, ProviderId, ProviderKeyRef, ProviderKind, SecretStore, WorkbenchLoad,
    WorkbenchState,
};
use sqler_ui::ai::{ALL_KINDS, AiAction, AiField, AiState};
use sqler_ui::sidebar::SidebarModel;
use sqler_ui::workspace::{Effect, SecretOp, Workspace};

const CANARY: &str = "sk-CANARY-7f3a9c1e-DO-NOT-LEAK";

fn files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn text(w: &mut Workspace, field: AiField, t: &str) {
    let _ = w.dispatch_ai(AiAction::SetText {
        field,
        text: t.into(),
    });
}

#[test]
fn canary_key_never_reaches_a_file_or_debug_output() {
    let root = std::env::temp_dir().join(format!("sqler-probe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let dir = ConfigDir::new(&root);
    let store = MemoryStore::new();
    let mut debug = String::new();

    let sidebar = SidebarModel::from_store(Default::default(), Some(dir.clone()));
    let ai = AiState::new(AiConfig::default(), ALL_KINDS.to_vec(), Some(dir.clone()));
    let mut w = Workspace::new(sidebar).with_ai(ai);

    let a = AiAction::SetText {
        field: AiField::Key,
        text: CANARY.into(),
    };
    debug.push_str(&format!("{a:?}"));
    let _ = w.dispatch_ai(AiAction::AddProvider(ProviderKind::Anthropic));
    text(&mut w, AiField::ProviderId, "claude");
    text(&mut w, AiField::Models, "m1");
    text(&mut w, AiField::Key, CANARY);
    debug.push_str(&format!("{:?}", w.ai()));
    // Test connection with a typed key.
    let t = w.dispatch_ai(AiAction::TestProvider);
    debug.push_str(&format!("{t:?}"));
    let effect = w.dispatch_ai(AiAction::SaveProvider);
    debug.push_str(&format!("{effect:?} {:?}", w.ai()));
    let Effect::SaveAiConfig {
        dir: d,
        config,
        secrets,
    } = effect
    else {
        panic!("expected SaveAiConfig");
    };
    d.unwrap().save_ai_config(&config).unwrap();
    debug.push_str(&format!("{config:?}"));
    let mut sets = 0;
    for op in secrets {
        match op {
            SecretOp::Set(r, s) => {
                debug.push_str(&format!("{r:?} {s:?}"));
                store.set(&r, &s).unwrap();
                sets += 1;
            }
            SecretOp::Delete(r) => {
                store.delete(&r).unwrap();
            }
        }
    }
    assert_eq!(sets, 1, "the key goes to the keychain store");
    // The editor shows the key as stored, never the text.
    debug.push_str(&format!("{:?}", w.ai().draft()));
    w.save_connections().unwrap();
    dir.save_workbench_state(&WorkbenchState::default())
        .unwrap();
    // Edit the saved provider again and remove it.
    let _ = w.dispatch_ai(AiAction::EditProvider(ProviderId::new("claude").unwrap()));
    debug.push_str(&format!("{:?}", w.ai()));

    // Positive control: the keychain store has it.
    let r = ProviderKeyRef::new("claude").unwrap();
    let r = r.secret_ref();
    assert_eq!(store.get(r).unwrap().unwrap().expose(), CANARY);

    let mut all = Vec::new();
    files(&root, &mut all);
    let names: Vec<_> = all
        .iter()
        .map(|p| p.file_name().unwrap().to_owned())
        .collect();
    assert!(all.iter().any(|p| p.ends_with("ai.toml")), "{names:?}");
    for p in &all {
        let bytes = fs::read(p).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(CANARY),
            "canary in {}",
            p.display()
        );
    }
    assert!(!debug.contains(CANARY), "canary in Debug output");
    assert!(!debug.contains("CANARY"), "canary fragment in Debug output");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn old_files_still_load() {
    let root = std::env::temp_dir().join(format!("sqler-probe-old-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let dir = ConfigDir::new(&root);
    // A v1 connections file with no AI keys at all.
    fs::write(
        dir.connections_path(),
        "version = 1\n\n[[connection]]\nid = \"c1\"\nname = \"Old\"\ndriver = \"sqlite\"\ntarget = \"sqlite::memory:\"\n",
    )
    .unwrap();
    let store = dir.load_connection_store();
    // A workbench file without the new side view value (and one with unknown view).
    fs::write(
        dir.workbench_path(),
        "version = 1\n\n[side_panel]\nvisible = true\nsize = 300.0\nview = \"saved-queries\"\n",
    )
    .unwrap();
    let wb = dir.load_workbench_state();
    let WorkbenchLoad::Loaded(s) = wb else {
        panic!("workbench did not load");
    };
    assert_eq!(s.side_panel.view, "saved-queries");
    assert!((s.side_panel.size - 300.0).abs() < 1e-3);
    fs::write(
        dir.workbench_path(),
        "version = 1\n\n[side_panel]\nview = \"ai\"\n",
    )
    .unwrap();
    let WorkbenchLoad::Loaded(s) = dir.load_workbench_state() else {
        panic!("ai view did not load");
    };
    assert_eq!(s.side_panel.view, "ai");
    // An empty-of-view file falls back, never fails.
    fs::write(dir.workbench_path(), "version = 1\n").unwrap();
    assert!(matches!(
        dir.load_workbench_state(),
        WorkbenchLoad::Loaded(_)
    ));
    let store = store.unwrap();
    assert_eq!(store.connections().len(), 1);
    assert_eq!(store.connections()[0].spec().name, "Old");
    let _ = fs::remove_dir_all(&root);
}
