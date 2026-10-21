// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Anthropic` implementation of the `AiProvider` trait.
//!
//! [`AnthropicProvider`] talks to the Anthropic Messages API
//! (`POST /v1/messages` with `stream: true`, `GET /v1/models`). The endpoint
//! defaults to [`DEFAULT_ENDPOINT`]; every request carries the
//! `anthropic-version` header [`API_VERSION`].
//!
//! # Tokio
//!
//! The HTTP client needs a runtime: poll the futures and streams inside a
//! Tokio runtime with I/O and time enabled.
//!
//! # Remote provider and the API key
//!
//! Anthropic is a remote provider: [`AnthropicProvider::is_local`] is always
//! false. The caller passes the API key to [`AnthropicProvider::new`]; it is
//! held as a sensitive header value, never printed by `Debug`, and removed
//! from every error text.
//!
//! # Capabilities
//!
//! [`AnthropicProvider::model_capabilities`] is a static table: tools and
//! vision on for `claude-` models, JSON mode off. A JSON-mode request fails
//! with `AiError::Unsupported`. A request without `max_output_tokens` sends
//! [`DEFAULT_MAX_TOKENS`], because the API requires `max_tokens`.
//!
//! # Cancellation
//!
//! Dropping the stream drops the HTTP response and closes the connection.

#![forbid(unsafe_code)]

mod convert;
mod error;
mod provider;
mod sse;
mod stream;
mod wire;

pub use provider::{API_VERSION, AnthropicProvider, DEFAULT_ENDPOINT, DEFAULT_MAX_TOKENS};
