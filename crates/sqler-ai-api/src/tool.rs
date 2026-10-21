// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use crate::error::AiError;

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ToolDefinition {
    /// The tool name.
    pub name: String,
    /// What the tool does, for the model.
    pub description: String,
    /// A JSON Schema object that describes the arguments.
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    /// Creates a tool definition.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A tool call the model made.
///
/// `arguments` is the raw JSON text. Malformed JSON from a model reaches the
/// tool layer and is not a stream error; use [`ToolCall::parse_arguments`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolCall {
    /// The provider's id for this call.
    pub id: String,
    /// The tool name.
    pub name: String,
    /// The arguments as raw JSON text.
    pub arguments: String,
}

impl ToolCall {
    /// Creates a tool call.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    /// Parses the arguments as JSON.
    ///
    /// # Errors
    ///
    /// Returns [`AiError::Invalid`] when the text is not valid JSON.
    pub fn parse_arguments(&self) -> Result<serde_json::Value, AiError> {
        serde_json::from_str(&self.arguments)
            .map_err(|e| AiError::Invalid(format!("tool arguments are not valid JSON: {e}")))
    }
}

/// The result of running a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolResult {
    /// The id of the call this answers.
    pub call_id: String,
    /// The result text.
    pub content: String,
    /// Whether the tool failed.
    pub is_error: bool,
}

impl ToolResult {
    /// A successful result.
    #[must_use]
    pub fn ok(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// A failed result.
    #[must_use]
    pub fn error(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_arguments() {
        let ok = ToolCall::new("1", "t", r#"{"a":1}"#);
        assert_eq!(ok.parse_arguments().unwrap()["a"], 1);
        let bad = ToolCall::new("1", "t", "{oops");
        assert!(matches!(bad.parse_arguments(), Err(AiError::Invalid(_))));
    }

    #[test]
    fn results_and_definition() {
        assert!(!ToolResult::ok("1", "x").is_error);
        assert!(ToolResult::error("1", "x").is_error);
        let d = ToolDefinition::new("n", "d", serde_json::json!({"type": "object"}));
        assert_eq!(d.parameters["type"], "object");
    }
}
