// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Every `ManifestError` variant is reached by one example; the fixtures parse.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::assert_is_empty,
    clippy::too_many_lines
)]

use sqler_plugin_api::{
    API_VERSION, ApiVersion, Capability, CapabilityError, CompatError, DriverKind, IdError,
    Manifest, ManifestError, ScopeError, SettingValue, command_id,
};

const FULL: &str = include_str!("fixtures/full.toml");
const MINIMAL: &str = include_str!("fixtures/minimal.toml");

const HEAD: &str = "[plugin]\nid = \"mini\"\nname = \"Mini\"\nversion = \"0.1.0\"\napi = \"0.1\"\n";

fn with_caps(caps: &str, rest: &str) -> String {
    format!("{HEAD}capabilities = [{caps}]\n{rest}")
}

fn err(text: &str) -> ManifestError {
    Manifest::parse(text).expect_err("manifest should be rejected")
}

#[test]
fn full_fixture_round_trips_all_fields() {
    let m = Manifest::parse(FULL).expect("full fixture parses");
    assert_eq!(m.id().as_str(), "demo-plugin");
    assert_eq!(m.name(), "Demo plugin");
    assert_eq!(m.version().to_string(), "1.2.3");
    assert_eq!(m.api(), API_VERSION);
    assert_eq!(m.description(), Some("Exercises every key."));
    assert_eq!(m.authors(), ["Ada", "Grace"]);
    assert_eq!(m.license(), Some("Apache-2.0"));
    assert_eq!(m.capabilities().len(), 8);
    assert!(
        m.capabilities()
            .contains(&"fs.read:data/cache".parse::<Capability>().unwrap())
    );
    assert_eq!(m.entry().unwrap().component, "bin/plugin.wasm");
    let c = m.contributes();
    assert_eq!(c.commands.len(), 1);
    assert_eq!(
        command_id(m.id(), &c.commands[0].id),
        "plugin:demo-plugin:run"
    );
    assert_eq!(c.panels[0].title, "Main");
    assert_eq!(c.status_items[0].tooltip.as_deref(), Some("Plugin state"));
    assert_eq!(c.menus[0].object_kinds.len(), 2);
    assert_eq!(c.exporters[0].extension, "tsv");
    assert_eq!(c.completion.as_ref().unwrap().drivers.len(), 1);
    assert_eq!(
        c.ai_tools[0].input_schema["properties"]["q"]["type"],
        "string"
    );
    assert!(matches!(c.drivers[0].kind, DriverKind::Host { .. }));
    assert_eq!(c.drivers[0].dialect.as_deref(), Some("duckdb"));
    assert_eq!(c.drivers[1].kind, DriverKind::Guest);
    assert_eq!(c.settings[0].default, SettingValue::Integer(10));
    assert_eq!(c.settings[1].default, SettingValue::Boolean(false));
    m.check_api(API_VERSION).expect("compatible");
    let digest = sqler_plugin_api::ComponentDigest::of(b"wasm");
    assert!(
        m.grant_key(Some(&digest))
            .unwrap()
            .to_string()
            .starts_with("demo-plugin@1.2.3#")
    );
}

#[test]
fn minimal_is_declarative_only() {
    let m = Manifest::parse(MINIMAL).expect("parses");
    assert!(m.entry().is_none());
    assert!(m.capabilities().is_empty());
}

#[test]
fn crlf_gives_the_same_hash() {
    let lf = Manifest::parse(FULL).unwrap();
    let crlf = Manifest::parse(&FULL.replace('\n', "\r\n")).unwrap();
    assert_eq!(lf.hash(), crlf.hash());
    assert_eq!(lf, crlf);
}

#[test]
fn compat_error() {
    let m = Manifest::parse(MINIMAL).unwrap();
    let host = ApiVersion::new(0, 2, 0);
    assert_eq!(
        m.check_api(host),
        Err(CompatError::Incompatible {
            required: m.api(),
            host
        })
    );
}

#[test]
fn size_and_encoding() {
    let big = format!("{MINIMAL}# {}\n", "x".repeat(70_000));
    assert!(matches!(
        err(&big),
        ManifestError::TooLarge { max: 65_536, .. }
    ));
    assert!(matches!(
        Manifest::parse_bytes(&vec![b' '; 65_537]),
        Err(ManifestError::TooLarge { .. })
    ));
    assert_eq!(
        Manifest::parse_bytes(&[0xff, 0xfe]),
        Err(ManifestError::NotUtf8)
    );
    assert!(Manifest::parse_bytes(MINIMAL.as_bytes()).is_ok());
}

#[test]
fn syntax_and_depth() {
    assert!(matches!(err("[plugin\nid ="), ManifestError::Syntax { .. }));
    let ManifestError::Syntax { line, .. } = err("[plugin]\nid = \"a\"\nx = = 1\n") else {
        panic!("syntax");
    };
    assert_eq!(line, Some(3));
    let deep = format!("x = {}{}", "[".repeat(17), "]".repeat(17));
    assert_eq!(err(&deep), ManifestError::TooDeep);
    // brackets in strings and comments do not count
    let ok = format!("{MINIMAL}# {}\n", "[".repeat(40));
    assert!(Manifest::parse(&ok).is_ok());
    let dotted = format!("{MINIMAL}\n[x]\n{} = 1\n", vec!["a"; 40].join("."));
    assert_eq!(err(&dotted), ManifestError::TooDeep);
}

#[test]
fn field_errors() {
    use ManifestError as E;
    assert!(matches!(err(""), E::MissingField { field } if field == "plugin"));
    assert!(matches!(err("x = 1"), E::UnknownField { field } if field == "x"));
    assert!(matches!(err("plugin = 1"), E::WrongType { .. }));
    assert!(matches!(
        err(HEAD),
        E::MissingField { field } if field == "plugin.capabilities"
    ));
    assert!(matches!(
        err(&with_caps("", "[plugin.extra]\n")),
        E::UnknownField { .. } | E::WrongType { .. }
    ));
    assert!(matches!(
        err(&HEAD.replace("id = \"mini\"", "id = 5")),
        E::WrongType { field, .. } if field == "plugin.id"
    ));
    assert!(matches!(
        err(&HEAD.replace("id = \"mini\"", "id = \"Mini\"")),
        E::BadId {
            reason: IdError::BadCharacter,
            ..
        }
    ));
    assert!(matches!(
        err(&format!(
            "{}capabilities = []\n",
            HEAD.replace("0.1.0", "x1")
        )),
        E::BadVersion { .. }
    ));
    assert!(matches!(
        err(&format!(
            "{}capabilities = []\n",
            HEAD.replace("api = \"0.1\"", "api = \"one\"")
        )),
        E::BadApi { .. }
    ));
}

#[test]
fn text_rules() {
    use ManifestError as E;
    let name = |n: &str| format!("{}capabilities = []\n", HEAD.replace("\"Mini\"", n));
    assert!(matches!(err(&name("\"a\\u0007b\"")), E::BadText { field } if field == "plugin.name"));
    assert!(matches!(err(&name("\"a\\u202Eb\"")), E::BadText { .. }));
    assert!(matches!(err(&name("\"a\\u2066b\"")), E::BadText { .. }));
    assert!(matches!(err(&name("\"a\\u0085b\"")), E::BadText { .. }));
    assert!(matches!(err(&name("\"a\\nb\"")), E::BadText { .. }));
    assert!(matches!(err(&name("\"\"")), E::BadText { .. }));
    assert!(matches!(
        err(&name(&format!("\"{}\"", "a".repeat(81)))),
        E::TooLong { max: 80, .. }
    ));
    assert!(Manifest::parse(&name(&format!("\"{}\"", "é".repeat(80)))).is_ok());
    let tip = with_caps(
        "",
        &format!(
            "[entry]\ncomponent = \"a.wasm\"\n[[status-items]]\nid = \"s\"\ntooltip = \"{}\"\n",
            "t".repeat(201)
        ),
    );
    assert!(matches!(err(&tip), E::TooLong { max: 200, .. }));
    let desc = format!(
        "{HEAD}description = \"{}\"\ncapabilities = []\n",
        "d".repeat(1025)
    );
    assert!(matches!(err(&desc), E::TooLong { max: 1024, .. }));
}

#[test]
fn capability_errors() {
    use ManifestError as E;
    assert!(matches!(
        err(&with_caps("\"db.read\", \"net.http:127.0.0.1\"", "")),
        E::Capability {
            index: 1,
            source: CapabilityError::BadHostPattern { .. }
        }
    ));
    assert!(matches!(
        err(&with_caps("\"db.read\", \"db.read\"", "")),
        E::DuplicateCapability { capability } if capability == "db.read"
    ));
    let long = format!("\"fs.read:data/{}\"", "d".repeat(60));
    match err(&with_caps(&format!("{long}, {long}"), "")) {
        E::DuplicateCapability { capability } => assert_eq!(capability.chars().count(), 64),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        err(&with_caps("\"db.write\"", "")),
        E::CapabilityRequires { requires, .. } if requires == "db.read"
    ));
    assert!(matches!(err(&with_caps("5", "")), E::WrongType { .. }));
    let many: Vec<String> = (0..33).map(|i| format!("\"fs.read:data/d{i}\"")).collect();
    assert!(matches!(
        err(&with_caps(&many.join(","), "")),
        E::TooMany { max: 32, .. }
    ));
    let ok: Vec<String> = (0..32).map(|i| format!("\"fs.read:data/d{i}\"")).collect();
    assert!(Manifest::parse(&with_caps(&ok.join(","), "")).is_ok());
}

#[test]
fn contribution_errors() {
    use ManifestError as E;
    let entry = "[entry]\ncomponent = \"a.wasm\"\n";
    let cmd = "[[commands]]\nid = \"run\"\ntitle = \"Run\"\n";
    assert!(matches!(
        err(&with_caps("", &format!("{entry}{cmd}{cmd}"))),
        E::DuplicateContribution {
            kind: "command",
            ..
        }
    ));
    assert!(matches!(
        err(&with_caps("", &format!("{entry}{cmd}[[menus]]\ncommand = \"other\"\nlocation = \"editor\"\n"))),
        E::UnknownCommand { menu } if menu == "menus[0]"
    ));
    assert!(matches!(
        err(&with_caps(
            "",
            &format!(
                "{entry}{cmd}[[menus]]\ncommand = \"run\"\nlocation = \"editor\"\nobject-kinds = [\"table\"]\n"
            )
        )),
        E::UnknownField { .. }
    ));
    assert!(matches!(
        err(&with_caps("", &format!("{entry}[[panels]]\nid = \"p\"\ntitle = \"P\"\n"))),
        E::MissingCapability { capability, .. } if capability == "ui.panel"
    ));
    assert!(matches!(
        err(&with_caps("", &format!("{entry}[[ai-tools]]\nname = \"t\"\ndescription = \"d\"\n[ai-tools.input-schema]\ntype = \"object\"\n"))),
        E::MissingCapability { capability, .. } if capability == "ai.tool"
    ));
    assert!(matches!(
        err(&with_caps("", "[[drivers]]\nid = \"d\"\nname = \"D\"\nkind = \"host\"\nengine = \"duckdb\"\n")),
        E::MissingCapability { capability, .. } if capability == "driver.host:duckdb"
    ));
    assert!(matches!(
        err(&with_caps(
            "\"driver.host:other\"",
            "[[drivers]]\nid = \"d\"\nname = \"D\"\nkind = \"host\"\nengine = \"duckdb\"\n"
        )),
        E::MissingCapability { .. }
    ));
    assert!(
        Manifest::parse(&with_caps(
            "\"driver.host:duckdb\"",
            "[[drivers]]\nid = \"d\"\nname = \"D\"\nkind = \"host\"\nengine = \"duckdb\"\n"
        ))
        .is_ok()
    );
    assert!(matches!(
        err(&with_caps(
            "\"driver.guest\"",
            &format!(
                "{entry}[[drivers]]\nid = \"d\"\nname = \"D\"\nkind = \"guest\"\nengine = \"x\"\n"
            )
        )),
        E::UnknownField { .. }
    ));
    assert!(matches!(
        err(&with_caps("\"driver.guest\"", "[[drivers]]\nid = \"d\"\nname = \"D\"\nkind = \"guest\"\n")),
        E::MissingEntry { contribution } if contribution == "drivers"
    ));
    assert!(
        matches!(err(&with_caps("", cmd)), E::MissingEntry { contribution } if contribution == "commands")
    );
    assert!(matches!(
        err(&with_caps(
            "",
            &format!("{entry}[[exporters]]\nid = \"e\"\nlabel = \"E\"\nextension = \"TXT\"\n")
        )),
        E::BadId {
            reason: IdError::BadCharacter,
            ..
        }
    ));
    assert!(matches!(
        err(&with_caps(
            "",
            &format!(
                "{entry}[[exporters]]\nid = \"e\"\nlabel = \"E\"\nextension = \"{}\"\n",
                "x".repeat(17)
            )
        )),
        E::BadId {
            reason: IdError::TooLong,
            ..
        }
    ));
    let many: String = (0..65)
        .map(|i| format!("[[commands]]\nid = \"c{i}\"\ntitle = \"T\"\n"))
        .collect();
    assert!(matches!(
        err(&with_caps("", &format!("{entry}{many}"))),
        E::TooMany { max: 64, .. }
    ));
    assert!(matches!(
        err(&with_caps(
            "",
            &format!("{entry}[[settings]]\nkey = \"k\"\nlabel = \"K\"\ndefault = [1]\n")
        )),
        E::WrongType { .. }
    ));
    assert!(matches!(
        err(&with_caps(
            "",
            "[[settings]]\nkey = \"K\"\nlabel = \"K\"\ndefault = 1\n"
        )),
        E::BadId { .. }
    ));
}

#[test]
fn entry_and_schema_errors() {
    use ManifestError as E;
    for (path, want) in [
        ("../a.wasm", ScopeError::Traversal),
        ("/a.wasm", ScopeError::Absolute),
        ("a\\\\b.wasm", ScopeError::BadCharacter),
        ("a.txt", ScopeError::BadCharacter),
        ("con.wasm", ScopeError::Reserved),
        (".hidden/a.wasm", ScopeError::HiddenSegment),
    ] {
        let text = with_caps("", &format!("[entry]\ncomponent = \"{path}\"\n"));
        assert!(
            matches!(err(&text), E::BadEntryPath { reason, .. } if reason == want),
            "{path}: {:?}",
            err(&text)
        );
    }
    let tool = |schema: &str| {
        with_caps(
            "\"ai.tool\"",
            &format!(
                "[entry]\ncomponent = \"a.wasm\"\n[[ai-tools]]\nname = \"t\"\ndescription = \"d\"\ninput-schema = {schema}\n"
            ),
        )
    };
    assert!(matches!(err(&tool("{ x = nan }")), E::BadSchema { tool } if tool == "t"));
    assert!(matches!(err(&tool("5")), E::WrongType { .. }));
    let big = format!("{{ d = \"{}\" }}", "x".repeat(17 * 1024));
    assert!(matches!(err(&tool(&big)), E::BadSchema { .. }));
    let ok = Manifest::parse(&tool("{ when = 1979-05-27T07:32:00Z }")).unwrap();
    assert_eq!(
        ok.contributes().ai_tools[0].input_schema["when"],
        "1979-05-27T07:32:00Z"
    );
}

#[test]
fn component_digest_binds_the_grant_key() {
    use sqler_plugin_api::{ComponentDigest, GrantKeyBuildError};
    let with_entry = Manifest::parse(
        "[plugin]\nid = \"demo-plugin\"\nname = \"D\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = []\n[entry]\ncomponent = \"d.wasm\"\n",
    )
    .unwrap();
    let decl = Manifest::parse(
        "[plugin]\nid = \"demo-plugin\"\nname = \"D\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = []\n",
    )
    .unwrap();
    let (one, two) = (ComponentDigest::of(b"one"), ComponentDigest::of(b"two"));

    // [entry] + digest: a key bound to the component
    let a = with_entry.grant_key(Some(&one)).unwrap();
    let b = with_entry.grant_key(Some(&two)).unwrap();
    assert_ne!(a, b);
    assert_eq!(a.component(), Some(&one));
    assert_eq!(
        a.to_string()
            .parse::<sqler_plugin_api::ParsedGrantKey>()
            .unwrap(),
        a
    );
    // [entry] without a digest: refused, never a key that a swapped .wasm would reuse
    assert_eq!(
        with_entry.grant_key(None),
        Err(GrantKeyBuildError::ComponentRequired)
    );
    // declarative without a digest: a key with nothing to bind
    let key = decl.grant_key(None).unwrap();
    assert!(key.component().is_none());
    assert!(!key.to_string().contains('+'));
    // declarative with a digest: refused
    assert_eq!(
        decl.grant_key(Some(&one)),
        Err(GrantKeyBuildError::NoComponent)
    );
}

#[test]
fn changing_capabilities_changes_the_grant_key() {
    let a = Manifest::parse(&with_caps("\"db.read\"", "")).unwrap();
    let b = Manifest::parse(&with_caps(
        "\"db.read\", \"ui.panel\"",
        "[entry]\ncomponent = \"a.wasm\"\n",
    ))
    .unwrap();
    let digest = sqler_plugin_api::ComponentDigest::of(b"w");
    assert_ne!(
        a.grant_key(None).unwrap(),
        b.grant_key(Some(&digest)).unwrap()
    );
    let d = b.capabilities().diff(a.capabilities());
    assert_eq!(d.added, vec![Capability::UiPanel]);
    assert!(d.removed.is_empty());
}
