// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `plugin-grants.toml` wire format (version 1).

use serde::{Deserialize, Serialize};

use super::model::{
    GrantDecision, GrantRecord, MAX_GRANT_CAPABILITIES, MAX_PLUGIN_GRANTS, PluginGrants,
};
use super::{GrantsLoad, parse_message};

/// Version of the `plugin-grants.toml` format this build writes and reads.
pub const PLUGIN_GRANTS_FORMAT_VERSION: u32 = 1;

/// Reads only the version, so a newer file is recognised whatever else it holds.
#[derive(Deserialize)]
struct Probe {
    version: Option<toml::Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default, rename = "grant")]
    grants: Vec<WireGrant>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireGrant {
    key: String,
    decision: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

/// Parses the file text. Never panics; messages carry line and column, never source text.
pub(super) fn parse(text: &str) -> GrantsLoad {
    let probe: Probe = match toml::from_str(text) {
        Ok(p) => p,
        Err(e) => return GrantsLoad::Invalid(parse_message(text, &e)),
    };
    match probe.version {
        Some(toml::Value::Integer(n)) if n >= 1 => {
            if n > i64::from(PLUGIN_GRANTS_FORMAT_VERSION) {
                return GrantsLoad::Newer(u32::try_from(n).unwrap_or(u32::MAX));
            }
        }
        Some(toml::Value::Integer(_)) => return GrantsLoad::Invalid("version must be 1".into()),
        Some(_) => return GrantsLoad::Invalid("version must be an integer".into()),
        None => return GrantsLoad::Invalid("missing `version`".into()),
    }
    let file: File = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => return GrantsLoad::Invalid(parse_message(text, &e)),
    };
    if file.grants.len() > MAX_PLUGIN_GRANTS {
        return GrantsLoad::Invalid(format!("more than {MAX_PLUGIN_GRANTS} grant records"));
    }
    let mut grants = PluginGrants::new();
    for (i, g) in file.grants.into_iter().enumerate() {
        let n = i + 1;
        if g.capabilities.len() > MAX_GRANT_CAPABILITIES {
            return GrantsLoad::Invalid(format!("grant {n}: too many capabilities"));
        }
        let record = match g.decision.as_str() {
            "approved" => GrantRecord::approved(g.key, g.capabilities),
            "denied" if g.capabilities.is_empty() => GrantRecord::denied(g.key),
            "denied" => {
                return GrantsLoad::Invalid(format!(
                    "grant {n}: a denied record lists capabilities"
                ));
            }
            _ => {
                return GrantsLoad::Invalid(format!(
                    "grant {n}: decision must be \"approved\" or \"denied\""
                ));
            }
        };
        let record = match record {
            Ok(r) => r,
            Err(e) => return GrantsLoad::Invalid(format!("grant {n}: {e}")),
        };
        if grants.for_plugin(record.plugin()).is_some() {
            return GrantsLoad::Invalid(format!("grant {n}: duplicate plugin id"));
        }
        grants.record(record);
    }
    GrantsLoad::Loaded(grants)
}

/// Serialises the book, sorted by plugin id, starting with `version = 1`.
pub(super) fn serialize(grants: &PluginGrants) -> Result<String, toml::ser::Error> {
    let file = File {
        version: PLUGIN_GRANTS_FORMAT_VERSION,
        grants: grants
            .iter()
            .map(|r| WireGrant {
                key: r.key().to_owned(),
                decision: match r.decision() {
                    GrantDecision::Approved => "approved",
                    GrantDecision::Denied => "denied",
                }
                .to_owned(),
                capabilities: r.capabilities().to_vec(),
            })
            .collect(),
    };
    toml::to_string(&file)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    const KEY: &str = "acme-sync@1.2.0#aa+bb";

    fn invalid(text: &str) -> String {
        match parse(text) {
            GrantsLoad::Invalid(m) => m,
            other => panic!("expected Invalid for {text:?}, got {other:?}"),
        }
    }

    fn one(decision: &str, caps: &str, key: &str) -> String {
        format!(
            "version = 1\n[[grant]]\nkey = \"{key}\"\ndecision = \"{decision}\"\ncapabilities = {caps}\n"
        )
    }

    #[test]
    fn version_cases() {
        invalid("");
        invalid("[[grant]]\n");
        invalid("version = 0\n");
        invalid("version = -1\n");
        invalid("version = \"1\"\n");
        invalid("version = 1.5\n");
        assert!(matches!(parse("version = 2\n"), GrantsLoad::Newer(2)));
        assert!(matches!(
            parse("version = 2\nfuture = true\n[[x]]\n"),
            GrantsLoad::Newer(2)
        ));
        assert!(matches!(
            parse("version = 99999999999\n"),
            GrantsLoad::Newer(u32::MAX)
        ));
        assert!(matches!(parse("version = 1\n"), GrantsLoad::Loaded(g) if g.is_empty()));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        invalid("version = 1\nextra = 1\n");
        invalid(&format!("{}extra = 1\n", one("approved", "[]", KEY)));
    }

    #[test]
    fn records_are_checked() {
        let GrantsLoad::Loaded(g) = parse(&one(
            "approved",
            "[\"db.write\", \"db.read\", \"db.read\"]",
            KEY,
        )) else {
            panic!("should load")
        };
        assert_eq!(
            g.for_plugin("acme-sync").unwrap().capabilities(),
            ["db.read", "db.write"]
        );
        invalid(&one("denied", "[\"db.read\"]", KEY));
        invalid(&one("maybe", "[]", KEY));
        invalid(&one("approved", "[]", "nokey"));
        invalid(&one("approved", "[]", "Acme@1#a"));
        invalid(&one("approved", "[]", "a@1#a"));
        invalid(&one("approved", "[]", "acme sync@1#a"));
        invalid(&one("approved", "[\"\"]", KEY));
        invalid(&one("approved", &format!("[\"{}\"]", "a".repeat(301)), KEY));
        invalid(&one("approved", "[\"db read\"]", KEY));
        invalid(&one("approved", "[\"db.\\u0007\"]", KEY));
        let many: Vec<String> = (0..33).map(|i| format!("\"c{i}\"")).collect();
        invalid(&one("approved", &format!("[{}]", many.join(",")), KEY));
        let long_key = format!("a-b@1#{}", "a".repeat(340));
        invalid(&one("approved", "[]", &long_key));
        let dup = format!(
            "{}{}",
            one("approved", "[]", KEY),
            "[[grant]]\nkey = \"acme-sync@2#a\"\ndecision = \"denied\"\n"
        );
        invalid(&dup);
    }

    #[test]
    fn crlf_parses() {
        let text = one("approved", "[\"db.read\"]", KEY).replace('\n', "\r\n");
        assert!(matches!(parse(&text), GrantsLoad::Loaded(g) if g.len() == 1));
    }

    #[test]
    fn serialisation_is_sorted_and_versioned() {
        let mut g = PluginGrants::new();
        g.record(GrantRecord::denied("zed@1#a".into()).unwrap());
        g.record(
            GrantRecord::approved(
                "alpha@1#a".into(),
                vec!["db.write".into(), "db.read".into()],
            )
            .unwrap(),
        );
        let text = serialize(&g).unwrap();
        assert!(text.starts_with("version = 1\n"));
        assert!(text.find("alpha@1").unwrap() < text.find("zed@1").unwrap());
        assert!(matches!(parse(&text), GrantsLoad::Loaded(back) if back == g));
    }

    #[test]
    fn too_many_records_are_rejected() {
        let mut text = String::from("version = 1\n");
        for i in 0..=MAX_PLUGIN_GRANTS {
            let _ = writeln!(
                text,
                "[[grant]]\nkey = \"p-{i}@1#a\"\ndecision = \"denied\""
            );
        }
        assert!(invalid(&text).contains("1024"));
    }
}
