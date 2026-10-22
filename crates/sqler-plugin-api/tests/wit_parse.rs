// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The WIT package parses and has the names ADR-0055 pins.

#![allow(clippy::expect_used)]

use std::path::Path;

use wit_parser::Resolve;

const WORLDS: [&str; 9] = [
    "imports",
    "base",
    "ui-plugin",
    "ai-tool-plugin",
    "exporter-plugin",
    "completion-plugin",
    "driver-plugin",
    "driver-catalog-plugin",
    "full",
];

const INTERFACES: [&str; 14] = [
    "types",
    "log",
    "settings",
    "catalog",
    "query",
    "widgets",
    "ui",
    "lifecycle",
    "ui-handler",
    "ai-tools",
    "exporter",
    "completion",
    "driver",
    "driver-catalog",
];

fn parse() -> Resolve {
    let mut resolve = Resolve::default();
    resolve
        .push_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("wit"))
        .expect("the wit directory parses");
    resolve
}

#[test]
fn package_is_sqler_plugin_0_1_1() {
    let resolve = parse();
    assert_eq!(resolve.packages.len(), 1, "no other package is referenced");
    let (_, package) = resolve.packages.iter().next().expect("one package");
    assert_eq!(package.name.namespace, "sqler");
    assert_eq!(package.name.name, "plugin");
    let version = package.name.version.as_ref().expect("a version");
    assert_eq!(version.to_string(), "0.1.1");
}

#[test]
fn worlds_and_interfaces_exist_by_name() {
    let resolve = parse();
    let (_, package) = resolve.packages.iter().next().expect("one package");
    let worlds: Vec<&str> = package.worlds.keys().map(String::as_str).collect();
    let interfaces: Vec<&str> = package.interfaces.keys().map(String::as_str).collect();
    assert_eq!(worlds.len(), WORLDS.len(), "worlds: {worlds:?}");
    assert_eq!(
        interfaces.len(),
        INTERFACES.len(),
        "interfaces: {interfaces:?}"
    );
    for name in WORLDS {
        assert!(worlds.contains(&name), "missing world {name}");
    }
    for name in INTERFACES {
        assert!(interfaces.contains(&name), "missing interface {name}");
    }
}
