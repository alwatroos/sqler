// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ChatRequest` to the Ollama request body, and stream lines to `ChatEvent`s.

use std::collections::VecDeque;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use sqler_ai_api::{
    AiError, Capabilities, ChatEvent, ChatRequest, Part, ResponseFormat, Role, StopReason, Usage,
};

use crate::error::{malformed, truncate};
use crate::wire::{
    ChatBody, ChatLine, Show, WireCall, WireMessage, WireOptions, WireTool, WireToolCall,
    WireToolFunction,
};

/// Builds the JSON body of `POST /api/chat` with `stream: true`.
pub(crate) fn request_body(req: &ChatRequest) -> Result<Vec<u8>, AiError> {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = &req.system {
        messages.push(plain("system", system.clone()));
    }
    // (call id, tool name) of every assistant tool call seen so far.
    let mut calls: Vec<(&str, &str)> = Vec::new();
    for m in &req.messages {
        match m.role() {
            Role::User => {
                let mut msg = plain("user", m.text());
                for p in m.parts() {
                    if let Part::Image(img) = p {
                        msg.images.push(STANDARD.encode(&img.data));
                    }
                }
                messages.push(msg);
            }
            Role::Assistant => {
                let mut msg = plain("assistant", m.text());
                for p in m.parts() {
                    if let Part::ToolCall(c) = p {
                        msg.tool_calls.push(WireToolCall {
                            function: WireCall {
                                name: c.name.clone(),
                                arguments: arguments_object(&c.arguments)?,
                            },
                        });
                        calls.push((&c.id, &c.name));
                    }
                }
                messages.push(msg);
            }
            Role::Tool => {
                for p in m.parts() {
                    if let Part::ToolResult(r) = p {
                        let name = calls
                            .iter()
                            .rev()
                            .find(|(id, _)| *id == r.call_id)
                            .map(|(_, n)| (*n).to_owned())
                            .ok_or_else(|| {
                                AiError::Invalid(
                                    "a tool result answers no earlier tool call".to_owned(),
                                )
                            })?;
                        let content = if r.is_error {
                            format!("error: {}", r.content)
                        } else {
                            r.content.clone()
                        };
                        let mut msg = plain("tool", content);
                        msg.tool_name = Some(name);
                        messages.push(msg);
                    }
                }
            }
            _ => return Err(AiError::Invalid("unsupported message role".to_owned())),
        }
    }
    let options =
        (req.temperature.is_some() || req.max_output_tokens.is_some()).then_some(WireOptions {
            temperature: req.temperature,
            num_predict: req.max_output_tokens,
        });
    let body = ChatBody {
        model: req.model.clone(),
        messages,
        tools: req
            .tools
            .iter()
            .map(|t| WireTool {
                kind: "function",
                function: WireToolFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect(),
        format: (req.response_format == ResponseFormat::Json).then_some("json"),
        options,
        stream: true,
    };
    serde_json::to_vec(&body).map_err(|_| AiError::Invalid("cannot encode the request".to_owned()))
}

fn plain(role: &'static str, content: String) -> WireMessage {
    WireMessage {
        role,
        content,
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_name: None,
    }
}

/// Ollama wants tool-call arguments as a JSON object, not as text.
fn arguments_object(text: &str) -> Result<Value, AiError> {
    if text.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    match serde_json::from_str::<Value>(text) {
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => Err(AiError::Invalid(
            "tool call arguments are not a JSON object".to_owned(),
        )),
        Err(e) => Err(AiError::Invalid(format!(
            "tool call arguments are not valid JSON: {e}"
        ))),
    }
}

/// Maps `done_reason` to a stop reason; `tools_emitted` says whether the
/// response contained a tool call.
pub(crate) fn stop_reason(reason: Option<&str>, tools_emitted: bool) -> StopReason {
    match reason {
        Some("length") => StopReason::MaxTokens,
        None | Some("stop") => {
            if tools_emitted {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        }
        Some(other) => StopReason::Other(truncate(other)),
    }
}

/// Per-response state of the line mapper.
#[derive(Debug, Default)]
pub(crate) struct LineMapper {
    tool_calls: usize,
}

impl LineMapper {
    /// Parses one NDJSON line and pushes its events. Returns `true` after the
    /// `done` line, whose `Done` event is the last one pushed.
    pub(crate) fn map_line(
        &mut self,
        line: &[u8],
        out: &mut VecDeque<Result<ChatEvent, AiError>>,
    ) -> Result<bool, AiError> {
        let parsed: ChatLine =
            serde_json::from_slice(line).map_err(|e| malformed("NDJSON line", &e))?;
        if let Some(err) = parsed.error {
            return Err(AiError::Provider(truncate(&err)));
        }
        if let Some(msg) = parsed.message {
            if let Some(text) = msg.content.filter(|t| !t.is_empty()) {
                out.push_back(Ok(ChatEvent::TextDelta(text)));
            }
            for call in msg.tool_calls.unwrap_or_default() {
                let n = self.tool_calls;
                self.tool_calls += 1;
                let id = call
                    .id
                    .filter(|i| !i.is_empty())
                    .unwrap_or_else(|| format!("call_{n}"));
                let args = call
                    .function
                    .arguments
                    .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                let args = serde_json::to_string(&args).map_err(|_| {
                    AiError::Protocol("tool call arguments cannot be encoded".to_owned())
                })?;
                out.push_back(Ok(ChatEvent::ToolCall(sqler_ai_api::ToolCall::new(
                    id,
                    call.function.name,
                    args,
                ))));
            }
        }
        if !parsed.done {
            return Ok(false);
        }
        out.push_back(Ok(ChatEvent::Usage(Usage::new(
            parsed.prompt_eval_count.unwrap_or(0),
            parsed.eval_count.unwrap_or(0),
        ))));
        out.push_back(Ok(ChatEvent::Done {
            stop: stop_reason(parsed.done_reason.as_deref(), self.tool_calls > 0),
        }));
        Ok(true)
    }
}

/// Per-model capabilities from an `/api/show` answer.
pub(crate) fn capabilities_from_show(show: &Show) -> Capabilities {
    let (tools, vision) = match &show.capabilities {
        Some(caps) => (
            caps.iter().any(|c| c == "tools"),
            caps.iter().any(|c| c == "vision"),
        ),
        // Documented defaults when the server does not report capabilities.
        None => (true, false),
    };
    Capabilities::new(tools, true, vision, context_length(show))
}

/// The first `<arch>.context_length` key of `model_info`.
pub(crate) fn context_length(show: &Show) -> Option<u32> {
    show.model_info
        .as_ref()?
        .iter()
        .find(|(k, _)| k.ends_with(".context_length"))
        .and_then(|(_, v)| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqler_ai_api::{Image, Message, ToolCall, ToolDefinition, ToolResult};

    use super::*;

    fn body(req: &ChatRequest) -> Value {
        serde_json::from_slice(&request_body(req).unwrap()).unwrap()
    }

    #[test]
    fn minimal_request() {
        let v = body(&ChatRequest::new("m", vec![Message::user("hi")]));
        assert_eq!(
            v,
            json!({"model":"m","messages":[{"role":"user","content":"hi"}],"stream":true})
        );
    }

    #[test]
    fn full_request() {
        let img = Message::user_parts(vec![
            Part::Text("look ".into()),
            Part::Image(Image::new("image/png", vec![1, 2, 3])),
            Part::Text("here".into()),
        ])
        .unwrap();
        let req = ChatRequest::new("m", vec![img])
            .with_system("sys")
            .with_tools(vec![ToolDefinition::new(
                "t",
                "d",
                json!({"type":"object"}),
            )])
            .with_temperature(0.5)
            .with_max_output_tokens(64)
            .with_response_format(ResponseFormat::Json);
        assert_eq!(
            body(&req),
            json!({
                "model":"m",
                "messages":[
                    {"role":"system","content":"sys"},
                    {"role":"user","content":"look here","images":["AQID"]}
                ],
                "tools":[{"type":"function","function":{"name":"t","description":"d","parameters":{"type":"object"}}}],
                "format":"json",
                "options":{"temperature":0.5,"num_predict":64},
                "stream":true
            })
        );
    }

    fn tool_turn(result: ToolResult) -> ChatRequest {
        ChatRequest::new(
            "m",
            vec![
                Message::user("q"),
                Message::assistant_with_tools(
                    None,
                    vec![
                        ToolCall::new("a", "first", "{\"x\":1}"),
                        ToolCall::new("b", "second", ""),
                    ],
                ),
                Message::tool_result(result),
            ],
        )
    }

    #[test]
    fn tool_result_gets_the_tool_name() {
        let v = body(&tool_turn(ToolResult::ok("a", "42")));
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(
            msgs[1]["tool_calls"],
            json!([{"function":{"name":"first","arguments":{"x":1}}},{"function":{"name":"second","arguments":{}}}])
        );
        assert_eq!(
            msgs[2],
            json!({"role":"tool","content":"42","tool_name":"first"})
        );
    }

    #[test]
    fn tool_error_is_prefixed() {
        let v = body(&tool_turn(ToolResult::error("b", "boom")));
        assert_eq!(
            v["messages"][2],
            json!({"role":"tool","content":"error: boom","tool_name":"second"})
        );
    }

    #[test]
    fn latest_earlier_call_with_the_id_wins() {
        let req = ChatRequest::new(
            "m",
            vec![
                Message::assistant_with_tools(None, vec![ToolCall::new("a", "old", "{}")]),
                Message::assistant_with_tools(None, vec![ToolCall::new("a", "new", "{}")]),
                Message::tool_result(ToolResult::ok("a", "r")),
            ],
        );
        assert_eq!(body(&req)["messages"][2]["tool_name"], "new");
    }

    #[test]
    fn unknown_call_id_is_invalid() {
        let req = ChatRequest::new(
            "m",
            vec![Message::tool_result(ToolResult::ok("zz", "secret prompt"))],
        );
        let e = request_body(&req).unwrap_err();
        assert!(matches!(&e, AiError::Invalid(m) if !m.contains("secret")));
    }

    #[test]
    fn bad_tool_arguments_are_invalid() {
        for bad in ["{not json", "[1]", "3"] {
            let req = ChatRequest::new(
                "m",
                vec![Message::assistant_with_tools(
                    None,
                    vec![ToolCall::new("a", "t", bad)],
                )],
            );
            assert!(
                matches!(request_body(&req), Err(AiError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn stop_reason_table() {
        assert_eq!(stop_reason(Some("length"), true), StopReason::MaxTokens);
        assert_eq!(stop_reason(Some("stop"), false), StopReason::EndTurn);
        assert_eq!(stop_reason(None, false), StopReason::EndTurn);
        assert_eq!(stop_reason(Some("stop"), true), StopReason::ToolUse);
        assert_eq!(stop_reason(None, true), StopReason::ToolUse);
        assert_eq!(
            stop_reason(Some("load"), false),
            StopReason::Other("load".into())
        );
    }

    fn map(lines: &[&str]) -> Vec<Result<ChatEvent, AiError>> {
        let mut m = LineMapper::default();
        let mut out = VecDeque::new();
        for l in lines {
            if m.map_line(l.as_bytes(), &mut out).unwrap() {
                break;
            }
        }
        out.into()
    }

    #[test]
    fn synthesizes_ids_and_serializes_arguments() {
        let ev = map(&[
            r#"{"message":{"content":"","tool_calls":[{"function":{"name":"a","arguments":{"k": "v"}}},{"id":"real","function":{"name":"b","arguments":{}}}]},"done":false}"#,
            r#"{"message":{"tool_calls":[{"function":{"name":"c"}}]},"done":false}"#,
            r#"{"done":true,"done_reason":"stop","prompt_eval_count":7,"eval_count":3}"#,
        ]);
        assert_eq!(
            ev,
            vec![
                Ok(ChatEvent::ToolCall(ToolCall::new(
                    "call_0",
                    "a",
                    r#"{"k":"v"}"#
                ))),
                Ok(ChatEvent::ToolCall(ToolCall::new("real", "b", "{}"))),
                Ok(ChatEvent::ToolCall(ToolCall::new("call_2", "c", "{}"))),
                Ok(ChatEvent::Usage(Usage::new(7, 3))),
                Ok(ChatEvent::Done {
                    stop: StopReason::ToolUse
                }),
            ]
        );
    }

    #[test]
    fn done_without_counts_reports_zero_usage() {
        let ev = map(&[r#"{"done":true}"#]);
        assert_eq!(
            ev,
            vec![
                Ok(ChatEvent::Usage(Usage::new(0, 0))),
                Ok(ChatEvent::Done {
                    stop: StopReason::EndTurn
                }),
            ]
        );
    }

    #[test]
    fn error_and_malformed_lines() {
        let mut m = LineMapper::default();
        let mut out = VecDeque::new();
        let e = m.map_line(br#"{"error":"boom"}"#, &mut out).unwrap_err();
        assert_eq!(e, AiError::Provider("boom".into()));
        let e = m.map_line(b"{oops", &mut out).unwrap_err();
        assert!(matches!(e, AiError::Protocol(m) if m.starts_with("malformed NDJSON line: ")));
    }

    #[test]
    fn capabilities_from_show_answers() {
        let s: Show = serde_json::from_value(json!({
            "capabilities":["completion","tools","vision"],
            "model_info":{"general.architecture":"llama","llama.context_length":8192}
        }))
        .unwrap();
        assert_eq!(
            capabilities_from_show(&s),
            Capabilities::new(true, true, true, Some(8192))
        );
        let s: Show = serde_json::from_value(json!({"capabilities":["completion"]})).unwrap();
        assert_eq!(
            capabilities_from_show(&s),
            Capabilities::new(false, true, false, None)
        );
        let s: Show = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            capabilities_from_show(&s),
            Capabilities::new(true, true, false, None)
        );
    }
}
