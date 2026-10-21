// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use crate::error::AiError;
use crate::tool::{ToolCall, ToolDefinition, ToolResult};

/// Who wrote a message. The system prompt is `ChatRequest::system`, not a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
    /// The user.
    User,
    /// The model.
    Assistant,
    /// A tool result.
    Tool,
}

/// An image attached to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Image {
    /// The MIME type, such as `image/png`.
    pub media_type: String,
    /// The raw image bytes.
    pub data: Vec<u8>,
}

impl Image {
    /// Creates an image.
    #[must_use]
    pub fn new(media_type: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            media_type: media_type.into(),
            data,
        }
    }
}

/// One part of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Part {
    /// Plain text.
    Text(String),
    /// An image.
    Image(Image),
    /// A tool call the model made.
    ToolCall(ToolCall),
    /// A tool result.
    ToolResult(ToolResult),
}

/// A chat message. Only the constructors build one, so every message is valid.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Message {
    role: Role,
    parts: Vec<Part>,
}

impl Message {
    /// A user message with text.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            parts: vec![Part::Text(text.into())],
        }
    }

    /// A user message with text and image parts.
    ///
    /// # Errors
    ///
    /// Returns [`AiError::Invalid`] when a part is a tool call or tool result.
    pub fn user_parts(parts: Vec<Part>) -> Result<Self, AiError> {
        if parts
            .iter()
            .any(|p| matches!(p, Part::ToolCall(_) | Part::ToolResult(_)))
        {
            return Err(AiError::Invalid(
                "a user message accepts only text and image parts".to_owned(),
            ));
        }
        Ok(Self {
            role: Role::User,
            parts,
        })
    }

    /// An assistant message with text.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            parts: vec![Part::Text(text.into())],
        }
    }

    /// An assistant message with optional text and tool calls.
    #[must_use]
    pub fn assistant_with_tools(text: Option<String>, calls: Vec<ToolCall>) -> Self {
        let mut parts: Vec<Part> = text.into_iter().map(Part::Text).collect();
        parts.extend(calls.into_iter().map(Part::ToolCall));
        Self {
            role: Role::Assistant,
            parts,
        }
    }

    /// A tool message that carries one result.
    #[must_use]
    pub fn tool_result(result: ToolResult) -> Self {
        Self {
            role: Role::Tool,
            parts: vec![Part::ToolResult(result)],
        }
    }

    /// The author of the message.
    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    /// The parts of the message.
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The `Text` parts joined together.
    #[must_use]
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// The output format the caller wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ResponseFormat {
    /// Free text.
    #[default]
    Text,
    /// JSON mode.
    Json,
}

/// A chat request.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ChatRequest {
    /// The model id.
    pub model: String,
    /// The system prompt.
    pub system: Option<String>,
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// The tools the model may call.
    pub tools: Vec<ToolDefinition>,
    /// The cap on completion tokens.
    pub max_output_tokens: Option<u32>,
    /// The sampling temperature.
    pub temperature: Option<f32>,
    /// The output format.
    pub response_format: ResponseFormat,
}

impl ChatRequest {
    /// Creates a request with no system prompt, tools or limits.
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages,
            tools: Vec::new(),
            max_output_tokens: None,
            temperature: None,
            response_format: ResponseFormat::Text,
        }
    }

    /// Sets the system prompt.
    #[must_use]
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Sets the tools.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    /// Sets the cap on completion tokens.
    #[must_use]
    pub fn with_max_output_tokens(mut self, max: u32) -> Self {
        self.max_output_tokens = Some(max);
        self
    }

    /// Sets the sampling temperature.
    #[must_use]
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets the output format.
    #[must_use]
    pub fn with_response_format(mut self, format: ResponseFormat) -> Self {
        self.response_format = format;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors() {
        let m = Message::user("hi");
        assert_eq!(m.role(), Role::User);
        assert_eq!(m.text(), "hi");
        let a = Message::assistant_with_tools(None, vec![ToolCall::new("1", "t", "{}")]);
        assert_eq!(a.role(), Role::Assistant);
        assert_eq!(a.parts().len(), 1);
        assert_eq!(a.text(), "");
        let a = Message::assistant_with_tools(Some("x".into()), vec![]);
        assert_eq!(a, Message::assistant("x"));
        let t = Message::tool_result(ToolResult::ok("1", "r"));
        assert_eq!(t.role(), Role::Tool);
    }

    #[test]
    fn user_parts_validates() {
        let ok = Message::user_parts(vec![
            Part::Text("a".into()),
            Part::Image(Image::new("image/png", vec![1])),
            Part::Text("b".into()),
        ])
        .unwrap();
        assert_eq!(ok.text(), "ab");
        let bad = Message::user_parts(vec![Part::ToolCall(ToolCall::new("1", "t", "{}"))]);
        assert!(matches!(bad, Err(AiError::Invalid(_))));
        let bad = Message::user_parts(vec![Part::ToolResult(ToolResult::ok("1", "x"))]);
        assert!(matches!(bad, Err(AiError::Invalid(_))));
    }

    #[test]
    fn request_builder() {
        let r = ChatRequest::new("m", vec![Message::user("q")])
            .with_system("s")
            .with_tools(vec![ToolDefinition::new("t", "d", serde_json::json!({}))])
            .with_max_output_tokens(10)
            .with_temperature(0.5)
            .with_response_format(ResponseFormat::Json);
        assert_eq!(r.system.as_deref(), Some("s"));
        assert_eq!(r.tools.len(), 1);
        assert_eq!(r.max_output_tokens, Some(10));
        assert_eq!(r.temperature, Some(0.5));
        assert_eq!(r.response_format, ResponseFormat::Json);
        assert_eq!(
            ChatRequest::new("m", vec![]).response_format,
            ResponseFormat::Text
        );
    }
}
