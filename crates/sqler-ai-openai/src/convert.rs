// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ChatRequest` to the Chat Completions body, and SSE events to `ChatEvent`s.

use std::collections::{BTreeMap, VecDeque};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use sqler_ai_api::{
    AiError, ChatEvent, ChatRequest, Part, ResponseFormat, Role, StopReason, ToolCall, Usage,
};

use crate::error::{scrub, stream_error, truncate};
use crate::provider::Flavor;
use crate::sse::SseEvent;
use crate::wire::{
    ChatBody, Chunk, StreamOptions, ToolCallChunk, WireCall, WireContent, WireImageUrl,
    WireMessage, WirePart, WireTool, WireToolCall, WireToolFunction, WireUsage,
};

/// Builds the JSON body of `POST /chat/completions` with `stream: true`.
pub(crate) fn request_body(req: &ChatRequest, flavor: Flavor) -> Result<Vec<u8>, AiError> {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = &req.system {
        messages.push(plain("system", system.clone()));
    }
    for m in &req.messages {
        match m.role() {
            Role::User => messages.push(user_message(m.parts(), &m.text())),
            Role::Assistant => {
                let text = m.text();
                let tool_calls: Vec<WireToolCall> = m
                    .parts()
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolCall(c) => Some(WireToolCall {
                            id: c.id.clone(),
                            kind: "function",
                            function: WireCall {
                                name: c.name.clone(),
                                arguments: if c.arguments.trim().is_empty() {
                                    "{}".to_owned()
                                } else {
                                    c.arguments.clone()
                                },
                            },
                        }),
                        _ => None,
                    })
                    .collect();
                let content = if text.is_empty() && !tool_calls.is_empty() {
                    None
                } else {
                    Some(WireContent::Text(text))
                };
                messages.push(WireMessage {
                    role: "assistant",
                    content,
                    tool_calls,
                    tool_call_id: None,
                });
            }
            Role::Tool => {
                for p in m.parts() {
                    if let Part::ToolResult(r) = p {
                        let content = if r.is_error {
                            format!("error: {}", r.content)
                        } else {
                            r.content.clone()
                        };
                        let mut msg = plain("tool", content);
                        msg.tool_call_id = Some(r.call_id.clone());
                        messages.push(msg);
                    }
                }
            }
            _ => return Err(AiError::Invalid("unsupported message role".to_owned())),
        }
    }
    let (max_completion_tokens, max_tokens) = match flavor {
        Flavor::OpenAi => (req.max_output_tokens, None),
        Flavor::Compatible => (None, req.max_output_tokens),
    };
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
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
        temperature: req.temperature,
        max_completion_tokens,
        max_tokens,
        response_format: (req.response_format == ResponseFormat::Json).then_some(
            crate::wire::ResponseFormat {
                kind: "json_object",
            },
        ),
    };
    serde_json::to_vec(&body).map_err(|_| AiError::Invalid("cannot encode the request".to_owned()))
}

fn plain(role: &'static str, content: String) -> WireMessage {
    WireMessage {
        role,
        content: Some(WireContent::Text(content)),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

fn user_message(parts: &[Part], text: &str) -> WireMessage {
    if !parts.iter().any(|p| matches!(p, Part::Image(_))) {
        return plain("user", text.to_owned());
    }
    let wire = parts
        .iter()
        .filter_map(|p| match p {
            Part::Text(t) => Some(WirePart::Text { text: t.clone() }),
            Part::Image(img) => Some(WirePart::ImageUrl {
                image_url: WireImageUrl {
                    url: format!(
                        "data:{};base64,{}",
                        img.media_type,
                        STANDARD.encode(&img.data)
                    ),
                },
            }),
            _ => None,
        })
        .collect();
    WireMessage {
        role: "user",
        content: Some(WireContent::Parts(wire)),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

/// Maps `finish_reason` to a stop reason; `tools_emitted` says whether the
/// response contained a tool call.
pub(crate) fn stop_reason(reason: Option<&str>, tools_emitted: bool) -> StopReason {
    match reason {
        Some("tool_calls" | "function_call") => StopReason::ToolUse,
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

#[derive(Debug, Default)]
struct Partial {
    id: Option<String>,
    name: Option<String>,
    args: String,
}

/// Per-response state of the chunk mapper.
#[derive(Default)]
pub(crate) struct ChunkMapper {
    /// The API key, scrubbed out of server error text.
    key: Option<String>,
    partials: BTreeMap<u32, Partial>,
    /// The slot the last fragment went to.
    last_slot: Option<u32>,
    /// The first slot above every slot used so far.
    next_free: u32,
    emitted_calls: usize,
    finish: Option<String>,
    usage: Option<WireUsage>,
}

impl std::fmt::Debug for ChunkMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkMapper")
            .field("key", &self.key.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

impl ChunkMapper {
    pub(crate) fn new(key: Option<String>) -> Self {
        Self {
            key,
            ..Self::default()
        }
    }

    /// Maps one SSE event and pushes its events. Returns `true` after
    /// `[DONE]`, whose `Done` event is the last one pushed.
    pub(crate) fn map_event(
        &mut self,
        ev: &SseEvent,
        out: &mut VecDeque<Result<ChatEvent, AiError>>,
    ) -> Result<bool, AiError> {
        let data = ev.data.trim();
        if ev.event.as_deref() == Some("error") {
            return Err(match serde_json::from_str::<Value>(data) {
                Ok(v) => stream_error(&v, self.key.as_deref()),
                Err(_) => AiError::Provider(scrub(data, self.key.as_deref())),
            });
        }
        if data == "[DONE]" {
            self.finish_stream(out)?;
            return Ok(true);
        }
        if data.is_empty() {
            return Ok(false);
        }
        let chunk: Chunk = serde_json::from_str(data)
            .map_err(|e| AiError::Protocol(format!("malformed SSE data: {e}")))?;
        if let Some(err) = chunk.error.filter(|e| !e.is_null()) {
            return Err(stream_error(&json!({ "error": err }), self.key.as_deref()));
        }
        if let Some(u) = chunk.usage {
            self.usage = Some(u);
        }
        let Some(choice) = chunk
            .choices
            .into_iter()
            .find(|c| c.index.unwrap_or(0) == 0)
        else {
            return Ok(false);
        };
        if let Some(delta) = choice.delta {
            for text in [delta.content, delta.refusal].into_iter().flatten() {
                if !text.is_empty() {
                    out.push_back(Ok(ChatEvent::TextDelta(text)));
                }
            }
            for frag in delta.tool_calls.unwrap_or_default() {
                self.add_fragment(frag);
            }
        }
        if let Some(reason) = choice.finish_reason
            && self.finish.is_none()
        {
            self.finish = Some(reason);
            self.flush(out)?;
        }
        Ok(false)
    }

    /// The stream ended without `[DONE]`. A server that sent a
    /// `finish_reason` is accepted; otherwise the response is cut short.
    pub(crate) fn finish_at_eof(
        &mut self,
        out: &mut VecDeque<Result<ChatEvent, AiError>>,
    ) -> Result<(), AiError> {
        if self.finish.is_some() {
            self.finish_stream(out)
        } else {
            Err(AiError::Protocol("stream ended before [DONE]".to_owned()))
        }
    }

    fn add_fragment(&mut self, frag: ToolCallChunk) {
        let id = frag.id.as_deref().filter(|i| !i.is_empty());
        let slot = match frag.index {
            Some(i) => i,
            None => match id {
                Some(id) => self
                    .partials
                    .iter()
                    .find(|(_, p)| p.id.as_deref() == Some(id))
                    .map_or(self.next_free, |(i, _)| *i),
                None => self.last_slot.unwrap_or(self.next_free),
            },
        };
        self.last_slot = Some(slot);
        self.next_free = self.next_free.max(slot.saturating_add(1));
        let p = self.partials.entry(slot).or_default();
        if let Some(id) = id {
            p.id = Some(id.to_owned());
        }
        if let Some(f) = frag.function {
            if let Some(name) = f.name.filter(|n| !n.is_empty()) {
                p.name = Some(name);
            }
            if let Some(args) = f.arguments {
                p.args.push_str(&args);
            }
        }
    }

    /// Emits the collected tool calls in index order.
    fn flush(&mut self, out: &mut VecDeque<Result<ChatEvent, AiError>>) -> Result<(), AiError> {
        for (index, p) in std::mem::take(&mut self.partials) {
            let name = p
                .name
                .ok_or_else(|| AiError::Protocol("tool call without a name".to_owned()))?;
            let id = p.id.unwrap_or_else(|| format!("call_{index}"));
            self.emitted_calls += 1;
            out.push_back(Ok(ChatEvent::ToolCall(ToolCall::new(id, name, p.args))));
        }
        Ok(())
    }

    fn finish_stream(
        &mut self,
        out: &mut VecDeque<Result<ChatEvent, AiError>>,
    ) -> Result<(), AiError> {
        self.flush(out)?;
        if let Some(u) = self.usage.take() {
            out.push_back(Ok(ChatEvent::Usage(Usage::new(
                u.prompt_tokens.unwrap_or(0),
                u.completion_tokens.unwrap_or(0),
            ))));
        }
        out.push_back(Ok(ChatEvent::Done {
            stop: stop_reason(self.finish.as_deref(), self.emitted_calls > 0),
        }));
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_wraps)]
mod tests {
    use serde_json::json;
    use sqler_ai_api::{Image, Message, ToolDefinition, ToolResult};

    use super::*;

    fn body(req: &ChatRequest, flavor: Flavor) -> Value {
        serde_json::from_slice(&request_body(req, flavor).unwrap()).unwrap()
    }

    #[test]
    fn mapper_debug_hides_the_key() {
        let m = ChunkMapper::new(Some("sk-secret".to_owned()));
        let d = format!("{m:?}");
        assert!(!d.contains("sk-secret"));
        assert!(d.contains("***"));
    }

    #[test]
    fn minimal_request() {
        let v = body(
            &ChatRequest::new("m", vec![Message::user("hi")]),
            Flavor::OpenAi,
        );
        assert_eq!(
            v,
            json!({
                "model":"m",
                "messages":[{"role":"user","content":"hi"}],
                "stream":true,
                "stream_options":{"include_usage":true}
            })
        );
    }

    #[test]
    fn max_tokens_field_depends_on_the_flavor() {
        let req = ChatRequest::new("m", vec![Message::user("hi")]).with_max_output_tokens(64);
        let o = body(&req, Flavor::OpenAi);
        assert_eq!(o["max_completion_tokens"], 64);
        assert!(o.get("max_tokens").is_none());
        let c = body(&req, Flavor::Compatible);
        assert_eq!(c["max_tokens"], 64);
        assert!(c.get("max_completion_tokens").is_none());
    }

    #[test]
    fn full_request_with_image_and_json_mode() {
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
            .with_response_format(ResponseFormat::Json);
        assert_eq!(
            body(&req, Flavor::OpenAi),
            json!({
                "model":"m",
                "messages":[
                    {"role":"system","content":"sys"},
                    {"role":"user","content":[
                        {"type":"text","text":"look "},
                        {"type":"image_url","image_url":{"url":"data:image/png;base64,AQID"}},
                        {"type":"text","text":"here"}
                    ]}
                ],
                "tools":[{"type":"function","function":{"name":"t","description":"d","parameters":{"type":"object"}}}],
                "stream":true,
                "stream_options":{"include_usage":true},
                "temperature":0.5,
                "response_format":{"type":"json_object"}
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
    fn assistant_tool_calls_have_null_content_and_raw_arguments() {
        let v = body(&tool_turn(ToolResult::ok("a", "42")), Flavor::OpenAi);
        assert_eq!(
            v["messages"][1],
            json!({"role":"assistant","content":null,"tool_calls":[
                {"id":"a","type":"function","function":{"name":"first","arguments":"{\"x\":1}"}},
                {"id":"b","type":"function","function":{"name":"second","arguments":"{}"}}
            ]})
        );
        assert_eq!(
            v["messages"][2],
            json!({"role":"tool","content":"42","tool_call_id":"a"})
        );
    }

    #[test]
    fn assistant_text_with_tool_calls_keeps_the_text() {
        let req = ChatRequest::new(
            "m",
            vec![Message::assistant_with_tools(
                Some("let me check".into()),
                vec![ToolCall::new("a", "t", "{}")],
            )],
        );
        assert_eq!(
            body(&req, Flavor::OpenAi)["messages"][0]["content"],
            "let me check"
        );
    }

    #[test]
    fn tool_error_is_prefixed() {
        let v = body(&tool_turn(ToolResult::error("b", "boom")), Flavor::OpenAi);
        assert_eq!(
            v["messages"][2],
            json!({"role":"tool","content":"error: boom","tool_call_id":"b"})
        );
    }

    #[test]
    fn two_tool_results_make_two_messages() {
        let req = ChatRequest::new(
            "m",
            vec![
                Message::tool_result(ToolResult::ok("a", "1")),
                Message::tool_result(ToolResult::ok("b", "2")),
            ],
        );
        let v = body(&req, Flavor::OpenAi);
        assert_eq!(v["messages"].as_array().unwrap().len(), 2);
    }

    // ---- ChunkMapper ----

    fn ev(data: &str) -> SseEvent {
        SseEvent {
            event: None,
            data: data.to_owned(),
        }
    }

    /// Feeds the events and returns what was queued; the last item may be an error.
    fn run(events: &[&str]) -> Vec<Result<ChatEvent, AiError>> {
        let mut m = ChunkMapper::new(Some("sk-secret".into()));
        let mut q = VecDeque::new();
        for e in events {
            match m.map_event(&ev(e), &mut q) {
                Ok(true) => break,
                Ok(false) => {}
                Err(err) => {
                    q.push_back(Err(err));
                    break;
                }
            }
        }
        q.into_iter().collect()
    }

    fn tc(id: &str, name: &str, args: &str) -> Result<ChatEvent, AiError> {
        Ok(ChatEvent::ToolCall(ToolCall::new(id, name, args)))
    }

    fn done(stop: StopReason) -> Result<ChatEvent, AiError> {
        Ok(ChatEvent::Done { stop })
    }

    #[test]
    fn text_usage_and_done() {
        let got = run(&[
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"Hi"}}],"usage":null}"#,
            r#"{"choices":[{"index":0,"delta":{"refusal":" no"},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                Ok(ChatEvent::TextDelta("Hi".into())),
                Ok(ChatEvent::TextDelta(" no".into())),
                Ok(ChatEvent::Usage(Usage::new(9, 2))),
                done(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn no_usage_chunk_means_no_usage_event() {
        let got = run(&[
            r#"{"choices":[{"delta":{"content":"a"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                Ok(ChatEvent::TextDelta("a".into())),
                done(StopReason::EndTurn)
            ]
        );
    }

    #[test]
    fn parallel_calls_with_interleaved_fragments() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c0","type":"function","function":{"name":"a","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c1","type":"function","function":{"name":"b","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"y\""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"x\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":":2}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                tc("c0", "a", "{\"x\":1}"),
                tc("c1", "b", "{\"y\":2}"),
                done(StopReason::ToolUse)
            ]
        );
    }

    #[test]
    fn a_call_sent_whole_without_index() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"x","function":{"name":"f","arguments":"{\"a\":1}"}}]},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![tc("x", "f", "{\"a\":1}"), done(StopReason::ToolUse)]
        );
    }

    #[test]
    fn indexless_fragments_follow_the_id_or_the_last_slot() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"p","function":{"name":"one","arguments":"{"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"q","function":{"name":"two","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"p","function":{"arguments":" "}}]}}]}"#,
            r#"{"choices":[{"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                tc("p", "one", "{} "),
                tc("q", "two", "{}"),
                done(StopReason::ToolUse)
            ]
        );
    }

    #[test]
    fn missing_id_becomes_call_index() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"name":"f","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![tc("call_2", "f", "{}"), done(StopReason::ToolUse)]
        );
    }

    #[test]
    fn missing_name_is_a_protocol_error() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![Err(AiError::Protocol("tool call without a name".into()))]
        );
    }

    #[test]
    fn stop_reason_table() {
        use StopReason::{EndTurn, MaxTokens, Other, ToolUse};
        assert_eq!(stop_reason(Some("stop"), false), EndTurn);
        assert_eq!(stop_reason(None, false), EndTurn);
        assert_eq!(stop_reason(Some("stop"), true), ToolUse);
        assert_eq!(stop_reason(None, true), ToolUse);
        assert_eq!(stop_reason(Some("tool_calls"), false), ToolUse);
        assert_eq!(stop_reason(Some("function_call"), false), ToolUse);
        assert_eq!(stop_reason(Some("length"), true), MaxTokens);
        assert_eq!(
            stop_reason(Some("content_filter"), false),
            Other("content_filter".into())
        );
        let StopReason::Other(s) = stop_reason(Some(&"x".repeat(500)), false) else {
            panic!("expected Other");
        };
        assert_eq!(s.len(), 200);
    }

    #[test]
    fn only_the_first_finish_reason_counts() {
        let got = run(&[
            r#"{"choices":[{"delta":{"content":"a"},"finish_reason":"length"}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(got.last().unwrap(), &done(StopReason::MaxTokens));
    }

    #[test]
    fn other_choices_are_ignored() {
        let got = run(&[
            r#"{"choices":[{"index":1,"delta":{"content":"no"}}]}"#,
            "[DONE]",
        ]);
        assert_eq!(got, vec![done(StopReason::EndTurn)]);
    }

    #[test]
    fn error_payloads_and_error_events() {
        let got = run(&[r#"{"error":{"message":"boom sk-secret","type":"server_error"}}"#]);
        assert_eq!(got, vec![Err(AiError::Provider("boom ***".into()))]);
        let mut m = ChunkMapper::new(None);
        let mut q = VecDeque::new();
        let e = SseEvent {
            event: Some("error".into()),
            data: r#"{"message":"overloaded"}"#.into(),
        };
        assert_eq!(
            m.map_event(&e, &mut q),
            Err(AiError::Provider("overloaded".into()))
        );
    }

    #[test]
    fn malformed_data_is_a_protocol_error() {
        let got = run(&["{not json"]);
        let Err(AiError::Protocol(m)) = &got[0] else {
            panic!("expected Protocol");
        };
        assert!(m.starts_with("malformed SSE data: "));
    }

    #[test]
    fn eof_with_and_without_a_finish_reason() {
        let mut m = ChunkMapper::new(None);
        let mut q = VecDeque::new();
        m.map_event(&ev(r#"{"choices":[{"delta":{"content":"a"}}]}"#), &mut q)
            .unwrap();
        assert_eq!(
            m.finish_at_eof(&mut q),
            Err(AiError::Protocol("stream ended before [DONE]".into()))
        );
        m.map_event(&ev(r#"{"choices":[{"finish_reason":"stop"}]}"#), &mut q)
            .unwrap();
        m.finish_at_eof(&mut q).unwrap();
        assert_eq!(
            q.back().unwrap(),
            &Ok(ChatEvent::Done {
                stop: StopReason::EndTurn
            })
        );
    }

    #[test]
    fn done_flushes_calls_never_finished() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"f","arguments":"{}"}}]}}]}"#,
            "[DONE]",
        ]);
        assert_eq!(got, vec![tc("a", "f", "{}"), done(StopReason::ToolUse)]);
    }
}
