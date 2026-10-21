// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Private serde DTOs for the Ollama JSON.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ---- requests ----

#[derive(Debug, Serialize)]
pub(crate) struct ChatBody {
    pub model: String,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<WireOptions>,
    pub stream: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireMessage {
    pub role: &'static str,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireToolCall {
    pub function: WireCall,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireCall {
    pub name: String,
    pub arguments: Value,
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

#[derive(Debug, Serialize)]
pub(crate) struct WireOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ShowBody<'a> {
    pub model: &'a str,
}

// ---- responses ----

/// One line of the `/api/chat` stream.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ChatLine {
    #[serde(default)]
    pub message: Option<LineMessage>,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub done_reason: Option<String>,
    #[serde(default)]
    pub prompt_eval_count: Option<u64>,
    #[serde(default)]
    pub eval_count: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct LineMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<InToolCall>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InToolCall {
    #[serde(default)]
    pub id: Option<String>,
    pub function: InCall,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Tags {
    #[serde(default)]
    pub models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TagModel {
    pub name: String,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Show {
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub model_info: Option<Map<String, Value>>,
}

/// The `{"error": "..."}` body of a failed request.
#[derive(Debug, Deserialize)]
pub(crate) struct ErrorBody {
    pub error: String,
}
