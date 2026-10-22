// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of manifest parsing: hostile input never panics; bounded work; valid manifests round-trip.

#![allow(clippy::format_push_string)]

use proptest::prelude::*;
use sqler_plugin_api::{Manifest, ManifestError};

const FRAGMENTS: &[&str] = &[
    "[",
    "]",
    "[[",
    "]]",
    "{",
    "}",
    "=",
    ".",
    ",",
    "\"",
    "'",
    "\"\"\"",
    "'''",
    "#",
    "\n",
    "\r\n",
    " ",
    "\\",
    "plugin",
    "id",
    "name",
    "capabilities",
    "entry",
    "commands",
    "panels",
    "ai-tools",
    "input-schema",
    "1",
    "-1",
    "1e400",
    "nan",
    "inf",
    "true",
    "1979-05-27T07:32:00Z",
    "é",
    "\u{202e}",
    "\0",
    "[plugin]",
    "[[commands]]",
    "db.read",
];

fn fragments() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(FRAGMENTS), 0..60).prop_map(|v| v.concat())
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = Manifest::parse_bytes(&bytes);
    }

    #[test]
    fn arbitrary_strings_never_panic(s in any::<String>()) {
        let _ = Manifest::parse(&s);
    }

    #[test]
    fn toml_shaped_noise_never_panics(s in fragments()) {
        let _ = Manifest::parse(&s);
    }

    #[test]
    fn mutated_valid_manifest_never_panics(cut in 0usize..400, insert in fragments()) {
        let base = include_str!("fixtures/full.toml");
        let mut at = cut.min(base.len());
        while !base.is_char_boundary(at) {
            at -= 1;
        }
        let _ = Manifest::parse(&format!("{}{}{}", &base[..at], insert, &base[at..]));
    }

    #[test]
    fn oversize_is_rejected_before_parsing(extra in 1usize..5000) {
        let text = " ".repeat(64 * 1024 + extra);
        let is_too_large = matches!(Manifest::parse(&text), Err(ManifestError::TooLarge { .. }));
        prop_assert!(is_too_large);
        let is_too_large = matches!(Manifest::parse_bytes(text.as_bytes()), Err(ManifestError::TooLarge { .. }));
        prop_assert!(is_too_large);
    }

    #[test]
    fn valid_manifests_round_trip(
        id in "[a-z][a-z0-9]{1,10}(-[a-z0-9]{1,5}){0,3}",
        name in "[A-Za-z0-9 ]{1,40}",
        (major, minor, patch) in (0u64..100, 0u64..100, 0u64..100),
        n_commands in 0usize..5,
        read in any::<bool>(),
    ) {
        let name = format!("N{name}");
        let caps = if read { "\"db.read\"" } else { "" };
        let mut text = format!(
            "[plugin]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"{major}.{minor}.{patch}\"\napi = \"0.1\"\ncapabilities = [{caps}]\n"
        );
        if n_commands > 0 {
            text.push_str("[entry]\ncomponent = \"p.wasm\"\n");
        }
        for i in 0..n_commands {
            text.push_str(&format!("[[commands]]\nid = \"c{i}\"\ntitle = \"Title {i}\"\n"));
        }
        let m = Manifest::parse(&text).map_err(|e| TestCaseError::fail(format!("{e}")))?;
        prop_assert_eq!(m.id.as_str(), id.as_str());
        prop_assert_eq!(&m.name, &name);
        prop_assert_eq!(m.version.to_string(), format!("{major}.{minor}.{patch}"));
        prop_assert_eq!(m.contributes.commands.len(), n_commands);
        prop_assert_eq!(m.capabilities.len(), usize::from(read));
        for (i, c) in m.contributes.commands.iter().enumerate() {
            prop_assert_eq!(c.id.as_str(), format!("c{i}"));
            prop_assert_eq!(&c.title, &format!("Title {i}"));
        }
    }
}

fn nested(open: &str, close: &str, n: usize) -> String {
    format!("x = {}{}", open.repeat(n), close.repeat(n))
}

#[test]
fn deep_nesting_does_not_overflow_the_stack() {
    for (open, close) in [("[", "]"), ("{a=", "}")] {
        for n in [17usize, 100, 10_000] {
            let text = nested(open, close, n);
            let r = Manifest::parse(&text);
            assert!(
                matches!(
                    r,
                    Err(ManifestError::TooDeep | ManifestError::Syntax { .. })
                ),
                "{open} x {n}: {r:?}"
            );
        }
        // unbalanced: only openers
        let r = Manifest::parse(&open.repeat(10_000));
        assert!(
            matches!(
                r,
                Err(ManifestError::TooDeep | ManifestError::Syntax { .. })
            ),
            "{r:?}"
        );
    }
    // table headers and dotted keys nest without brackets
    let dotted = format!("{}a = 1\n", "a.".repeat(20_000));
    let r = Manifest::parse(&dotted);
    assert!(
        matches!(
            r,
            Err(ManifestError::TooDeep | ManifestError::Syntax { .. })
        ),
        "{r:?}"
    );
    let header = format!("[{}]\n", vec!["a"; 20_000].join("."));
    let r = Manifest::parse(&header);
    assert!(
        matches!(
            r,
            Err(ManifestError::TooDeep | ManifestError::Syntax { .. })
        ),
        "{r:?}"
    );
}
