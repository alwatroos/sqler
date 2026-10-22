// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Private serde DTOs for the Mistral Chat Completions JSON.
//!
//! Verified on 2026-10-21 against the `OpenAPI` document
//! (<https://docs.mistral.ai/openapi.yaml>): the request has no
//! `stream_options`; `max_tokens` caps the output; a user `image_url` part is
//! a string or `{url}`; `function.arguments` is an object or a string;
//! `delta.content` is a string or an array of chunks; the stream
//! `finish_reason` is `stop`, `length`, `error`, `tool_calls` or null
//! (`model_length` appears in the non-stream enum and is accepted too); a
//! 422 body is `{"detail":[{"msg",...}]}`. The tool-call id rule
//! (9 characters, letters and digits) is not in the document; it is the rule
//! the API enforces and is kept as a defensive normalisation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- requests ----

#[derive(Debug, Serialize)]
pub(crate) struct ChatBody {
    pub model: String,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponseFormat {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireMessage {
    pub role: &'static str,
    /// Always present; `""` (not `null`) for an assistant turn with only tool calls.
    pub content: WireContent,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum WireContent {
    Text(String),
    Parts(Vec<WirePart>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WirePart {
    Text {
        text: String,
    },
    /// The `image_url` is the data URL as a plain string.
    ImageUrl {
        image_url: String,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireCall,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireToolFunction,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

// ---- responses ----

/// One `data:` payload of the chat stream.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Chunk {
    /// `"error"` for an error payload.
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<WireUsage>,
    #[serde(default)]
    pub error: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Choice {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub delta: Option<Delta>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Delta {
    /// A string, or an array of chunks (`text`, `thinking`, ...).
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallChunk>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToolCallChunk {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<FunctionChunk>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct FunctionChunk {
    #[serde(default)]
    pub name: Option<String>,
    /// A string (fragments of JSON text) or a JSON object.
    #[serde(default)]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub(crate) struct WireUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Models {
    #[serde(default)]
    pub data: Vec<WireModel>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub max_context_length: Option<u64>,
    #[serde(default)]
    pub capabilities: Option<WireCapabilities>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireCapabilities {
    #[serde(default)]
    pub completion_chat: Option<bool>,
    #[serde(default)]
    pub function_calling: bool,
    #[serde(default)]
    pub vision: bool,
}
