// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Mistral` implementation of the `AiProvider` trait.
//!
//! [`MistralProvider`] talks to the Mistral API (`GET /v1/models`,
//! `GET /v1/models/{id}`, `POST /v1/chat/completions` with `stream: true`)
//! over server-sent events. The endpoint defaults to [`DEFAULT_ENDPOINT`];
//! the provider appends the `/v1/...` paths.
//!
//! # Tokio
//!
//! The HTTP client needs a runtime: poll the futures and streams inside a
//! Tokio runtime with I/O and time enabled.
//!
//! # API key
//!
//! The key is a required constructor argument. It is kept only as a
//! sensitive header value, never appears in `Debug` output or in an error
//! (server error text has it replaced by `***`), and this crate does not log.
//!
//! # Cancellation
//!
//! Dropping the stream drops the HTTP response and closes the connection.
//!
//! # Locality
//!
//! Mistral is a remote provider: [`MistralProvider::is_local`] is always
//! false, even for a loopback endpoint (a test server or a proxy).

#![forbid(unsafe_code)]

mod convert;
mod error;
mod provider;
mod sse;
mod stream;
mod wire;

pub use provider::{DEFAULT_ENDPOINT, MistralProvider};
