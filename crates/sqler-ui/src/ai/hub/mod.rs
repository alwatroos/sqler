// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI side view model: the providers, the models per use case and the AI state of the
//! connection the user works on. The view only renders it.

use sqler_config::{ConnectionProfile, ProviderId, ProviderKind};
use sqler_driver_api::sqler_core::Environment;

use super::action::AiAction;
use super::indicator::{AiStatus, ai_status, is_local};
use super::models::{SLOTS, slot, use_case_title};
use super::state::AiState;

#[cfg(feature = "gui")]
pub(crate) mod view;

/// One configured provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    /// The provider id.
    pub id: ProviderId,
    /// The API family.
    pub kind: ProviderKind,
    /// Whether it runs on this computer.
    pub local: bool,
    /// Whether an API key is kept in the keychain.
    pub key_stored: bool,
    /// The configured endpoint; empty for the provider's default.
    pub endpoint: String,
}

/// The connection the view is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubConnection {
    /// The connection name.
    pub name: String,
    /// Its environment tag.
    pub env: Option<Environment>,
    /// Whether AI is on for it.
    pub enabled: bool,
    /// What AI does for it.
    pub status: Option<AiStatus>,
}

/// Everything the AI side view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubModel {
    /// Why `ai.toml` could not be read, if it could not.
    pub load_error: Option<String>,
    /// The configured providers, in file order.
    pub providers: Vec<ProviderRow>,
    /// The active tab's connection, else the selected one.
    pub connection: Option<HubConnection>,
    /// The model of the default and of each use case as `provider/model`, `None` when unset.
    pub models: Vec<(&'static str, Option<String>)>,
}

/// What "Set up a provider" does: add the first provider kind this build has, else open the
/// settings page (which says no provider is available).
#[must_use]
pub fn set_up_action(ai: &AiState) -> AiAction {
    ai.kinds()
        .first()
        .map_or(AiAction::OpenSettings, |kind| AiAction::AddProvider(*kind))
}

/// Builds the model for the connection `profile` (if there is one).
#[must_use]
pub fn hub_model(ai: &AiState, profile: Option<&ConnectionProfile>) -> HubModel {
    let config = ai.config();
    let providers = config
        .providers
        .iter()
        .map(|p| ProviderRow {
            id: p.id.clone(),
            kind: p.kind,
            local: is_local(p),
            key_stored: p.api_key_in_keychain,
            endpoint: p.endpoint.clone().unwrap_or_default(),
        })
        .collect();
    let connection = profile.map(|p| {
        let spec = p.spec();
        HubConnection {
            name: spec.name.clone(),
            env: spec.environment,
            enabled: spec.ai.enabled,
            status: ai_status(config, Some(spec)),
        }
    });
    let choice =
        |c: Option<&sqler_config::ModelChoice>| c.map(|c| format!("{}/{}", c.provider, c.model));
    let models = SLOTS
        .into_iter()
        .map(|u| (use_case_title(u), choice(slot(&config.models, u))))
        .collect();
    HubModel {
        load_error: ai.load_error().map(str::to_owned),
        providers,
        connection,
        models,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::{
        AiConfig, ConnectionAi, ConnectionSpec, ConnectionStore, ModelChoice, ProviderConfig,
    };
    use sqler_driver_api::sqler_core::DriverId;

    fn profile(ai: ConnectionAi) -> ConnectionProfile {
        let mut store = ConnectionStore::new();
        let spec = ConnectionSpec::new("dev", DriverId::new("sqlite").unwrap(), "x")
            .with_environment(Environment::Dev)
            .with_ai(ai);
        let id = store.add_connection(spec, None).unwrap();
        store.connection(&id).unwrap().clone()
    }

    #[test]
    fn set_up_adds_the_first_kind_or_opens_settings() {
        assert!(matches!(
            set_up_action(&AiState::default()),
            AiAction::OpenSettings
        ));
        let state = AiState::new(AiConfig::default(), vec![ProviderKind::Anthropic], None);
        assert!(matches!(
            set_up_action(&state),
            AiAction::AddProvider(ProviderKind::Anthropic)
        ));
    }

    #[test]
    fn empty_setup() {
        let m = hub_model(&AiState::default(), None);
        assert_eq!(m.providers.len(), 0);
        assert!(m.connection.is_none());
        assert_eq!(m.load_error, None);
        assert_eq!(m.models.len(), 5);
        assert!(m.models.iter().all(|(_, v)| v.is_none()));
        assert_eq!(m.models[0].0, "Default");
    }

    #[test]
    fn populated_setup() {
        let id = |s| ProviderId::new(s).unwrap();
        let mut config = AiConfig::default();
        config
            .providers
            .push(ProviderConfig::new(id("ollama"), ProviderKind::Ollama).with_models(["q"]));
        config.providers.push(
            ProviderConfig::new(id("claude"), ProviderKind::Anthropic)
                .with_models(["c1"])
                .with_api_key_in_keychain(true),
        );
        config.models.default = Some(ModelChoice::new(id("ollama"), "q"));
        config.models.fix = Some(ModelChoice::new(id("claude"), "c1"));
        let ai = AiState::for_config(config);
        let p = profile(ConnectionAi::new(true));
        let m = hub_model(&ai, Some(&p));
        assert_eq!(m.providers.len(), 2);
        assert!(m.providers[0].local && !m.providers[0].key_stored);
        assert!(!m.providers[1].local && m.providers[1].key_stored);
        let c = m.connection.unwrap();
        assert_eq!(
            (c.name.as_str(), c.env, c.enabled),
            ("dev", Some(Environment::Dev), true)
        );
        assert_eq!(c.status.unwrap().label(), "ollama \u{b7} local +remote");
        assert_eq!(m.models[0].1.as_deref(), Some("ollama/q"));
        assert_eq!(m.models[3], ("Fix", Some("claude/c1".to_owned())));
        assert_eq!(m.models[1], ("NL to SQL", None));
    }

    #[test]
    fn a_connection_with_ai_off() {
        let p = profile(ConnectionAi::default());
        let c = hub_model(&AiState::default(), Some(&p)).connection.unwrap();
        assert!(!c.enabled);
        assert_eq!(c.status, Some(AiStatus::Off));
    }
}
