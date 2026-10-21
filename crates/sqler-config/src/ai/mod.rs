// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! AI configuration: providers, models per use case, redaction list and query limits, in the
//! versioned `ai.toml` file (ADR-0035).
//!
//! Provider API keys are never stored here; they live in the OS keychain under a
//! [`ProviderKeyRef`](crate::ProviderKeyRef) derived from the provider id.

mod error;
mod file;
mod limits;
mod model;
mod redaction;

pub use error::{AiConfigError, AiConfigProblem};
pub use file::AI_CONFIG_FORMAT_VERSION;
pub use limits::{
    AI_MAX_ROWS_RANGE, AI_TIMEOUT_SECS_RANGE, AiLimits, DEFAULT_AI_MAX_ROWS,
    DEFAULT_AI_TIMEOUT_SECS, QueryCaps,
};
pub use model::{
    AiConfig, ModelAssignments, ModelChoice, ProviderConfig, ProviderId, ProviderKind, UseCase,
};
pub use redaction::{DEFAULT_REDACTED_COLUMNS, RedactionList};

use std::path::PathBuf;

use crate::ConfigDir;
use crate::fs::{read_optional, write_atomic};

impl ConfigDir {
    /// Path of `ai.toml`.
    #[must_use]
    pub fn ai_config_path(&self) -> PathBuf {
        self.root().join("ai.toml")
    }

    /// Loads `ai.toml`; a missing file yields [`AiConfig::default`]. The file is never rewritten
    /// here.
    ///
    /// # Errors
    /// [`AiConfigError::File`] for a read failure, malformed TOML, wrong types, unknown keys
    /// (the message gives line and column, never the source text), a missing `version` or one
    /// newer than [`AI_CONFIG_FORMAT_VERSION`]; [`AiConfigError::Invalid`] for a value that
    /// breaks a rule ([`AiConfig::validate`]).
    pub fn load_ai_config(&self) -> Result<AiConfig, AiConfigError> {
        let path = self.ai_config_path();
        let Some(text) = read_optional(&path)? else {
            return Ok(AiConfig::default());
        };
        file::parse(&path, &text)
    }

    /// Validates the configuration and writes it to `ai.toml` as `version = 1`, creating the
    /// directory when missing. The write is atomic. No secret is ever written.
    ///
    /// # Errors
    /// [`AiConfigError::Invalid`] if [`AiConfig::validate`] fails; [`AiConfigError::File`] for
    /// a serialisation or write failure.
    pub fn save_ai_config(&self, config: &AiConfig) -> Result<(), AiConfigError> {
        let path = self.ai_config_path();
        config
            .validate()
            .map_err(|problem| AiConfigError::Invalid {
                path: path.clone(),
                problem,
            })?;
        let text = file::serialize(&path, config)?;
        write_atomic(&path, &text)?;
        Ok(())
    }
}

/// Describes a parse error by line, column and message only. The `toml` Display
/// output quotes the offending source line, which may hold a key.
fn parse_message(text: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("line {line}, column {col}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}
