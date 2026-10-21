// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::future::Future;
use std::pin::Pin;

use crate::error::AiError;
use crate::message::ChatRequest;
use crate::stream::ChatStream;

/// A boxed, sendable future. Keeps the traits dyn-compatible without `async-trait`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a provider supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Capabilities {
    /// Tool calling.
    pub tools: bool,
    /// JSON mode.
    pub json_mode: bool,
    /// Image input.
    pub vision: bool,
    /// The largest context window the provider offers, if known.
    pub max_context_tokens: Option<u32>,
}

impl Capabilities {
    /// Creates a capability set.
    #[must_use]
    pub fn new(
        tools: bool,
        json_mode: bool,
        vision: bool,
        max_context_tokens: Option<u32>,
    ) -> Self {
        Self {
            tools,
            json_mode,
            vision,
            max_context_tokens,
        }
    }
}

/// A model a provider offers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelInfo {
    /// The model id to put in `ChatRequest::model`.
    pub id: String,
    /// A name for display.
    pub display_name: Option<String>,
    /// The context window in tokens, if known.
    pub context_window: Option<u32>,
}

impl ModelInfo {
    /// Creates model info with only an id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            context_window: None,
        }
    }

    /// Sets the display name.
    #[must_use]
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// Sets the context window.
    #[must_use]
    pub fn with_context_window(mut self, tokens: u32) -> Self {
        self.context_window = Some(tokens);
        self
    }
}

/// An AI provider such as `Ollama` or `Anthropic`.
pub trait AiProvider: Send + Sync {
    /// A stable id such as `ollama`, `anthropic` or `mock`.
    fn id(&self) -> &'static str;

    /// Lists the models the provider offers.
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>>;

    /// Starts a chat. This is not async: a setup failure (auth, network) is the
    /// first `Err` item of the stream.
    ///
    /// Dropping the returned stream cancels the request, so an implementation
    /// must tie the HTTP request's lifetime to the stream.
    fn stream(&self, request: ChatRequest) -> Box<dyn ChatStream + '_>;

    /// What the provider supports.
    fn capabilities(&self) -> Capabilities;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders() {
        let m = ModelInfo::new("m")
            .with_display_name("M")
            .with_context_window(8);
        assert_eq!(m.display_name.as_deref(), Some("M"));
        assert_eq!(m.context_window, Some(8));
        assert_eq!(
            Capabilities::default(),
            Capabilities::new(false, false, false, None)
        );
        assert!(Capabilities::new(true, false, false, Some(1)).tools);
    }

    #[test]
    fn provider_is_dyn_compatible() {
        fn _takes(_: &dyn AiProvider) {}
    }
}
