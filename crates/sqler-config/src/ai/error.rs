// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Typed errors of the AI configuration.

use std::path::PathBuf;

use thiserror::Error;

use super::model::{ProviderId, UseCase};
use crate::ConfigError;

fn scope(use_case: Option<UseCase>) -> &'static str {
    use_case.map_or("default", UseCase::as_str)
}

/// One thing wrong with an [`AiConfig`](super::AiConfig). Texts never echo endpoint or key values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AiConfigProblem {
    /// A provider id breaks the id grammar.
    #[error("invalid provider id {id:?}: {reason}")]
    InvalidProviderId {
        /// The offending id.
        id: String,
        /// Why it was rejected.
        reason: String,
    },
    /// Two providers share an id.
    #[error("duplicate provider id {0}")]
    DuplicateProvider(ProviderId),
    /// A provider kind is not one of `ollama`, `anthropic`, `openai`, `mistral`.
    #[error(
        "provider {provider}: unknown kind {kind:?} (expected ollama, anthropic, openai or mistral)"
    )]
    UnknownProviderKind {
        /// The provider.
        provider: ProviderId,
        /// The offending kind text.
        kind: String,
    },
    /// A provider endpoint is not an acceptable URL.
    #[error("provider {provider}: invalid endpoint: {reason}")]
    InvalidEndpoint {
        /// The provider.
        provider: ProviderId,
        /// Why it was rejected (never the endpoint itself).
        reason: &'static str,
    },
    /// A provider lists an empty model name.
    #[error("provider {provider}: model names must not be empty")]
    EmptyModelName {
        /// The provider.
        provider: ProviderId,
    },
    /// A model assignment names a provider that is not configured.
    #[error("models.{}: unknown provider {provider:?}", scope(*use_case))]
    UnknownProvider {
        /// The use case, `None` for `default`.
        use_case: Option<UseCase>,
        /// The provider id as written.
        provider: String,
    },
    /// A model assignment names a model its provider does not list.
    #[error("models.{}: provider {provider} does not list model {model:?}", scope(*use_case))]
    UnknownModel {
        /// The use case, `None` for `default`.
        use_case: Option<UseCase>,
        /// The provider.
        provider: ProviderId,
        /// The model as written.
        model: String,
    },
    /// An extra redaction entry is unusable.
    #[error("redaction.extra[{index}]: {reason}")]
    InvalidRedactionEntry {
        /// Position of the entry.
        index: usize,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A limit is outside its allowed range.
    #[error("limits.{key} is {value}, expected {min}..={max}")]
    LimitOutOfRange {
        /// The key.
        key: &'static str,
        /// The offending value.
        value: i64,
        /// Smallest allowed value.
        min: u32,
        /// Largest allowed value.
        max: u32,
    },
}

/// Failure to load or save `ai.toml`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AiConfigError {
    /// The file could not be read, parsed or written.
    #[error(transparent)]
    File(#[from] ConfigError),
    /// The file parsed but its content (or the config to save) is not acceptable.
    #[error("{}: {problem}", path.display())]
    Invalid {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        problem: AiConfigProblem,
    },
}
