// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI provider registry: builds the providers compiled in, one cargo feature each
//! (`ai-ollama`, `ai-anthropic`, `ai-openai`, `ai-mistral`), for `sqler-ui`, which sees providers
//! only through `sqler-ai-api` (ADR-0050).
//!
//! A kind without a provider crate behind it (or built without its feature) is left out of
//! [`Providers::kinds`] and answers [`AiError::Unsupported`]. API keys are read from the OS
//! keychain on the blocking pool only, and never appear in an error.

use std::sync::Arc;

use sqler_ai_api::{AiError, AiProvider};
use sqler_config::{ProviderConfig, ProviderKind, Secret, SecretStore};
use sqler_ui::ai::{AiProviderFactory, kind_label, needs_key};

/// The providers of this build.
pub struct Providers {
    secrets: Arc<dyn SecretStore>,
}

impl Providers {
    /// A registry that reads stored API keys from `secrets`.
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self { secrets }
    }

    /// The key to give a provider: the typed one, else the stored one when the provider keeps a
    /// key, else none. A kind that cannot work without a key fails with [`AiError::Auth`].
    fn resolve_key(
        &self,
        provider: &ProviderConfig,
        typed: Option<&Secret>,
    ) -> Result<Option<Secret>, AiError> {
        if let Some(key) = typed {
            return Ok(Some(key.clone()));
        }
        let missing = || {
            AiError::Auth(format!(
                "an API key is needed for {}",
                kind_label(provider.kind)
            ))
        };
        let Some(key_ref) = provider.key_ref() else {
            return if needs_key(provider.kind) {
                Err(missing())
            } else {
                Ok(None)
            };
        };
        match self.secrets.get(key_ref.secret_ref()) {
            Ok(Some(key)) => Ok(Some(key)),
            Ok(None) => Err(AiError::Auth(format!(
                "no API key is stored for {}",
                provider.id
            ))),
            Err(err) => Err(AiError::Auth(format!(
                "the keychain could not be read: {err}"
            ))),
        }
    }
}

impl AiProviderFactory for Providers {
    // Empty when no provider feature is on; pushes follow the features otherwise.
    #[allow(unused_mut, clippy::vec_init_then_push)]
    fn kinds(&self) -> Vec<ProviderKind> {
        // One block per provider crate that exports a provider type, in enum order.
        let mut kinds = Vec::new();
        #[cfg(feature = "ai-ollama")]
        kinds.push(ProviderKind::Ollama);
        #[cfg(feature = "ai-anthropic")]
        kinds.push(ProviderKind::Anthropic);
        #[cfg(feature = "ai-openai")]
        kinds.push(ProviderKind::OpenAi);
        #[cfg(feature = "ai-mistral")]
        kinds.push(ProviderKind::Mistral);
        kinds
    }

    #[allow(unused_variables)] // `key` is used by the arms of the providers compiled in
    fn build(
        &self,
        provider: &ProviderConfig,
        key: Option<&Secret>,
    ) -> Result<Arc<dyn AiProvider>, AiError> {
        if !self.kinds().contains(&provider.kind) {
            return Err(AiError::Unsupported(format!(
                "{} is not available in this build",
                kind_label(provider.kind)
            )));
        }
        #[allow(unused_variables)] // no provider feature on: nothing reads the key
        let key = self.resolve_key(provider, key)?;
        // One `#[cfg(feature = ...)]` arm per provider whose crate exports a provider type.
        match provider.kind {
            #[cfg(feature = "ai-ollama")]
            ProviderKind::Ollama => {
                let built = sqler_ai_ollama::OllamaProvider::new(provider.endpoint.as_deref())?;
                Ok(Arc::new(built))
            }
            #[cfg(feature = "ai-anthropic")]
            ProviderKind::Anthropic => {
                let key = key.ok_or_else(|| AiError::Auth("an API key is needed".to_owned()))?;
                let built = sqler_ai_anthropic::AnthropicProvider::new(
                    key.expose(),
                    provider.endpoint.as_deref(),
                )?;
                Ok(Arc::new(built))
            }
            #[cfg(feature = "ai-openai")]
            ProviderKind::OpenAi => {
                let key = key.ok_or_else(|| AiError::Auth("an API key is needed".to_owned()))?;
                let built = sqler_ai_openai::OpenAiProvider::new(
                    provider.endpoint.as_deref(),
                    Some(key.expose()),
                )?;
                Ok(Arc::new(built))
            }
            #[cfg(feature = "ai-mistral")]
            ProviderKind::Mistral => {
                let key = key.ok_or_else(|| AiError::Auth("an API key is needed".to_owned()))?;
                let built = sqler_ai_mistral::MistralProvider::new(
                    key.expose(),
                    provider.endpoint.as_deref(),
                )?;
                Ok(Arc::new(built))
            }
            #[allow(unreachable_patterns)]
            _ => Err(AiError::Unsupported(format!(
                "{} is not available in this build",
                kind_label(provider.kind)
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::{MemoryStore, ProviderId};

    fn provider(kind: ProviderKind, keyed: bool) -> ProviderConfig {
        ProviderConfig::new(ProviderId::new("p").unwrap(), kind).with_api_key_in_keychain(keyed)
    }

    fn registry(store: MemoryStore) -> Providers {
        Providers::new(Arc::new(store))
    }

    #[test]
    fn kinds_follow_the_providers_compiled_in() {
        let kinds = registry(MemoryStore::new()).kinds();
        assert_eq!(
            kinds.contains(&ProviderKind::Ollama),
            cfg!(feature = "ai-ollama")
        );
        assert_eq!(
            kinds.contains(&ProviderKind::Anthropic),
            cfg!(feature = "ai-anthropic")
        );
        assert_eq!(
            kinds.contains(&ProviderKind::OpenAi),
            cfg!(feature = "ai-openai")
        );
        assert_eq!(
            kinds.contains(&ProviderKind::Mistral),
            cfg!(feature = "ai-mistral")
        );
    }

    #[cfg(feature = "ai-ollama")]
    #[test]
    fn ollama_builds_without_network_and_a_bad_endpoint_is_invalid() {
        let r = registry(MemoryStore::new());
        assert!(
            r.build(&provider(ProviderKind::Ollama, false), None)
                .is_ok()
        );
        let mut p = provider(ProviderKind::Ollama, false);
        p.endpoint = Some("ftp://host".to_owned());
        let err = r.build(&p, None).err().unwrap();
        assert!(matches!(err, AiError::Invalid(_)), "{err}");
    }

    #[test]
    fn a_kind_without_a_provider_is_unsupported() {
        let r = registry(MemoryStore::new());
        let key = Secret::new("sk-test");
        for (kind, on) in [
            (ProviderKind::Anthropic, cfg!(feature = "ai-anthropic")),
            (ProviderKind::OpenAi, cfg!(feature = "ai-openai")),
            (ProviderKind::Mistral, cfg!(feature = "ai-mistral")),
        ] {
            if on {
                continue;
            }
            let err = r.build(&provider(kind, false), Some(&key)).err().unwrap();
            assert!(matches!(err, AiError::Unsupported(_)), "{err}");
        }
    }

    #[cfg(any(
        feature = "ai-anthropic",
        feature = "ai-openai",
        feature = "ai-mistral"
    ))]
    #[allow(clippy::vec_init_then_push)]
    fn keyed_kinds() -> Vec<ProviderKind> {
        let mut kinds = Vec::new();
        #[cfg(feature = "ai-anthropic")]
        kinds.push(ProviderKind::Anthropic);
        #[cfg(feature = "ai-openai")]
        kinds.push(ProviderKind::OpenAi);
        #[cfg(feature = "ai-mistral")]
        kinds.push(ProviderKind::Mistral);
        kinds
    }

    #[cfg(any(
        feature = "ai-anthropic",
        feature = "ai-openai",
        feature = "ai-mistral"
    ))]
    #[test]
    fn keyed_providers_build_without_network_and_a_bad_endpoint_is_invalid() {
        let r = registry(MemoryStore::new());
        let key = Secret::new("sk-test");
        for kind in keyed_kinds() {
            assert!(r.build(&provider(kind, false), Some(&key)).is_ok());
            let mut p = provider(kind, false);
            p.endpoint = Some("ftp://host".to_owned());
            let err = r.build(&p, Some(&key)).err().unwrap();
            assert!(matches!(err, AiError::Invalid(_)), "{err}");
            assert!(!err.to_string().contains("sk-test"));
        }
    }

    #[cfg(any(
        feature = "ai-anthropic",
        feature = "ai-openai",
        feature = "ai-mistral"
    ))]
    #[test]
    fn keyed_providers_without_a_key_are_an_auth_error() {
        let r = registry(MemoryStore::new());
        for kind in keyed_kinds() {
            for keyed in [false, true] {
                let err = r.build(&provider(kind, keyed), None).err().unwrap();
                assert!(matches!(err, AiError::Auth(_)), "{err}");
            }
        }
    }

    #[test]
    fn a_typed_key_wins_over_the_stored_one() {
        let store = MemoryStore::new();
        let p = provider(ProviderKind::OpenAi, true);
        store
            .set(p.key_ref().unwrap().secret_ref(), &Secret::new("stored"))
            .unwrap();
        let r = registry(store);
        let typed = Secret::new("typed");
        let got = r.resolve_key(&p, Some(&typed)).unwrap().unwrap();
        assert_eq!(got.expose(), "typed");
        let got = r.resolve_key(&p, None).unwrap().unwrap();
        assert_eq!(got.expose(), "stored");
    }

    #[test]
    fn a_missing_key_is_an_auth_error_without_key_text() {
        let r = registry(MemoryStore::new());
        for keyed in [false, true] {
            let err = r
                .resolve_key(&provider(ProviderKind::Anthropic, keyed), None)
                .unwrap_err();
            assert!(matches!(err, AiError::Auth(_)), "{err}");
            assert!(!err.to_string().contains("sk-"));
        }
    }

    #[test]
    fn ollama_needs_no_key() {
        let r = registry(MemoryStore::new());
        assert!(
            r.resolve_key(&provider(ProviderKind::Ollama, false), None)
                .unwrap()
                .is_none()
        );
    }
}
