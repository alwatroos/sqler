// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plugin template (`plugins/template`) has not drifted from the repository: its WIT copy, toolchain file and
//! `wit-bindgen` pin are the repository's, its manifest is valid once the placeholders are filled, and its Rust
//! sources carry no template tags.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqler_plugin_api::{API_VERSION, Manifest};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repository root")
        .to_path_buf()
}

fn template_dir() -> PathBuf {
    repo_root().join("plugins").join("template")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .replace('\r', "")
}

/// File name -> content (carriage returns removed) of every `.wit` file in `dir`.
fn wit_files(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "wit"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            let text = read(&path);
            (name, text)
        })
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("directory lists") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn wit_copy_equals_the_api_crate() {
    let source = wit_files(
        &repo_root()
            .join("crates")
            .join("sqler-plugin-api")
            .join("wit"),
    );
    let copy = wit_files(&template_dir().join("wit"));
    assert!(!source.is_empty(), "the API crate has WIT files");
    assert_eq!(
        source.keys().collect::<Vec<_>>(),
        copy.keys().collect::<Vec<_>>(),
        "the template wit/ folder must hold the same files as crates/sqler-plugin-api/wit"
    );
    for (name, text) in &source {
        assert_eq!(
            Some(text),
            copy.get(name),
            "plugins/template/wit/{name} differs from crates/sqler-plugin-api/wit/{name}; copy the file again"
        );
    }
}

#[test]
fn toolchain_file_equals_the_root_one() {
    assert_eq!(
        read(&repo_root().join("rust-toolchain.toml")),
        read(&template_dir().join("rust-toolchain.toml")),
        "plugins/template/rust-toolchain.toml must be a copy of rust-toolchain.toml"
    );
}

fn wit_bindgen_pin(manifest: &str, workspace: bool) -> String {
    let table: toml::Table = manifest.parse().expect("Cargo.toml parses");
    let dependencies = if workspace {
        table
            .get("workspace")
            .and_then(|w| w.get("dependencies"))
            .expect("workspace.dependencies")
    } else {
        table.get("dependencies").expect("dependencies")
    };
    dependencies
        .get("wit-bindgen")
        .and_then(toml::Value::as_str)
        .expect("wit-bindgen is a version string")
        .to_owned()
}

#[test]
fn wit_bindgen_pin_equals_the_guest_workspace() {
    let guests = read(&repo_root().join("plugins").join("Cargo.toml"));
    let template = read(&template_dir().join("Cargo.toml"));
    assert_eq!(
        wit_bindgen_pin(&guests, true),
        wit_bindgen_pin(&template, false),
        "wit-bindgen in plugins/template/Cargo.toml must equal workspace.dependencies in plugins/Cargo.toml"
    );
}

#[test]
fn manifest_is_valid_with_the_placeholders_filled() {
    let text = read(&template_dir().join("sqler.toml"))
        .replace("{{project-name}}", "template-check")
        .replace("{{crate_name}}", "template_check")
        .replace("{{display_name}}", "Template check");
    assert!(!text.contains("{{"), "an unknown placeholder is left");
    let manifest = Manifest::parse(&text).expect("the filled manifest is valid");
    assert_eq!(manifest.id().to_string(), "template-check");
    assert_eq!(manifest.name(), "Template check");
    assert_eq!(manifest.api(), API_VERSION);
    assert_eq!(
        manifest.entry().map(|e| e.component.as_str()),
        Some("template_check.wasm")
    );
}

#[test]
fn rust_sources_have_no_template_tags() {
    let mut files = Vec::new();
    rust_files(&template_dir(), &mut files);
    assert!(!files.is_empty(), "the template has Rust sources");
    for file in files {
        let text = read(&file);
        assert!(
            !text.contains("{{") && !text.contains("{%"),
            "{} contains a template tag; Rust files are not templated",
            file.display()
        );
    }
}
