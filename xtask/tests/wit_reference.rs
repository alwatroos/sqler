// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The WIT reference page lists every item of the WIT package, names nothing else, and agrees with the capability gate.

#![allow(clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sqler_plugin_api::{Gate, HostInterface, gate};
use wit_parser::{Resolve, Type, TypeDefKind};

const QUERY_RULE: &str = "db.read or db.write (query rule)";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repository root")
        .to_path_buf()
}

fn parse_wit() -> Resolve {
    let mut resolve = Resolve::default();
    resolve
        .push_dir(
            repo_root()
                .join("crates")
                .join("sqler-plugin-api")
                .join("wit"),
        )
        .expect("the wit directory parses");
    resolve
}

fn page() -> String {
    std::fs::read_to_string(
        repo_root()
            .join("docs")
            .join("src")
            .join("plugin-development")
            .join("wit-reference.md"),
    )
    .expect("the reference page is readable")
}

fn host_interface(interface: &str, function: &str) -> HostInterface {
    match (interface, function) {
        ("ui", "refresh-panel") | ("ui-handler", "render-panel" | "panel-event") => {
            HostInterface::UiPanel
        }
        ("ui", _) => HostInterface::Ui,
        ("ui-handler", _) => HostInterface::UiHandler,
        ("driver-catalog", _) => HostInterface::Driver,
        _ => HostInterface::ALL
            .into_iter()
            .find(|h| h.wit_interface() == Some(interface))
            .expect("every function belongs to a gated interface"),
    }
}

fn expected_capability(interface: &str, function: &str) -> String {
    match gate(host_interface(interface, function)) {
        Gate::None => "none".to_owned(),
        Gate::Requires(kind) => kind.name().to_owned(),
        Gate::Query => QUERY_RULE.to_owned(),
        other => panic!("the reference page does not know the gate {other:?}"),
    }
}

/// The WIT items as `key -> expected capability cell` (`-` for types), plus the world names.
fn wit_items(resolve: &Resolve) -> (BTreeMap<String, String>, BTreeSet<String>, String) {
    let (_, package) = resolve.packages.iter().next().expect("one package");
    let version = package
        .name
        .version
        .as_ref()
        .expect("the package has a version")
        .to_string();
    let worlds = package.worlds.keys().cloned().collect();
    let mut items = BTreeMap::new();
    for (name, id) in &package.interfaces {
        let interface = &resolve.interfaces[*id];
        for (type_name, type_id) in &interface.types {
            let def = &resolve.types[*type_id];
            // A `use` line becomes an alias of a type owned by another interface; it is not an item here.
            if let TypeDefKind::Type(Type::Id(target)) = &def.kind
                && resolve.types[*target].owner != def.owner
            {
                continue;
            }
            items.insert(format!("{name}.{type_name}"), "-".to_owned());
        }
        for function in interface.functions.values() {
            let (item, method) = if let Some(rest) = function.name.strip_prefix("[method]") {
                (
                    rest.to_owned(),
                    rest.rsplit('.').next().unwrap_or(rest).to_owned(),
                )
            } else if let Some(rest) = function.name.strip_prefix("[static]") {
                (
                    rest.to_owned(),
                    rest.rsplit('.').next().unwrap_or(rest).to_owned(),
                )
            } else if let Some(rest) = function.name.strip_prefix("[constructor]") {
                (format!("{rest}.constructor"), "constructor".to_owned())
            } else {
                (function.name.clone(), function.name.clone())
            };
            items.insert(format!("{name}.{item}"), expected_capability(name, &method));
        }
    }
    (items, worlds, version)
}

fn cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_owned())
        .collect()
}

fn unquote(cell: &str) -> Option<&str> {
    cell.strip_prefix('`')?.strip_suffix('`')
}

struct Page {
    version: Option<String>,
    worlds: Vec<String>,
    rows: Vec<(String, String, String)>,
    duplicates: Vec<String>,
}

fn parse_page(text: &str) -> Page {
    let mut page = Page {
        version: None,
        worlds: Vec::new(),
        rows: Vec::new(),
        duplicates: Vec::new(),
    };
    let mut section = String::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(heading) = line.strip_prefix("## ") {
            heading.trim().clone_into(&mut section);
            continue;
        }
        if page.version.is_none()
            && let Some(start) = line.find("`sqler:plugin@")
        {
            let rest = &line[start + "`sqler:plugin@".len()..];
            if let Some(end) = rest.find('`') {
                page.version = Some(rest[..end].to_owned());
            }
        }
        if !line.starts_with("| `") {
            continue;
        }
        let row = cells(line);
        let Some(key) = row.first().and_then(|c| unquote(c)) else {
            continue;
        };
        if section == "Worlds" {
            page.worlds.push(key.to_owned());
        } else if let Some(interface) = unquote(&section) {
            assert!(
                key.starts_with(&format!("{interface}.")),
                "row `{key}` sits under the section `{interface}`"
            );
            assert!(
                row.len() >= 3,
                "row `{key}` has a signature and a capability cell"
            );
            if !seen.insert(key.to_owned()) {
                page.duplicates.push(key.to_owned());
            }
            page.rows
                .push((key.to_owned(), row[1].clone(), row[row.len() - 1].clone()));
        }
    }
    page
}

#[test]
fn package_version_is_on_the_page() {
    let (_, _, version) = wit_items(&parse_wit());
    let page = parse_page(&page());
    assert_eq!(page.version.as_deref(), Some(version.as_str()));
}

#[test]
fn worlds_match_in_both_directions() {
    let (_, worlds, _) = wit_items(&parse_wit());
    let page = parse_page(&page());
    let listed: BTreeSet<String> = page.worlds.iter().cloned().collect();
    assert_eq!(listed.len(), page.worlds.len(), "a world is listed twice");
    let missing: Vec<_> = worlds.difference(&listed).collect();
    let stale: Vec<_> = listed.difference(&worlds).collect();
    assert!(
        missing.is_empty(),
        "worlds missing from the page: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "worlds on the page but not in the WIT: {stale:?}"
    );
}

#[test]
fn items_match_in_both_directions() {
    let (items, _, _) = wit_items(&parse_wit());
    let page = parse_page(&page());
    assert!(
        page.duplicates.is_empty(),
        "rows listed twice: {:?}",
        page.duplicates
    );
    let listed: BTreeSet<&str> = page.rows.iter().map(|(k, _, _)| k.as_str()).collect();
    let missing: Vec<_> = items
        .keys()
        .filter(|k| !listed.contains(k.as_str()))
        .collect();
    let stale: Vec<_> = listed.iter().filter(|k| !items.contains_key(**k)).collect();
    assert!(
        missing.is_empty(),
        "WIT items missing from the page: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "page rows that name no WIT item: {stale:?}"
    );
}

#[test]
fn capability_cells_agree_with_the_gate() {
    let (items, _, _) = wit_items(&parse_wit());
    let page = parse_page(&page());
    for (key, signature, capability) in &page.rows {
        assert!(!signature.is_empty(), "row `{key}` has a signature");
        if let Some(expected) = items.get(key) {
            assert_eq!(capability, expected, "capability cell of `{key}`");
        }
    }
}
