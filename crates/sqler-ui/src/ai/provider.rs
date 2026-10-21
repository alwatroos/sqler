// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! How the UI reaches AI providers without depending on them: a factory trait that the binary
//! implements for the providers compiled in, and the startup data around it.

use std::sync::Arc;

use sqler_ai_api::{AiError, AiProvider};
use sqler_config::{AiConfig, ConfigDir, ProviderConfig, ProviderKind, Secret};

/// Builds providers from their configuration. Implemented by the binary per cargo feature.
pub trait AiProviderFactory: Send + Sync {
    /// The provider kinds compiled into this build, in enum order.
    fn kinds(&self) -> Vec<ProviderKind>;

    /// Builds the provider. Blocking: it reads the keychain when `key` is `None` and
    /// `provider.api_key_in_keychain` is set, so call it on the blocking pool only. A failure
    /// never carries the key.
    ///
    /// # Errors
    /// [`AiError::Unsupported`] for a kind that is not compiled in, [`AiError::Auth`] for a
    /// missing key, [`AiError::Invalid`] for a bad endpoint.
    fn build(
        &self,
        provider: &ProviderConfig,
        key: Option<&Secret>,
    ) -> Result<Arc<dyn AiProvider>, AiError>;
}

/// A factory with no provider (a build without AI features, and most tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProviders;

impl AiProviderFactory for NoProviders {
    fn kinds(&self) -> Vec<ProviderKind> {
        Vec::new()
    }

    fn build(
        &self,
        provider: &ProviderConfig,
        _key: Option<&Secret>,
    ) -> Result<Arc<dyn AiProvider>, AiError> {
        Err(AiError::Unsupported(format!(
            "{} is not available in this build",
            super::kind_label(provider.kind)
        )))
    }
}

/// What the binary hands over about AI at startup: the loaded `ai.toml`, where to save it and the
/// provider factory.
#[derive(Clone)]
pub struct AiStartup {
    /// The configuration, or why `ai.toml` could not be read. A missing file is the default.
    pub config: Result<AiConfig, String>,
    /// Where `ai.toml` is saved; `None` without a config directory or when the file could not be
    /// read (it is never overwritten then).
    pub dir: Option<ConfigDir>,
    /// Builds the providers compiled in.
    pub factory: Arc<dyn AiProviderFactory>,
}

impl std::fmt::Debug for AiStartup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiStartup")
            .field("config", &self.config.as_ref().map(|_| ()))
            .field("dir", &self.dir.is_some())
            .finish_non_exhaustive()
    }
}

impl AiStartup {
    /// Loads `ai.toml` from `dir`. The directory is kept only when the load worked, so a broken
    /// file is never overwritten.
    #[must_use]
    pub fn load(dir: Option<ConfigDir>, factory: Arc<dyn AiProviderFactory>) -> Self {
        let Some(d) = dir else {
            return Self {
                config: Ok(AiConfig::default()),
                dir: None,
                factory,
            };
        };
        match d.load_ai_config() {
            Ok(config) => Self {
                config: Ok(config),
                dir: Some(d),
                factory,
            },
            Err(err) => Self {
                config: Err(err.to_string()),
                dir: None,
                factory,
            },
        }
    }

    /// No configuration directory and no provider.
    #[must_use]
    pub fn none() -> Self {
        Self::load(None, Arc::new(NoProviders))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("sqler-ui-ai-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn no_directory_is_the_default_without_a_save_target() {
        let s = AiStartup::none();
        assert_eq!(s.config.unwrap(), AiConfig::default());
        assert!(s.dir.is_none());
    }

    #[test]
    fn a_missing_file_is_the_default_and_keeps_the_directory() {
        let p = temp("missing");
        let s = AiStartup::load(Some(ConfigDir::new(&p)), Arc::new(NoProviders));
        assert_eq!(s.config.unwrap(), AiConfig::default());
        assert!(s.dir.is_some());
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn a_broken_file_is_an_error_and_drops_the_directory() {
        let p = temp("broken");
        std::fs::write(p.join("ai.toml"), "version = 1\nbogus = [").unwrap();
        let s = AiStartup::load(Some(ConfigDir::new(&p)), Arc::new(NoProviders));
        assert!(s.config.is_err());
        assert!(s.dir.is_none());
        assert_eq!(
            std::fs::read_to_string(p.join("ai.toml")).unwrap(),
            "version = 1\nbogus = ["
        );
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn debug_shows_only_whether_it_loaded() {
        let text = format!("{:?}", AiStartup::none());
        assert!(text.contains("AiStartup"));
    }

    #[test]
    fn no_providers_builds_nothing() {
        let f = NoProviders;
        assert_eq!(f.kinds().len(), 0);
        let p = ProviderConfig::new(
            sqler_config::ProviderId::new("a").unwrap(),
            ProviderKind::Ollama,
        );
        assert!(matches!(f.build(&p, None), Err(AiError::Unsupported(_))));
    }
}
