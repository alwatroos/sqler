// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Domain types of the AI configuration: providers, model assignments and the whole [`AiConfig`].

use std::collections::HashSet;
use std::fmt;

use super::error::AiConfigProblem;
use super::limits::AiLimits;
use super::redaction::RedactionList;
use crate::secrets::{ProviderKeyRef, validate_id};

const MAX_ENDPOINT_BYTES: usize = 2048;

/// The id of an AI provider: 1 to 64 characters of lowercase letters, digits and `-`, starting
/// with a letter or digit (the connection id grammar).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    /// Validates and wraps an id.
    ///
    /// # Errors
    /// [`AiConfigProblem::InvalidProviderId`].
    pub fn new(id: &str) -> Result<Self, AiConfigProblem> {
        let bad = |reason: &str| AiConfigProblem::InvalidProviderId {
            id: id.to_owned(),
            reason: reason.to_owned(),
        };
        validate_id(id).map_err(|r| bad(&r.replace("connection id", "provider id")))?;
        if !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(bad(
                "provider id may only contain lowercase letters, digits and '-'",
            ));
        }
        Ok(Self(id.to_owned()))
    }

    /// The id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The keychain reference of this provider's API key.
    #[must_use]
    pub fn key_ref(&self) -> ProviderKeyRef {
        ProviderKeyRef::from_valid(&self.0)
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The API family a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProviderKind {
    /// A local or remote Ollama server.
    Ollama,
    /// Anthropic's API.
    Anthropic,
    /// `OpenAI`'s API (or a compatible one).
    OpenAi,
    /// Mistral's API.
    Mistral,
}

impl ProviderKind {
    /// The id used in the file (`ollama`, `anthropic`, `openai`, `mistral`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Mistral => "mistral",
        }
    }

    /// The kind with this id (exact match).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "ollama" => Some(Self::Ollama),
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAi),
            "mistral" => Some(Self::Mistral),
            _ => None,
        }
    }
}

/// One configured provider. The API key itself is never part of it: only whether one is kept in
/// the keychain, under [`ProviderConfig::key_ref`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProviderConfig {
    /// Unique id.
    pub id: ProviderId,
    /// The API family.
    pub kind: ProviderKind,
    /// Base URL; `None` for the provider's default.
    pub endpoint: Option<String>,
    /// The models offered by this provider.
    pub models: Vec<String>,
    /// Whether an API key is stored in the keychain.
    pub api_key_in_keychain: bool,
}

impl ProviderConfig {
    /// A provider with no endpoint, no models and no key.
    #[must_use]
    pub fn new(id: ProviderId, kind: ProviderKind) -> Self {
        Self {
            id,
            kind,
            endpoint: None,
            models: Vec::new(),
            api_key_in_keychain: false,
        }
    }

    /// Sets the endpoint.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Sets the model list.
    #[must_use]
    pub fn with_models(mut self, models: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.models = models.into_iter().map(Into::into).collect();
        self
    }

    /// Marks whether an API key is kept in the keychain.
    #[must_use]
    pub fn with_api_key_in_keychain(mut self, yes: bool) -> Self {
        self.api_key_in_keychain = yes;
        self
    }

    /// The keychain reference of the API key, when one is kept.
    #[must_use]
    pub fn key_ref(&self) -> Option<ProviderKeyRef> {
        self.api_key_in_keychain.then(|| self.id.key_ref())
    }
}

/// What an AI feature is used for. Each can have its own model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UseCase {
    /// Natural language to SQL.
    NlToSql,
    /// Explaining a query or result.
    Explain,
    /// Fixing a failing query.
    Fix,
    /// Exploring a schema or data.
    Exploration,
}

impl UseCase {
    /// Every use case.
    pub const ALL: [Self; 4] = [Self::NlToSql, Self::Explain, Self::Fix, Self::Exploration];

    /// The id used in the file.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NlToSql => "nl-to-sql",
            Self::Explain => "explain",
            Self::Fix => "fix",
            Self::Exploration => "exploration",
        }
    }

    /// The use case with this id (exact match).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|u| u.as_str() == id)
    }
}

impl fmt::Display for UseCase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A provider and one of its models.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelChoice {
    /// The provider.
    pub provider: ProviderId,
    /// The model, as listed by the provider.
    pub model: String,
}

impl ModelChoice {
    /// A choice of `model` on `provider`.
    #[must_use]
    pub fn new(provider: ProviderId, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

/// The `[models]` table: a default and an optional override per use case.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ModelAssignments {
    /// Used for every use case without its own entry.
    pub default: Option<ModelChoice>,
    /// Natural language to SQL.
    pub nl_to_sql: Option<ModelChoice>,
    /// Explaining.
    pub explain: Option<ModelChoice>,
    /// Fixing.
    pub fix: Option<ModelChoice>,
    /// Exploration.
    pub exploration: Option<ModelChoice>,
}

impl ModelAssignments {
    /// The model for `use_case`: its own entry, else the default.
    #[must_use]
    pub fn for_use_case(&self, use_case: UseCase) -> Option<&ModelChoice> {
        let own = match use_case {
            UseCase::NlToSql => &self.nl_to_sql,
            UseCase::Explain => &self.explain,
            UseCase::Fix => &self.fix,
            UseCase::Exploration => &self.exploration,
        };
        own.as_ref().or(self.default.as_ref())
    }

    fn entries(&self) -> impl Iterator<Item = (Option<UseCase>, &ModelChoice)> {
        [
            (None, &self.default),
            (Some(UseCase::NlToSql), &self.nl_to_sql),
            (Some(UseCase::Explain), &self.explain),
            (Some(UseCase::Fix), &self.fix),
            (Some(UseCase::Exploration), &self.exploration),
        ]
        .into_iter()
        .filter_map(|(u, c)| c.as_ref().map(|c| (u, c)))
    }
}

/// All AI settings (`ai.toml`). Plain data: it holds no secret.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct AiConfig {
    /// Configured providers, in file order.
    pub providers: Vec<ProviderConfig>,
    /// Which model serves which use case.
    pub models: ModelAssignments,
    /// Columns never sent to a provider.
    pub redaction: RedactionList,
    /// Row and time caps.
    pub limits: AiLimits,
}

impl AiConfig {
    /// The provider with this id.
    #[must_use]
    pub fn provider(&self, id: &ProviderId) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| &p.id == id)
    }

    /// The keychain references of every provider that keeps an API key.
    #[must_use]
    pub fn key_refs(&self) -> Vec<ProviderKeyRef> {
        self.providers
            .iter()
            .filter_map(ProviderConfig::key_ref)
            .collect()
    }

    /// Checks the whole configuration.
    ///
    /// # Errors
    /// The first [`AiConfigProblem`] found.
    pub fn validate(&self) -> Result<(), AiConfigProblem> {
        let mut seen = HashSet::new();
        for p in &self.providers {
            if !seen.insert(&p.id) {
                return Err(AiConfigProblem::DuplicateProvider(p.id.clone()));
            }
            if let Some(endpoint) = &p.endpoint
                && let Some(reason) = endpoint_problem(endpoint, p.api_key_in_keychain)
            {
                return Err(AiConfigProblem::InvalidEndpoint {
                    provider: p.id.clone(),
                    reason,
                });
            }
            if p.models.iter().any(|m| m.trim().is_empty()) {
                return Err(AiConfigProblem::EmptyModelName {
                    provider: p.id.clone(),
                });
            }
        }
        for (use_case, choice) in self.models.entries() {
            let Some(p) = self.provider(&choice.provider) else {
                return Err(AiConfigProblem::UnknownProvider {
                    use_case,
                    provider: choice.provider.to_string(),
                });
            };
            if !p.models.contains(&choice.model) {
                return Err(AiConfigProblem::UnknownModel {
                    use_case,
                    provider: p.id.clone(),
                    model: choice.model.clone(),
                });
            }
        }
        self.limits.validate()?;
        Ok(())
    }
}

/// Why `endpoint` is not acceptable, `None` when it is. Never quotes the endpoint.
fn endpoint_problem(endpoint: &str, has_api_key: bool) -> Option<&'static str> {
    if endpoint.len() > MAX_ENDPOINT_BYTES {
        return Some("longer than 2048 bytes");
    }
    if endpoint.chars().any(char::is_whitespace) {
        return Some("must not contain whitespace");
    }
    if endpoint.contains(['@', '?', '#']) {
        return Some("must not contain credentials, a query or a fragment");
    }
    let Some(rest) = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
    else {
        return Some("must start with http:// or https://");
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.starts_with(':') {
        return Some("must have a host");
    }
    if has_api_key && endpoint.starts_with("http://") && !is_loopback_authority(authority) {
        return Some("an API key needs https:// unless the host is local");
    }
    None
}

/// Whether the `host[:port]` authority is `localhost`, `127.0.0.0/8` or `[::1]`.
fn is_loopback_authority(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> ProviderId {
        ProviderId::new(s).unwrap()
    }

    fn cfg() -> AiConfig {
        let mut c = AiConfig::default();
        c.providers.push(
            ProviderConfig::new(id("local"), ProviderKind::Ollama)
                .with_endpoint("http://127.0.0.1:11434")
                .with_models(["m1", "m2"]),
        );
        c.providers.push(
            ProviderConfig::new(id("claude"), ProviderKind::Anthropic)
                .with_models(["c1"])
                .with_api_key_in_keychain(true),
        );
        c
    }

    #[test]
    fn provider_ids() {
        assert!(ProviderId::new("a").is_ok());
        assert!(ProviderId::new(&"a".repeat(64)).is_ok());
        let long = "a".repeat(65);
        for bad in [
            "",
            long.as_str(),
            "A",
            "a_b",
            "a.b",
            "-a",
            "a b",
            "a:b",
            "é",
        ] {
            assert!(
                matches!(
                    ProviderId::new(bad),
                    Err(AiConfigProblem::InvalidProviderId { .. })
                ),
                "{bad:?}"
            );
        }
        let e = ProviderId::new("A").unwrap_err().to_string();
        assert!(e.contains("lowercase"), "{e}");
    }

    #[test]
    fn key_refs_follow_the_flag() {
        let c = cfg();
        assert_eq!(c.providers[0].key_ref(), None);
        let r = c.providers[1].key_ref().unwrap();
        assert_eq!(r.to_string(), "claude:ai-provider-key");
        assert_eq!(c.key_refs(), vec![r]);
        assert_eq!(id("x").key_ref().provider_id(), "x");
    }

    #[test]
    fn kinds_and_use_cases_round_trip() {
        for k in [
            ProviderKind::Ollama,
            ProviderKind::Anthropic,
            ProviderKind::OpenAi,
            ProviderKind::Mistral,
        ] {
            assert_eq!(ProviderKind::from_id(k.as_str()), Some(k));
        }
        assert_eq!(ProviderKind::from_id("gpt"), None);
        for u in UseCase::ALL {
            assert_eq!(UseCase::from_id(u.as_str()), Some(u));
            assert_eq!(u.to_string(), u.as_str());
        }
        assert_eq!(UseCase::from_id("default"), None);
    }

    #[test]
    fn for_use_case_falls_back_to_default() {
        let mut m = ModelAssignments::default();
        assert!(m.for_use_case(UseCase::Fix).is_none());
        m.default = Some(ModelChoice::new(id("local"), "m1"));
        m.fix = Some(ModelChoice::new(id("local"), "m2"));
        assert_eq!(m.for_use_case(UseCase::Fix).unwrap().model, "m2");
        assert_eq!(m.for_use_case(UseCase::Explain).unwrap().model, "m1");
    }

    #[test]
    fn valid_config_passes() {
        let mut c = cfg();
        c.models.default = Some(ModelChoice::new(id("local"), "m1"));
        c.models.explain = Some(ModelChoice::new(id("claude"), "c1"));
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(AiConfig::default().validate(), Ok(()));
    }

    #[test]
    fn duplicate_provider() {
        let mut c = cfg();
        c.providers
            .push(ProviderConfig::new(id("local"), ProviderKind::Mistral));
        assert_eq!(
            c.validate(),
            Err(AiConfigProblem::DuplicateProvider(id("local")))
        );
    }

    #[test]
    fn endpoint_rules() {
        let ok = [
            "http://127.0.0.1:11434",
            "https://api.example.com/v1",
            "http://[::1]:11434",
        ];
        for e in ok {
            assert_eq!(endpoint_problem(e, false), None, "{e}");
        }
        let long = format!("http://h/{}", "a".repeat(2048));
        for bad in [
            "",
            "ftp://h",
            "localhost:11434",
            "http://",
            "https:///x",
            "http://:80",
            "http://u:p@h",
            "http://h?x=1",
            "http://h#f",
            "http://h x",
            long.as_str(),
        ] {
            assert!(endpoint_problem(bad, false).is_some(), "{bad:?}");
        }
        let mut c = cfg();
        c.providers[0].endpoint = Some("http://user:sk-SECRET@h".into());
        let e = c.validate().unwrap_err();
        assert!(matches!(e, AiConfigProblem::InvalidEndpoint { .. }));
        assert!(!e.to_string().contains("sk-SECRET"));
    }

    #[test]
    fn empty_model_name() {
        let mut c = cfg();
        c.providers[0].models.push("  ".into());
        assert_eq!(
            c.validate(),
            Err(AiConfigProblem::EmptyModelName {
                provider: id("local")
            })
        );
    }

    #[test]
    fn model_assignments_must_resolve() {
        let mut c = cfg();
        c.models.nl_to_sql = Some(ModelChoice::new(id("nope"), "m1"));
        assert_eq!(
            c.validate(),
            Err(AiConfigProblem::UnknownProvider {
                use_case: Some(UseCase::NlToSql),
                provider: "nope".into()
            })
        );
        c.models.nl_to_sql = None;
        c.models.default = Some(ModelChoice::new(id("local"), "zzz"));
        let e = c.validate().unwrap_err();
        assert_eq!(
            e,
            AiConfigProblem::UnknownModel {
                use_case: None,
                provider: id("local"),
                model: "zzz".into()
            }
        );
        assert!(e.to_string().starts_with("models.default:"), "{e}");
    }

    #[test]
    fn limits_are_checked() {
        let mut c = cfg();
        c.limits.max_rows = 0;
        assert!(matches!(
            c.validate(),
            Err(AiConfigProblem::LimitOutOfRange {
                key: "max_rows",
                ..
            })
        ));
        c.limits.max_rows = 5;
        c.limits.timeout_secs = 4_000;
        assert!(matches!(
            c.validate(),
            Err(AiConfigProblem::LimitOutOfRange {
                key: "timeout_secs",
                ..
            })
        ));
    }

    #[test]
    fn plain_http_with_api_key_needs_a_loopback_host() {
        let check = |endpoint: &str, key: bool| {
            let mut c = cfg();
            c.providers[0].endpoint = Some(endpoint.into());
            c.providers[0].api_key_in_keychain = key;
            c.validate()
        };
        for ok in [
            ("http://127.0.0.1:11434", true),
            ("http://localhost:11434", true),
            ("http://[::1]:11434", true),
            ("http://remote.example.com:11434", false),
            ("https://remote.example.com/v1", true),
        ] {
            assert_eq!(check(ok.0, ok.1), Ok(()), "{ok:?}");
        }
        for bad in [
            "http://remote.example.com",
            "http://10.0.0.5:11434",
            "http://localhost.evil.com",
        ] {
            let e = check(bad, true).unwrap_err();
            assert!(
                matches!(e, AiConfigProblem::InvalidEndpoint { .. }),
                "{bad}"
            );
            assert!(!e.to_string().contains(bad.trim_start_matches("http://")));
        }
    }
}
