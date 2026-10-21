// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `OpenAI` and any OpenAI-compatible endpoint implementation of the `AiProvider` trait.
//!
//! [`OpenAiProvider`] talks to the Chat Completions API (`GET {base}/models`,
//! `POST {base}/chat/completions` with `stream: true`) over server-sent
//! events. The base URL defaults to [`DEFAULT_BASE_URL`]; a different base URL
//! (LM Studio, vLLM, the llama.cpp server, a gateway) selects
//! [`Flavor::Compatible`].
//!
//! # Tokio
//!
//! The HTTP client needs a runtime: poll the futures and streams inside a
//! Tokio runtime with I/O and time enabled.
//!
//! # Locality
//!
//! [`OpenAiProvider::is_local`] is true for `localhost` and loopback hosts.
//!
//! # API key
//!
//! The key is an optional constructor argument. It is never part of `Debug`
//! output or of an error, and over `http://` it is refused unless the host is
//! local.
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

pub use provider::{DEFAULT_BASE_URL, Flavor, OpenAiProvider};
