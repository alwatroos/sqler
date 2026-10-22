// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of the capability grammar: round-trips, and hostile hosts and scopes are always rejected.

use std::path::{Component, Path};

use proptest::prelude::*;
use sqler_plugin_api::{Capability, FsRoots, FsScope, HostPattern};

fn label() -> impl Strategy<Value = String> {
    "[a-z0-9]([a-z0-9-]{0,10}[a-z0-9])?"
}

fn valid_host() -> impl Strategy<Value = String> {
    (
        proptest::collection::vec(label(), 1..4),
        "[a-z][a-z0-9-]{0,6}[a-z]",
        prop::option::of(1u16..=65535),
        any::<bool>(),
    )
        .prop_map(|(labels, tld, port, wildcard)| {
            let mut host = labels.join(".");
            host.push('.');
            host.push_str(&tld);
            if wildcard && host.matches('.').count() >= 2 {
                host = format!("*.{host}");
            }
            if let Some(p) = port {
                host = format!("{host}:{p}");
            }
            host
        })
}

fn valid_scope() -> impl Strategy<Value = String> {
    (
        prop_oneof![Just("plugin"), Just("data"), Just("home")],
        proptest::collection::vec("[A-Za-z0-9_-][A-Za-z0-9_-]{0,10}", 1..5),
    )
        .prop_filter("no reserved device names", |(_, segs)| {
            segs.iter().all(|s| {
                let l = s.to_ascii_lowercase();
                !["con", "nul", "aux", "prn"].contains(&l.as_str())
                    && !((l.starts_with("com") || l.starts_with("lpt")) && l.len() == 4)
            })
        })
        .prop_map(|(root, segs)| format!("{root}/{}", segs.join("/")))
}

fn hostile_host() -> impl Strategy<Value = String> {
    let ipv4 = (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>());
    prop_oneof![
        (
            valid_host(),
            prop_oneof![
                Just("http://"),
                Just("https://"),
                Just("ftp://"),
                Just("//")
            ]
        )
            .prop_map(|(h, s)| format!("{s}{h}")),
        (valid_host(), "[a-z]{1,6}").prop_map(|(h, u)| format!("{u}@{h}")),
        (valid_host(), "[a-z]{1,6}:[a-z]{1,6}").prop_map(|(h, u)| format!("{u}@{h}")),
        ipv4.prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        ipv4.prop_map(|(a, b, _, _)| format!("{a}.{b}")),
        ipv4.prop_map(|(a, b, c, d)| format!("0x{a:x}.0x{b:x}.0x{c:x}.0x{d:x}")),
        ipv4.prop_map(|(a, b, c, d)| u32::from_be_bytes([a, b, c, d]).to_string()),
        any::<u128>().prop_map(|x| format!("[{:x}::{:x}]", x >> 64, x & 0xffff)),
        any::<u128>().prop_map(|x| format!("[{:x}::1]:80", x & 0xffff)),
        valid_host().prop_map(|h| format!("{h}/path")),
        valid_host().prop_map(|h| format!("{h}?q=1")),
        valid_host().prop_map(|h| format!("{h}#frag")),
        valid_host().prop_map(|h| format!("{h}%2e")),
        valid_host().prop_map(|h| format!("{h}.")),
        valid_host().prop_map(|h| format!("{h} ")),
        valid_host().prop_map(|h| format!("é{h}")),
        valid_host()
            .prop_filter("has a letter", |h| h
                .chars()
                .any(|c| c.is_ascii_lowercase()))
            .prop_map(|h| h.to_ascii_uppercase()),
        label().prop_map(|l| format!("*.{l}")),
        Just("*".to_owned()),
        Just("*.com".to_owned()),
        Just(String::new()),
        label().prop_filter("not localhost", |l| l != "localhost"),
        (valid_host(), 65536u32..=999_999)
            .prop_map(|(h, p)| format!("{}:{p}", h.split(':').next().unwrap_or(""))),
        valid_host().prop_map(|h| format!("{}:0", h.split(':').next().unwrap_or(""))),
        valid_host().prop_map(|h| format!("{}:080", h.split(':').next().unwrap_or(""))),
    ]
}

fn hostile_scope() -> impl Strategy<Value = String> {
    let bad_segment = prop_oneof![
        Just("..".to_owned()),
        Just(".".to_owned()),
        Just(String::new()),
        "[a-z]{1,4}".prop_map(|s| format!(".{s}")),
        "[a-z]{1,4}".prop_map(|s| format!("{s}\\{s}")),
        "[a-z]{1,4}".prop_map(|s| format!("{s}:{s}")),
        Just("C:".to_owned()),
        Just("%2e%2e".to_owned()),
        "[a-z]{1,4}".prop_map(|s| format!("{s}%2f{s}")),
        "[a-z]{1,4}".prop_map(|s| format!("{s}\0")),
        prop_oneof![
            Just("con"),
            Just("NUL"),
            Just("Aux"),
            Just("prn.txt"),
            Just("com1"),
            Just("LPT9.log")
        ]
        .prop_map(str::to_owned),
        "[a-z]{1,4}".prop_map(|s| format!("{s}é")),
        Just("a".repeat(65)),
    ];
    prop_oneof![
        (
            prop_oneof![Just("plugin"), Just("data"), Just("home")],
            bad_segment.clone(),
            valid_scope()
        )
            .prop_map(|(root, bad, ok)| format!(
                "{root}/{bad}/{}",
                ok.split_once('/').map_or("x", |p| p.1)
            )),
        (
            prop_oneof![Just("plugin"), Just("data"), Just("home")],
            bad_segment
        )
            .prop_map(|(root, bad)| format!("{root}/{bad}")),
        valid_scope().prop_map(|s| format!("/{s}")),
        valid_scope().prop_map(|s| format!("{s}/")),
        valid_scope().prop_map(|s| s.replace('/', "\\")),
        "[A-Za-z]".prop_map(|d| format!("{d}:/x")),
        Just("home".to_owned()),
        Just(String::new()),
    ]
}

proptest! {
    #[test]
    fn valid_capabilities_round_trip(host in valid_host(), scope in valid_scope(), engine in "[a-z][a-z0-9]{0,10}") {
        for text in [
            format!("net.http:{host}"),
            format!("fs.read:{scope}"),
            format!("driver.host:{engine}"),
        ] {
            let cap: Capability = text.parse().map_err(|e| TestCaseError::fail(format!("{text}: {e}")))?;
            prop_assert_eq!(cap.to_string(), text);
        }
    }

    #[test]
    fn hostile_hosts_are_rejected(h in hostile_host()) {
        prop_assert!(h.parse::<HostPattern>().is_err(), "accepted {:?}", h);
        let cap = format!("net.http:{h}");
        prop_assert!(cap.parse::<Capability>().is_err(), "accepted {:?}", cap);
    }

    #[test]
    fn hostile_scopes_are_rejected(s in hostile_scope()) {
        prop_assert!(s.parse::<FsScope>().is_err(), "accepted {:?}", s);
        let cap = format!("fs.read:{s}");
        prop_assert!(cap.parse::<Capability>().is_err(), "accepted {:?}", cap);
    }

    #[test]
    fn arbitrary_text_never_panics_and_accepted_values_are_safe(s in any::<String>()) {
        if let Ok(p) = s.parse::<HostPattern>() {
            prop_assert!(p.to_string().is_ascii());
            prop_assert!(!p.host().chars().all(|c| c.is_ascii_digit() || c == '.'));
        }
        if let Ok(scope) = s.parse::<FsScope>() {
            let roots = FsRoots {
                plugin_dir: Path::new("p").to_path_buf(),
                data_dir: Path::new("d").to_path_buf(),
                home_dir: Some(Path::new("h").to_path_buf()),
            };
            let resolved = scope.resolve(&roots).expect("home is set");
            prop_assert!(resolved.components().all(|c| matches!(c, Component::Normal(_))));
        }
        let _ = s.parse::<Capability>();
    }

    #[test]
    fn arbitrary_text_with_a_capability_prefix_never_panics(
        kind in prop_oneof![Just("net.http:"), Just("fs.read:"), Just("driver.host:")],
        s in any::<String>(),
    ) {
        let _ = format!("{kind}{s}").parse::<Capability>();
    }

    #[test]
    fn matching_is_consistent_with_the_pattern(host in valid_host(), port in 1u16..=65535) {
        let p: HostPattern = host.parse().map_err(|e| TestCaseError::fail(format!("{e}")))?;
        let own = p.matches(p.host(), p.port().unwrap_or(443));
        let sub = p.matches(&format!("x.{}", p.host()), p.port().unwrap_or(443));
        if p.is_wildcard() {
            prop_assert!(sub && !own);
        } else {
            prop_assert!(own && !sub);
        }
        if p.port().is_some_and(|q| q != port) {
            prop_assert!(!p.matches(p.host(), port) || p.is_wildcard());
        }
    }
}
