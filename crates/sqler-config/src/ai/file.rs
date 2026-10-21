// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `ai.toml` wire format (version 1), with version dispatch and serialisation.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::{AiConfigError, AiConfigProblem};
use super::limits::{AI_MAX_ROWS_RANGE, AI_TIMEOUT_SECS_RANGE, AiLimits, check_limit};
use super::model::{
    AiConfig, ModelAssignments, ModelChoice, ProviderConfig, ProviderId, ProviderKind,
};
use super::parse_message;
use super::redaction::RedactionList;
use crate::ConfigError;

/// Version of the `ai.toml` format this build writes and reads up to.
pub const AI_CONFIG_FORMAT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default, rename = "provider", skip_serializing_if = "Vec::is_empty")]
    providers: Vec<WireProvider>,
    #[serde(default, skip_serializing_if = "WireModels::is_empty")]
    models: WireModels,
    #[serde(default, skip_serializing_if = "WireRedaction::is_empty")]
    redaction: WireRedaction,
    #[serde(default, skip_serializing_if = "WireLimits::is_empty")]
    limits: WireLimits,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProvider {
    id: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    api_key_in_keychain: bool,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct WireModels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<WireChoice>,
    #[serde(default, rename = "nl-to-sql", skip_serializing_if = "Option::is_none")]
    nl_to_sql: Option<WireChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    explain: Option<WireChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fix: Option<WireChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exploration: Option<WireChoice>,
}

impl WireModels {
    fn is_empty(&self) -> bool {
        self.default.is_none()
            && self.nl_to_sql.is_none()
            && self.explain.is_none()
            && self.fix.is_none()
            && self.exploration.is_none()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireChoice {
    provider: String,
    model: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct WireRedaction {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra: Vec<String>,
}

impl WireRedaction {
    fn is_empty(&self) -> bool {
        self.extra.is_empty()
    }
}

/// Limits are `i64` on the wire so a negative value is a typed range error, not a parse error.
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct WireLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_rows: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    unlimited: bool,
}

impl WireLimits {
    fn is_empty(&self) -> bool {
        self.max_rows.is_none() && self.timeout_secs.is_none() && !self.unlimited
    }
}

// serde's `skip_serializing_if` passes a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

fn invalid(path: &Path, message: String) -> AiConfigError {
    ConfigError::Invalid {
        path: path.to_owned(),
        message,
    }
    .into()
}

/// Parses and validates the text of `ai.toml`.
pub(super) fn parse(path: &Path, text: &str) -> Result<AiConfig, AiConfigError> {
    let parse_err = |e: &toml::de::Error| {
        AiConfigError::from(ConfigError::Parse {
            path: path.to_owned(),
            message: parse_message(text, e),
        })
    };
    let table: toml::Table = toml::from_str(text).map_err(|e| parse_err(&e))?;
    match table.get("version") {
        None => Err(invalid(path, "missing required key `version`".to_owned())),
        Some(toml::Value::Integer(n)) if *n >= 0 => {
            if *n == i64::from(AI_CONFIG_FORMAT_VERSION) {
                let file: File = toml::from_str(text).map_err(|e| parse_err(&e))?;
                let config = convert(file).and_then(|c| c.validate().map(|()| c));
                config.map_err(|problem| AiConfigError::Invalid {
                    path: path.to_owned(),
                    problem,
                })
            } else if *n == 0 {
                Err(invalid(
                    path,
                    "unsupported version 0 (the first format is 1)".to_owned(),
                ))
            } else {
                Err(invalid(
                    path,
                    format!(
                        "written by a newer SQLer (version {n}, this build reads up to {AI_CONFIG_FORMAT_VERSION})"
                    ),
                ))
            }
        }
        Some(_) => Err(ConfigError::Parse {
            path: path.to_owned(),
            message: "version must be a non-negative integer".to_owned(),
        }
        .into()),
    }
}

fn convert_choice(w: Option<WireChoice>) -> Result<Option<ModelChoice>, AiConfigProblem> {
    w.map(|c| Ok(ModelChoice::new(ProviderId::new(&c.provider)?, c.model)))
        .transpose()
}

fn convert(file: File) -> Result<AiConfig, AiConfigProblem> {
    let mut providers = Vec::with_capacity(file.providers.len());
    for w in file.providers {
        let id = ProviderId::new(&w.id)?;
        let kind =
            ProviderKind::from_id(&w.kind).ok_or_else(|| AiConfigProblem::UnknownProviderKind {
                provider: id.clone(),
                kind: w.kind.clone(),
            })?;
        let mut p = ProviderConfig::new(id, kind)
            .with_models(w.models)
            .with_api_key_in_keychain(w.api_key_in_keychain);
        p.endpoint = w.endpoint;
        providers.push(p);
    }
    let models = ModelAssignments {
        default: convert_choice(file.models.default)?,
        nl_to_sql: convert_choice(file.models.nl_to_sql)?,
        explain: convert_choice(file.models.explain)?,
        fix: convert_choice(file.models.fix)?,
        exploration: convert_choice(file.models.exploration)?,
    };
    let mut limits = AiLimits::default();
    if let Some(v) = file.limits.max_rows {
        limits.max_rows = check_limit("max_rows", v, &AI_MAX_ROWS_RANGE)?;
    }
    if let Some(v) = file.limits.timeout_secs {
        limits.timeout_secs = check_limit("timeout_secs", v, &AI_TIMEOUT_SECS_RANGE)?;
    }
    limits.unlimited = file.limits.unlimited;
    Ok(AiConfig {
        providers,
        models,
        redaction: RedactionList::new(file.redaction.extra)?,
        limits,
    })
}

fn wire_choice(c: Option<&ModelChoice>) -> Option<WireChoice> {
    c.map(|c| WireChoice {
        provider: c.provider.to_string(),
        model: c.model.clone(),
    })
}

/// Serialises the configuration as a `version = 1` file.
pub(super) fn serialize(path: &Path, config: &AiConfig) -> Result<String, ConfigError> {
    let file = File {
        version: AI_CONFIG_FORMAT_VERSION,
        providers: config
            .providers
            .iter()
            .map(|p| WireProvider {
                id: p.id.to_string(),
                kind: p.kind.as_str().to_owned(),
                endpoint: p.endpoint.clone(),
                models: p.models.clone(),
                api_key_in_keychain: p.api_key_in_keychain,
            })
            .collect(),
        models: WireModels {
            default: wire_choice(config.models.default.as_ref()),
            nl_to_sql: wire_choice(config.models.nl_to_sql.as_ref()),
            explain: wire_choice(config.models.explain.as_ref()),
            fix: wire_choice(config.models.fix.as_ref()),
            exploration: wire_choice(config.models.exploration.as_ref()),
        },
        redaction: WireRedaction {
            extra: config.redaction.extra().to_vec(),
        },
        limits: if config.limits == AiLimits::default() {
            WireLimits::default()
        } else {
            WireLimits {
                max_rows: Some(i64::from(config.limits.max_rows)),
                timeout_secs: Some(i64::from(config.limits.timeout_secs)),
                unlimited: config.limits.unlimited,
            }
        },
    };
    toml::to_string_pretty(&file).map_err(|e| ConfigError::Serialize {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"version = 1

[[provider]]
id = "local"
kind = "ollama"
endpoint = "http://127.0.0.1:11434"
models = ["qwen2.5-coder:7b"]

[[provider]]
id = "claude"
kind = "anthropic"
models = ["sonnet", "haiku"]
api_key_in_keychain = true

[models]
default = { provider = "local", model = "qwen2.5-coder:7b" }
nl-to-sql = { provider = "claude", model = "sonnet" }

[redaction]
extra = ["IBAN", "card_no"]

[limits]
max_rows = 500
timeout_secs = 10
unlimited = true
"#;

    fn p() -> &'static Path {
        Path::new("ai.toml")
    }

    fn err(text: &str) -> String {
        parse(p(), text).unwrap_err().to_string()
    }

    fn problem(text: &str) -> AiConfigProblem {
        match parse(p(), text).unwrap_err() {
            AiConfigError::Invalid { problem, .. } => problem,
            other => panic!("expected a problem, got {other:?}"),
        }
    }

    #[test]
    fn empty_version_1_gives_defaults() {
        let c = parse(p(), "version = 1\n").unwrap();
        assert_eq!(c, AiConfig::default());
        assert!(c.providers.is_empty() && c.models.default.is_none());
        assert_eq!(c.limits.max_rows, 1_000);
        assert_eq!(c.limits.timeout_secs, 30);
        assert!(!c.limits.unlimited && c.redaction.extra().is_empty());
    }

    #[test]
    fn full_example_parses_and_round_trips() {
        let c = parse(p(), FULL).unwrap();
        assert_eq!(c.providers.len(), 2);
        assert_eq!(c.providers[0].kind, ProviderKind::Ollama);
        assert_eq!(
            c.providers[0].endpoint.as_deref(),
            Some("http://127.0.0.1:11434")
        );
        assert!(c.providers[1].api_key_in_keychain && c.providers[1].endpoint.is_none());
        assert_eq!(
            c.models.default.as_ref().unwrap().provider.as_str(),
            "local"
        );
        assert_eq!(c.models.nl_to_sql.as_ref().unwrap().model, "sonnet");
        assert_eq!(c.redaction.extra(), ["iban", "card_no"]);
        assert_eq!(
            (c.limits.max_rows, c.limits.timeout_secs, c.limits.unlimited),
            (500, 10, true)
        );
        let text = serialize(p(), &c).unwrap();
        assert!(text.starts_with("version = 1\n"), "{text}");
        assert_eq!(parse(p(), &text).unwrap(), c);
    }

    #[test]
    fn default_config_serialises_to_the_version_line_only() {
        let text = serialize(p(), &AiConfig::default()).unwrap();
        assert_eq!(text, "version = 1\n");
    }

    #[test]
    fn version_is_required_and_checked() {
        assert!(err("").contains("missing required key `version`"));
        assert!(err("version = 0").contains("unsupported version 0"));
        assert!(err("version = 2").contains("newer SQLer (version 2, this build reads up to 1)"));
        assert!(err("version = \"1\"").contains("non-negative integer"));
        assert!(matches!(
            parse(p(), "version = -1").unwrap_err(),
            AiConfigError::File(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(err("version = 1\nextra = 1").contains("unknown field"));
        assert!(err("version = 1\n[redaction]\ndefaults = []").contains("unknown field"));
        assert!(err("version = 1\n[limits]\nbogus = 1").contains("unknown field"));
        assert!(
            err("version = 1\n[models]\nbogus = { provider = \"a\", model = \"b\" }")
                .contains("unknown field")
        );
    }

    #[test]
    fn a_key_in_the_file_is_a_parse_error_that_does_not_echo_it() {
        let text =
            "version = 1\n[[provider]]\nid = \"a\"\nkind = \"openai\"\napi_key = \"sk-SECRET\"\n";
        let r = parse(p(), text).unwrap_err();
        assert!(
            matches!(r, AiConfigError::File(ConfigError::Parse { .. })),
            "{r:?}"
        );
        let msg = r.to_string();
        assert!(
            msg.contains("unknown field") && !msg.contains("sk-SECRET"),
            "{msg}"
        );
        let msg = err("version = 1\nkey = \"sk-SECRET\" oops");
        assert!(
            !msg.contains("sk-SECRET") && msg.contains("line 2"),
            "{msg}"
        );
    }

    #[test]
    fn each_problem_is_reported() {
        let prov = "version = 1\n[[provider]]\nid = \"a\"\nkind = \"ollama\"\nmodels = [\"m\"]\n";
        assert!(matches!(
            problem("version = 1\n[[provider]]\nid = \"A b\"\nkind = \"ollama\"\n"),
            AiConfigProblem::InvalidProviderId { .. }
        ));
        assert!(matches!(
            problem(&format!(
                "{prov}[[provider]]\nid = \"a\"\nkind = \"ollama\"\n"
            )),
            AiConfigProblem::DuplicateProvider(_)
        ));
        assert!(matches!(
            problem("version = 1\n[[provider]]\nid = \"a\"\nkind = \"gpt\"\n"),
            AiConfigProblem::UnknownProviderKind { .. }
        ));
        let e = problem(
            "version = 1\n[[provider]]\nid = \"a\"\nkind = \"ollama\"\nendpoint = \"ftp://secret-host\"\n",
        );
        assert!(matches!(e, AiConfigProblem::InvalidEndpoint { .. }));
        assert!(!e.to_string().contains("secret-host"));
        assert!(matches!(
            problem("version = 1\n[[provider]]\nid = \"a\"\nkind = \"ollama\"\nmodels = [\" \"]\n"),
            AiConfigProblem::EmptyModelName { .. }
        ));
        assert!(matches!(
            problem(&format!(
                "{prov}[models]\nfix = {{ provider = \"zz\", model = \"m\" }}\n"
            )),
            AiConfigProblem::UnknownProvider {
                use_case: Some(_),
                ..
            }
        ));
        assert!(matches!(
            problem(&format!(
                "{prov}[models]\ndefault = {{ provider = \"a\", model = \"x\" }}\n"
            )),
            AiConfigProblem::UnknownModel { use_case: None, .. }
        ));
        assert!(matches!(
            problem(&format!(
                "{prov}[models]\ndefault = {{ provider = \"A\", model = \"m\" }}\n"
            )),
            AiConfigProblem::InvalidProviderId { .. }
        ));
        assert!(matches!(
            problem("version = 1\n[redaction]\nextra = [\"\"]\n"),
            AiConfigProblem::InvalidRedactionEntry { index: 0, .. }
        ));
        for (limits, key) in [
            ("max_rows = 0", "max_rows"),
            ("max_rows = -1", "max_rows"),
            ("max_rows = 1000001", "max_rows"),
            ("timeout_secs = 0", "timeout_secs"),
            ("timeout_secs = 3601", "timeout_secs"),
        ] {
            assert!(
                matches!(
                    problem(&format!("version = 1\n[limits]\n{limits}\n")),
                    AiConfigProblem::LimitOutOfRange { key: k, .. } if k == key
                ),
                "{limits}"
            );
        }
        assert!(
            parse(
                p(),
                "version = 1\n[limits]\nmax_rows = 1000000\ntimeout_secs = 3600\n"
            )
            .is_ok()
        );
    }

    #[test]
    fn invalid_problem_names_the_file() {
        let e = err("version = 1\n[limits]\nmax_rows = 0\n");
        assert!(e.starts_with("ai.toml: limits.max_rows is 0"), "{e}");
    }

    #[test]
    fn redaction_defaults_survive_a_round_trip() {
        let text = "version = 1\n[redaction]\nextra = [\"password\", \"iban\"]\n";
        let c = parse(p(), text).unwrap();
        assert_eq!(c.redaction.extra(), ["iban"]);
        assert!(c.redaction.matches_column("PASSWORD_hash"));
        let out = serialize(p(), &c).unwrap();
        assert!(!out.contains("password"), "{out}");
    }

    #[test]
    fn prod_still_caps_with_unlimited() {
        use sqler_core::Environment;
        let c = parse(p(), FULL).unwrap();
        assert!(c.limits.unlimited);
        let prod = c.limits.effective(Some(Environment::Prod));
        assert_eq!(prod.max_rows, Some(500));
        assert!(prod.timeout.is_some());
        assert_eq!(c.limits.effective(Some(Environment::Dev)).max_rows, None);
    }
}
