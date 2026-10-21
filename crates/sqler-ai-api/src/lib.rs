// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `AiProvider` trait, message and tool types, streaming and token accounting.
//!
//! # Stream protocol
//!
//! [`AiProvider::stream`] returns a [`ChatStream`]. It yields any number of
//! `TextDelta`, `ToolCall` and `Usage` events in any order, then exactly one
//! `Done`, then `None` forever. An `Err` ends the stream and is followed by
//! `None`. A setup failure is the first `Err` item.
//!
//! # Cancellation
//!
//! Dropping the stream is the only way to cancel. Providers must tie the HTTP
//! request's lifetime to the stream, so no event and no error comes after a drop.
//!
//! # Tokens
//!
//! Each `Usage` event is an increment, not a running total.

#![forbid(unsafe_code)]

mod error;
mod message;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;
mod provider;
mod stream;
mod tool;
mod usage;

pub use error::AiError;
pub use message::{ChatRequest, Image, Message, Part, ResponseFormat, Role};
pub use provider::{AiProvider, BoxFuture, Capabilities, ModelInfo};
pub use stream::{ChatEvent, ChatResponse, ChatStream, StopReason, collect};
pub use tool::{ToolCall, ToolDefinition, ToolResult};
pub use usage::Usage;
