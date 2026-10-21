// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI indicator: which provider receives the schema (and result rows) of a connection, and
//! whether that is this computer or a remote service. The status bar and the AI view show it.

use sqler_config::{AiConfig, ConnectionSpec, ModelChoice, ProviderConfig, ProviderKind, UseCase};

use super::kind_label;

/// What AI does for one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiStatus {
    /// AI is off for the connection (the default).
    Off,
    /// AI is on but no provider and model is chosen yet.
    NotSetUp,
    /// AI is on and goes to this provider.
    On {
        /// The provider id.
        provider: String,
        /// The provider's kind.
        kind: ProviderKind,
        /// The model.
        model: String,
        /// Whether the provider runs on this computer.
        local: bool,
        /// A local provider serves the main model, but another feature goes to a remote one.
        also_remote: bool,
        /// Whether result rows (not only the schema) may be sent.
        rows: bool,
    },
}

impl AiStatus {
    /// The short text of the status bar item.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Off => "AI off".to_owned(),
            Self::NotSetUp => "AI not set up".to_owned(),
            Self::On {
                provider,
                local,
                also_remote,
                ..
            } => {
                let place = match (local, also_remote) {
                    (true, true) => "local +remote",
                    (true, false) => "local",
                    (false, _) => "remote",
                };
                format!("{provider} \u{b7} {place}")
            }
        }
    }

    /// The tooltip of the status bar item.
    #[must_use]
    pub fn tooltip(&self) -> String {
        match self {
            Self::Off => "AI is off for this connection. Click to show the AI view.".to_owned(),
            Self::NotSetUp => {
                "AI is on for this connection, but no provider is set up. Click to show the AI view."
                    .to_owned()
            }
            Self::On {
                provider,
                kind,
                model,
                local,
                also_remote,
                rows,
            } => {
                let what = if *rows {
                    "Schema and result rows"
                } else {
                    "Schema"
                };
                let to = if *local {
                    "this computer"
                } else {
                    "a remote service"
                };
                let more = if *also_remote {
                    " Some features use a remote service."
                } else {
                    ""
                };
                format!(
                    "AI provider: {provider} ({}), model {model}. {what} go to {to}.{more} Click to show the AI view.",
                    kind_label(*kind)
                )
            }
        }
    }
}

/// The model the connection uses: the default, else the first use case with a model of its own.
#[must_use]
pub fn main_choice(config: &AiConfig) -> Option<&ModelChoice> {
    config.models.default.as_ref().or_else(|| {
        UseCase::ALL
            .into_iter()
            .find_map(|u| config.models.for_use_case(u))
    })
}

/// Whether the provider runs on this computer: its endpoint is `localhost` or a loopback
/// address, or it is an Ollama provider with no endpoint (the default is `127.0.0.1`).
#[must_use]
pub fn is_local(provider: &ProviderConfig) -> bool {
    match provider.endpoint.as_deref() {
        None => provider.kind == ProviderKind::Ollama,
        Some(endpoint) => host_is_local(endpoint),
    }
}

fn host_is_local(endpoint: &str) -> bool {
    let rest = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint);
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The status for a connection; `None` when there is no connection.
#[must_use]
pub fn ai_status(config: &AiConfig, spec: Option<&ConnectionSpec>) -> Option<AiStatus> {
    let spec = spec?;
    if !spec.ai.enabled {
        return Some(AiStatus::Off);
    }
    let Some(choice) = main_choice(config) else {
        return Some(AiStatus::NotSetUp);
    };
    let Some(provider) = config.provider(&choice.provider) else {
        return Some(AiStatus::NotSetUp);
    };
    let local = is_local(provider);
    let also_remote = local
        && super::models::SLOTS
            .into_iter()
            .filter_map(|u| super::models::slot(&config.models, u))
            .filter_map(|c| config.provider(&c.provider))
            .any(|p| !is_local(p));
    Some(AiStatus::On {
        provider: provider.id.to_string(),
        kind: provider.kind,
        model: choice.model.clone(),
        local,
        also_remote,
        rows: spec.ai.sends_rows(spec.environment),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::{ConnectionAi, ProviderId};
    use sqler_driver_api::sqler_core::{DriverId, Environment};

    fn id(s: &str) -> ProviderId {
        ProviderId::new(s).unwrap()
    }

    fn spec(ai: ConnectionAi, env: Option<Environment>) -> ConnectionSpec {
        let mut s = ConnectionSpec::new("c", DriverId::new("sqlite").unwrap(), "x").with_ai(ai);
        if let Some(env) = env {
            s = s.with_environment(env);
        }
        s
    }

    fn config(providers: Vec<ProviderConfig>, default: Option<(&str, &str)>) -> AiConfig {
        let mut c = AiConfig::default();
        c.providers = providers;
        c.models.default = default.map(|(p, m)| ModelChoice::new(id(p), m));
        c
    }

    fn ollama() -> ProviderConfig {
        ProviderConfig::new(id("ollama"), ProviderKind::Ollama).with_models(["q"])
    }

    fn claude() -> ProviderConfig {
        ProviderConfig::new(id("claude"), ProviderKind::Anthropic)
            .with_models(["c1"])
            .with_api_key_in_keychain(true)
    }

    #[test]
    fn local_and_remote() {
        assert!(is_local(&ollama()));
        assert!(is_local(&ollama().with_endpoint("http://localhost:11434")));
        assert!(is_local(&ollama().with_endpoint("http://[::1]:11434/x")));
        assert!(is_local(&ollama().with_endpoint("http://127.0.0.1")));
        assert!(!is_local(&ollama().with_endpoint("http://10.0.0.5:11434")));
        assert!(!is_local(
            &ollama().with_endpoint("https://ollama.example.com")
        ));
        assert!(!is_local(&claude()));
        let compatible = ProviderConfig::new(id("lm"), ProviderKind::OpenAi)
            .with_endpoint("http://127.0.0.1:1234");
        assert!(is_local(&compatible), "a loopback OpenAI-compatible server");
        let remote = ProviderConfig::new(id("oa"), ProviderKind::OpenAi);
        assert!(!is_local(&remote));
    }

    #[test]
    fn no_connection_no_status() {
        assert_eq!(ai_status(&AiConfig::default(), None), None);
    }

    #[test]
    fn off_by_default() {
        let c = config(vec![ollama()], Some(("ollama", "q")));
        let s = spec(ConnectionAi::default(), None);
        let st = ai_status(&c, Some(&s)).unwrap();
        assert_eq!(st, AiStatus::Off);
        assert_eq!(st.label(), "AI off");
    }

    #[test]
    fn on_without_a_provider_is_not_set_up() {
        let s = spec(ConnectionAi::new(true), None);
        let st = ai_status(&AiConfig::default(), Some(&s)).unwrap();
        assert_eq!(st, AiStatus::NotSetUp);
        assert_eq!(st.label(), "AI not set up");
        let no_model = config(vec![ollama()], None);
        assert_eq!(ai_status(&no_model, Some(&s)), Some(AiStatus::NotSetUp));
    }

    #[test]
    fn a_local_provider() {
        let c = config(vec![ollama()], Some(("ollama", "q")));
        let s = spec(ConnectionAi::new(true), Some(Environment::Dev));
        let st = ai_status(&c, Some(&s)).unwrap();
        assert_eq!(st.label(), "ollama \u{b7} local");
        assert_eq!(
            st.tooltip(),
            "AI provider: ollama (Ollama), model q. Schema and result rows go to this computer. Click to show the AI view."
        );
    }

    #[test]
    fn a_remote_provider_and_rows_off_on_prod() {
        let c = config(vec![claude()], Some(("claude", "c1")));
        let s = spec(ConnectionAi::new(true), Some(Environment::Prod));
        let st = ai_status(&c, Some(&s)).unwrap();
        assert_eq!(st.label(), "claude \u{b7} remote");
        assert_eq!(
            st.tooltip(),
            "AI provider: claude (Anthropic), model c1. Schema go to a remote service. Click to show the AI view."
        );
        let s = spec(
            ConnectionAi::new(true).with_send_rows(true),
            Some(Environment::Prod),
        );
        assert!(matches!(
            ai_status(&c, Some(&s)),
            Some(AiStatus::On { rows: true, .. })
        ));
    }

    #[test]
    fn a_mixed_setup_says_so() {
        let mut c = config(vec![ollama(), claude()], Some(("ollama", "q")));
        c.models.explain = Some(ModelChoice::new(id("claude"), "c1"));
        let s = spec(ConnectionAi::new(true), None);
        let st = ai_status(&c, Some(&s)).unwrap();
        assert_eq!(st.label(), "ollama \u{b7} local +remote");
        assert!(st.tooltip().contains("Some features use a remote service."));
        // A remote main model is just remote.
        let mut c = config(vec![ollama(), claude()], Some(("claude", "c1")));
        c.models.explain = Some(ModelChoice::new(id("ollama"), "q"));
        let st = ai_status(&c, Some(&s)).unwrap();
        assert_eq!(st.label(), "claude \u{b7} remote");
    }

    #[test]
    fn without_a_default_the_first_use_case_with_a_model_serves() {
        let mut c = config(vec![ollama(), claude()], None);
        c.models.fix = Some(ModelChoice::new(id("claude"), "c1"));
        c.models.explain = Some(ModelChoice::new(id("ollama"), "q"));
        let s = spec(ConnectionAi::new(true), None);
        // Use case order: NL to SQL, Explain, Fix, Exploration.
        let st = ai_status(&c, Some(&s)).unwrap();
        assert_eq!(st.label(), "ollama \u{b7} local +remote");
    }
}
