// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Ollama` implementation of the `AiProvider` trait.
//!
//! [`OllamaProvider`] talks to an Ollama server over its native API
//! (`GET /api/tags`, `POST /api/show`, `POST /api/chat` with `stream: true`).
//! The endpoint defaults to [`DEFAULT_ENDPOINT`] (`http://127.0.0.1:11434`).
//!
//! # Tokio
//!
//! The HTTP client needs a runtime: poll the futures and streams inside a
//! Tokio runtime with I/O and time enabled.
//!
//! # Locality and capabilities
//!
//! [`OllamaProvider::is_local`] is true for `localhost` and loopback hosts.
//! [`OllamaProvider::model_capabilities`] reads one model's `capabilities`
//! list and context length from the server. Without the list the defaults are
//! tools on, JSON mode on, vision off.
//!
//! # Cancellation
//!
//! Dropping the stream drops the HTTP response and closes the connection.

#![forbid(unsafe_code)]

mod convert;
mod error;
mod ndjson;
mod provider;
mod stream;
mod wire;

pub use provider::{DEFAULT_ENDPOINT, OllamaProvider};
