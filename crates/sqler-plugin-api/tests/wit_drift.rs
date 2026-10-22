// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The Rust mirror of the WIT package has not drifted from `wit/`.

#![allow(clippy::expect_used)]

use std::path::Path;

use sqler_core::ObjectKind;
use sqler_plugin_api::{API_VERSION, HostInterface, WIDGET_CASES, WIT_PACKAGE};
use wit_parser::{Resolve, TypeDefKind};

fn parse() -> Resolve {
    let mut resolve = Resolve::default();
    resolve
        .push_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("wit"))
        .expect("the wit directory parses");
    resolve
}

fn interface_types<'a>(resolve: &'a Resolve, interface: &str) -> &'a wit_parser::Interface {
    let (_, package) = resolve.packages.iter().next().expect("one package");
    let id = package.interfaces.get(interface).expect("interface exists");
    &resolve.interfaces[*id]
}

#[test]
fn package_version_equals_api_version() {
    let resolve = parse();
    let (_, package) = resolve.packages.iter().next().expect("one package");
    assert_eq!(
        format!("{}:{}", package.name.namespace, package.name.name),
        WIT_PACKAGE
    );
    assert_eq!(
        package.name.version.as_ref().expect("version").to_string(),
        API_VERSION.to_string()
    );
}

#[test]
fn widget_cases_equal_the_wit_variant() {
    let resolve = parse();
    let widgets = interface_types(&resolve, "widgets");
    let id = widgets.types.get("widget").expect("widget type");
    let TypeDefKind::Variant(variant) = &resolve.types[*id].kind else {
        panic!("widget is a variant");
    };
    let cases: Vec<&str> = variant.cases.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cases, WIDGET_CASES);
}

#[test]
fn object_kind_cases_equal_core() {
    let resolve = parse();
    let types = interface_types(&resolve, "types");
    let id = types.types.get("object-kind").expect("object-kind type");
    let TypeDefKind::Enum(e) = &resolve.types[*id].kind else {
        panic!("object-kind is an enum");
    };
    let cases: Vec<&str> = e.cases.iter().map(|c| c.name.as_str()).collect();
    let core: Vec<&str> = ObjectKind::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(cases, core);
}

#[test]
fn every_host_interface_exists_in_the_wit() {
    let resolve = parse();
    let (_, package) = resolve.packages.iter().next().expect("one package");
    for h in HostInterface::ALL {
        if let Some(name) = h.wit_interface() {
            assert!(package.interfaces.contains_key(name), "{h:?} -> {name}");
        }
    }
}
