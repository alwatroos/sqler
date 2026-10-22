// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of the manifest hash and the grant key.

#![allow(clippy::format_push_string)]

use std::collections::BTreeSet;

use proptest::prelude::*;
use sqler_plugin_api::{Manifest, ParsedGrantKey};

const POOL: &[&str] = &[
    "db.read",
    "db.write",
    "ui.panel",
    "ai.tool",
    "driver.guest",
    "net.http:api.example.com",
    "net.http:*.example.org:8443",
    "fs.read:data/cache",
    "fs.read:plugin",
    "driver.host:duckdb",
];

fn manifest_with(caps: &BTreeSet<usize>) -> String {
    let list: Vec<String> = normalised(caps)
        .iter()
        .map(|&i| format!("\"{}\"", POOL[i]))
        .collect();
    format!(
        "[plugin]\nid = \"p-one\"\nname = \"P\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = [{}]\n",
        list.join(", ")
    )
}

fn normalised(caps: &BTreeSet<usize>) -> BTreeSet<usize> {
    let mut c = caps.clone();
    if c.contains(&1) {
        c.insert(0);
    }
    c
}

fn parts() -> Vec<(&'static str, String)> {
    vec![
        ("id", "\"p-one\"".to_owned()),
        ("name", "\"P\"".to_owned()),
        ("version", "\"1.0.0\"".to_owned()),
        ("api", "\"0.1\"".to_owned()),
        ("capabilities", "[\"db.read\", \"ui.panel\"]".to_owned()),
    ]
}

proptest! {
    #[test]
    fn formatting_does_not_change_the_hash(
        order in Just(parts()).prop_shuffle(),
        crlf in any::<bool>(),
        pad in "[ \t]{0,4}",
        comment in "[a-z #\"\\[\\]=]{0,20}",
        literal in any::<bool>(),
    ) {
        let reference = Manifest::parse(
            "[plugin]\nid = \"p-one\"\nname = \"P\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = [\"db.read\", \"ui.panel\"]\n",
        ).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let mut text = format!("# {comment}\n[plugin]{pad}\n");
        for (k, v) in order {
            let v = if literal && v.starts_with('"') { format!("'{}'", &v[1..v.len() - 1]) } else { v };
            text.push_str(&format!("{pad}{k}{pad}={pad}{v}{pad} # {comment}\n"));
        }
        if crlf {
            text = text.replace('\n', "\r\n");
        }
        let m = Manifest::parse(&text).map_err(|e| TestCaseError::fail(format!("{e}: {text:?}")))?;
        prop_assert_eq!(m.hash(), reference.hash());
        prop_assert_eq!(m.grant_key(None).unwrap(), reference.grant_key(None).unwrap());
    }

    #[test]
    fn capability_changes_change_the_hash(
        a in proptest::collection::btree_set(0usize..POOL.len(), 0..6),
        b in proptest::collection::btree_set(0usize..POOL.len(), 0..6),
    ) {
        let ma = Manifest::parse(&manifest_with(&a)).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let mb = Manifest::parse(&manifest_with(&b)).map_err(|e| TestCaseError::fail(e.to_string()))?;
        if normalised(&a) == normalised(&b) {
            prop_assert_eq!(ma.hash(), mb.hash());
        } else {
            prop_assert_ne!(ma.hash(), mb.hash());
            prop_assert_ne!(ma.grant_key(None).unwrap(), mb.grant_key(None).unwrap());
            prop_assert!(!mb.capabilities.diff(&ma.capabilities).is_empty());
        }
    }

    #[test]
    fn grant_key_round_trips(a in proptest::collection::btree_set(0usize..POOL.len(), 0..6)) {
        let m = Manifest::parse(&manifest_with(&a)).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let key = m.grant_key(None).unwrap();
        let parsed: ParsedGrantKey = key.to_string().parse().map_err(|e| TestCaseError::fail(format!("{e}")))?;
        prop_assert_eq!(parsed, key);
    }

    #[test]
    fn grant_key_parse_never_panics(s in any::<String>()) {
        let _ = s.parse::<ParsedGrantKey>();
    }
}
