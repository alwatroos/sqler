// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ChatRequest` to the Messages request body, and SSE events to `ChatEvent`s.

use std::collections::{HashMap, VecDeque};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use sqler_ai_api::{
    AiError, Capabilities, ChatEvent, ChatRequest, Part, ResponseFormat, Role, StopReason,
    ToolCall, Usage,
};

use crate::error::{clean, from_event_error};
use crate::provider::DEFAULT_MAX_TOKENS;
use crate::sse::SseEvent;
use crate::wire::{
    Block, Delta, Event, ImageSource, MessagesBody, WireBlock, WireMessage, WireTool, WireUsage,
};

/// The most argument text buffered for one tool call.
const MAX_TOOL_ARGS_BYTES: usize = 8 * 1024 * 1024;

/// Builds the JSON body of `POST /v1/messages` with `stream: true`.
pub(crate) fn request_body(req: &ChatRequest) -> Result<Vec<u8>, AiError> {
    if req.response_format == ResponseFormat::Json {
        return Err(AiError::Unsupported(
            "JSON mode is not available for Anthropic".to_owned(),
        ));
    }
    let mut messages: Vec<WireMessage> = Vec::with_capacity(req.messages.len());
    for m in &req.messages {
        match m.role() {
            Role::User => {
                let mut blocks = Vec::new();
                for p in m.parts() {
                    match p {
                        Part::Text(t) if !t.is_empty() => {
                            blocks.push(WireBlock::Text { text: t.clone() });
                        }
                        Part::Image(img) => blocks.push(WireBlock::Image {
                            source: ImageSource {
                                kind: "base64",
                                media_type: img.media_type.clone(),
                                data: STANDARD.encode(&img.data),
                            },
                        }),
                        _ => {}
                    }
                }
                if !blocks.is_empty() {
                    push_message(&mut messages, "user", blocks);
                }
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                let text = m.text();
                if !text.is_empty() {
                    blocks.push(WireBlock::Text { text });
                }
                for p in m.parts() {
                    if let Part::ToolCall(c) = p {
                        blocks.push(WireBlock::ToolUse {
                            id: c.id.clone(),
                            name: c.name.clone(),
                            input: arguments_object(&c.arguments)?,
                        });
                    }
                }
                if !blocks.is_empty() {
                    push_message(&mut messages, "assistant", blocks);
                }
            }
            Role::Tool => {
                let blocks = m
                    .parts()
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolResult(r) => Some(WireBlock::ToolResult {
                            tool_use_id: r.call_id.clone(),
                            content: r.content.clone(),
                            is_error: r.is_error,
                        }),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !blocks.is_empty() {
                    push_message(&mut messages, "user", blocks);
                }
            }
            _ => return Err(AiError::Invalid("unsupported message role".to_owned())),
        }
    }
    let body = MessagesBody {
        model: req.model.clone(),
        max_tokens: req.max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        system: req.system.clone().filter(|s| !s.is_empty()),
        messages,
        tools: req
            .tools
            .iter()
            .map(|t| WireTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
            })
            .collect(),
        temperature: req.temperature,
        stream: true,
    };
    serde_json::to_vec(&body).map_err(|_| AiError::Invalid("cannot encode the request".to_owned()))
}

/// Adds a message, merging it into the previous one when the role is the
/// same: the API wants alternating turns, and several tool results belong in
/// one user turn.
fn push_message(out: &mut Vec<WireMessage>, role: &'static str, blocks: Vec<WireBlock>) {
    match out.last_mut() {
        Some(last) if last.role == role => last.content.extend(blocks),
        _ => out.push(WireMessage {
            role,
            content: blocks,
        }),
    }
}

/// The Messages API wants `tool_use.input` as a JSON object, not as text.
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

/// Maps `stop_reason` to a stop reason; `tools_emitted` says whether the
/// response contained a tool call.
pub(crate) fn stop_reason(reason: Option<&str>, tools_emitted: bool) -> StopReason {
    match reason {
        Some("end_turn" | "stop_sequence") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens" | "model_context_window_exceeded") => StopReason::MaxTokens,
        Some(other) => StopReason::Other(crate::error::truncate(other)),
        None if tools_emitted => StopReason::ToolUse,
        None => StopReason::EndTurn,
    }
}

/// What the table knows about a model id, without any I/O.
pub(crate) fn model_capabilities_for(id: &str) -> Capabilities {
    if id.starts_with("claude-") {
        Capabilities::new(true, false, true, Some(200_000))
    } else {
        Capabilities::new(true, false, false, None)
    }
}

/// A `tool_use` block being assembled.
#[derive(Debug)]
struct ToolBuf {
    id: String,
    name: String,
    /// The `input` of `content_block_start`, used when no delta comes.
    start_input: Option<Value>,
    args: String,
}

/// Per-response state of the event mapper.
#[derive(Debug, Default)]
pub(crate) struct SseMapper {
    blocks: HashMap<usize, ToolBuf>,
    tools_emitted: bool,
    stop: Option<String>,
    /// The latest cumulative counts.
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
    /// What has been reported through `Usage` events so far.
    seen: Usage,
}

impl SseMapper {
    /// Parses one event and pushes its `ChatEvent`s. Returns `true` after
    /// `message_stop`, whose `Done` is the last event pushed. `key` is only
    /// used to scrub error text.
    pub(crate) fn map_event(
        &mut self,
        ev: &SseEvent,
        key: &str,
        out: &mut VecDeque<Result<ChatEvent, AiError>>,
    ) -> Result<bool, AiError> {
        let parsed: Event = serde_json::from_str(&ev.data).map_err(|e| {
            AiError::Protocol(format!(
                "malformed SSE event: {}",
                clean(&e.to_string(), key)
            ))
        })?;
        match parsed {
            Event::MessageStart { message } => self.usage(message.usage.as_ref(), out),
            Event::ContentBlockStart {
                index,
                content_block,
            } => match content_block {
                Block::Text { text } => {
                    if !text.is_empty() {
                        out.push_back(Ok(ChatEvent::TextDelta(text)));
                    }
                }
                Block::ToolUse { id, name, input } => {
                    self.blocks.insert(
                        index,
                        ToolBuf {
                            id,
                            name,
                            start_input: input,
                            args: String::new(),
                        },
                    );
                }
                Block::Other => {}
            },
            Event::ContentBlockDelta { index, delta } => match delta {
                Delta::Text { text } => {
                    if !text.is_empty() {
                        out.push_back(Ok(ChatEvent::TextDelta(text)));
                    }
                }
                Delta::InputJson { partial_json } => {
                    if let Some(buf) = self.blocks.get_mut(&index) {
                        if buf.args.len() + partial_json.len() > MAX_TOOL_ARGS_BYTES {
                            return Err(AiError::Protocol(
                                "tool call arguments too large".to_owned(),
                            ));
                        }
                        buf.args.push_str(&partial_json);
                    }
                }
                Delta::Other => {}
            },
            Event::ContentBlockStop { index } => {
                if let Some(buf) = self.blocks.remove(&index) {
                    self.tools_emitted = true;
                    out.push_back(Ok(ChatEvent::ToolCall(ToolCall::new(
                        buf.id,
                        buf.name,
                        tool_args(buf.args, buf.start_input),
                    ))));
                }
            }
            Event::MessageDelta { delta, usage } => {
                if delta.stop_reason.is_some() {
                    self.stop = delta.stop_reason;
                }
                self.usage(usage.as_ref(), out);
            }
            Event::MessageStop => {
                out.push_back(Ok(ChatEvent::Done {
                    stop: stop_reason(self.stop.as_deref(), self.tools_emitted),
                }));
                return Ok(true);
            }
            Event::Error { error } => {
                return Err(from_event_error(
                    &clean(&error.kind, key),
                    &clean(&error.message, key),
                ));
            }
            Event::Ping | Event::Unknown => {}
        }
        Ok(false)
    }

    /// Takes the cumulative counts of a `usage` object and pushes the
    /// increment since the last report, if any.
    fn usage(&mut self, usage: Option<&WireUsage>, out: &mut VecDeque<Result<ChatEvent, AiError>>) {
        let Some(u) = usage else {
            return;
        };
        // A count that is missing keeps its last value.
        self.input = u.input_tokens.unwrap_or(self.input);
        self.cache_creation = u.cache_creation_input_tokens.unwrap_or(self.cache_creation);
        self.cache_read = u.cache_read_input_tokens.unwrap_or(self.cache_read);
        self.output = u.output_tokens.unwrap_or(self.output);
        let input = self
            .input
            .saturating_add(self.cache_creation)
            .saturating_add(self.cache_read);
        let inc = Usage::new(
            input.saturating_sub(self.seen.input_tokens),
            self.output.saturating_sub(self.seen.output_tokens),
        );
        if inc != Usage::default() {
            self.seen.add(inc);
            out.push_back(Ok(ChatEvent::Usage(inc)));
        }
    }
}

/// The arguments of a finished tool call: the buffered fragments, else the
/// `input` of the start event, else `{}`. Malformed text passes through raw.
fn tool_args(buffered: String, start_input: Option<Value>) -> String {
    if !buffered.trim().is_empty() {
        return buffered;
    }
    match start_input {
        Some(v) if !v.is_null() => v.to_string(),
        _ => "{}".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqler_ai_api::{Image, Message, ToolDefinition, ToolResult};

    use super::*;

    fn body(req: &ChatRequest) -> Value {
        serde_json::from_slice(&request_body(req).unwrap()).unwrap()
    }

    #[test]
    fn minimal_request() {
        let v = body(&ChatRequest::new("m", vec![Message::user("hi")]));
        assert_eq!(
            v,
            json!({
                "model":"m","max_tokens":4096,"stream":true,
                "messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]
            })
        );
    }

    #[test]
    fn full_request() {
        let img = Message::user_parts(vec![
            Part::Text("look ".into()),
            Part::Image(Image::new("image/png", vec![1, 2, 3])),
            Part::Text(String::new()),
            Part::Text("here".into()),
        ])
        .unwrap();
        let req = ChatRequest::new("m", vec![img])
            .with_system("be brief")
            .with_temperature(0.5)
            .with_max_output_tokens(77)
            .with_tools(vec![ToolDefinition::new(
                "run_sql",
                "Runs SQL",
                json!({"type":"object","properties":{"q":{"type":"string"}}}),
            )]);
        assert_eq!(
            body(&req),
            json!({
                "model":"m","max_tokens":77,"system":"be brief","temperature":0.5,"stream":true,
                "tools":[{"name":"run_sql","description":"Runs SQL",
                          "input_schema":{"type":"object","properties":{"q":{"type":"string"}}}}],
                "messages":[{"role":"user","content":[
                    {"type":"text","text":"look "},
                    {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}},
                    {"type":"text","text":"here"}
                ]}]
            })
        );
    }

    #[test]
    fn tool_turn_merges_results_into_one_user_message() {
        let calls = vec![
            ToolCall::new("t1", "a", r#"{"x":1}"#),
            ToolCall::new("t2", "b", ""),
        ];
        let req = ChatRequest::new(
            "m",
            vec![
                Message::user("go"),
                Message::assistant_with_tools(Some("ok".into()), calls),
                Message::tool_result(ToolResult::ok("t1", "r1")),
                Message::tool_result(ToolResult::error("t2", "boom")),
                Message::user("thanks"),
            ],
        );
        let v = body(&req);
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(
            msgs[1],
            json!({"role":"assistant","content":[
                {"type":"text","text":"ok"},
                {"type":"tool_use","id":"t1","name":"a","input":{"x":1}},
                {"type":"tool_use","id":"t2","name":"b","input":{}}
            ]})
        );
        assert_eq!(
            msgs[2],
            json!({"role":"user","content":[
                {"type":"tool_result","tool_use_id":"t1","content":"r1"},
                {"type":"tool_result","tool_use_id":"t2","content":"boom","is_error":true},
                {"type":"text","text":"thanks"}
            ]})
        );
    }

    #[test]
    fn empty_user_message_is_dropped() {
        let req = ChatRequest::new("m", vec![Message::assistant("a"), Message::user("")]);
        let v = body(&req);
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
        assert_eq!(v["messages"][0]["role"], "assistant");
    }

    #[test]
    fn empty_assistant_message_is_dropped() {
        let req = ChatRequest::new(
            "m",
            vec![
                Message::user("a"),
                Message::assistant(""),
                Message::user("b"),
            ],
        );
        let v = body(&req);
        // The two user turns merge once the empty assistant turn is gone.
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
        assert_eq!(v["messages"][0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn bad_arguments_are_invalid() {
        for bad in ["not json", "[1]", "\"s\"", "3"] {
            let req = ChatRequest::new(
                "m",
                vec![Message::assistant_with_tools(
                    None,
                    vec![ToolCall::new("t", "n", bad)],
                )],
            );
            assert!(
                matches!(request_body(&req), Err(AiError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn json_mode_is_unsupported() {
        let req = ChatRequest::new("m", vec![Message::user("x")])
            .with_response_format(ResponseFormat::Json);
        assert_eq!(
            request_body(&req).unwrap_err(),
            AiError::Unsupported("JSON mode is not available for Anthropic".into())
        );
    }

    fn ev(data: &Value) -> SseEvent {
        SseEvent {
            event: None,
            data: data.to_string(),
        }
    }

    /// Runs the events through a fresh mapper; the last bool is `done`.
    fn run(events: &[Value]) -> (Vec<Result<ChatEvent, AiError>>, bool) {
        let mut m = SseMapper::default();
        let mut out = VecDeque::new();
        let mut done = false;
        for e in events {
            match m.map_event(&ev(e), "KEY", &mut out) {
                Ok(d) => done = d,
                Err(e) => {
                    out.push_back(Err(e));
                    break;
                }
            }
            if done {
                break;
            }
        }
        (out.into_iter().collect(), done)
    }

    fn ok(events: &[Value]) -> Vec<ChatEvent> {
        run(events).0.into_iter().map(Result::unwrap).collect()
    }

    #[test]
    fn text_blocks() {
        let got = ok(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"ping"}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":""}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}),
            json!({"type":"message_stop"}),
        ]);
        assert_eq!(
            got,
            vec![
                ChatEvent::Usage(Usage::new(25, 1)),
                ChatEvent::TextDelta("Hello".into()),
                ChatEvent::Usage(Usage::new(0, 14)),
                ChatEvent::Done {
                    stop: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn nonempty_text_in_block_start_is_a_delta() {
        let got = ok(&[
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Hi"}}),
            json!({"type":"message_stop"}),
        ]);
        assert_eq!(got[0], ChatEvent::TextDelta("Hi".into()));
    }

    #[test]
    fn interleaved_tool_blocks_with_fragmented_json() {
        let d = |i: usize, s: &str| {
            json!({"type":"content_block_delta","index":i,
                   "delta":{"type":"input_json_delta","partial_json":s}})
        };
        let got = ok(&[
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"A","name":"a","input":{}}}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"B","name":"b","input":{}}}),
            d(1, "{\"q\": \"sel"),
            d(2, ""),
            d(2, "{\"n\":"),
            d(1, "ect 1\"}"),
            d(2, "2}"),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
            json!({"type":"message_stop"}),
        ]);
        assert_eq!(
            got,
            vec![
                ChatEvent::ToolCall(ToolCall::new("B", "b", "{\"n\":2}")),
                ChatEvent::ToolCall(ToolCall::new("A", "a", "{\"q\": \"select 1\"}")),
                ChatEvent::Done {
                    stop: StopReason::ToolUse
                },
            ]
        );
    }

    #[test]
    fn tool_block_without_delta_uses_the_start_input() {
        let got = ok(&[
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"A","name":"a","input":{"k":1}}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"B","name":"b"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_stop"}),
        ]);
        assert_eq!(
            got[0],
            ChatEvent::ToolCall(ToolCall::new("A", "a", "{\"k\":1}"))
        );
        assert_eq!(got[1], ChatEvent::ToolCall(ToolCall::new("B", "b", "{}")));
        // No stop reason arrived, but a tool call did.
        assert_eq!(
            got[2],
            ChatEvent::Done {
                stop: StopReason::ToolUse
            }
        );
    }

    #[test]
    fn malformed_tool_json_passes_through_raw() {
        let got = ok(&[
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"A","name":"a","input":{}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"x\":"}}),
            json!({"type":"content_block_stop","index":0}),
        ]);
        assert_eq!(
            got,
            vec![ChatEvent::ToolCall(ToolCall::new("A", "a", "{\"x\":"))]
        );
    }

    #[test]
    fn usage_is_reported_as_increments() {
        let got = ok(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}),
            json!({"type":"message_delta","delta":{},"usage":{"input_tokens":25,"output_tokens":15}}),
            json!({"type":"message_delta","delta":{},"usage":{"output_tokens":15}}),
            json!({"type":"message_stop"}),
        ]);
        assert_eq!(
            got,
            vec![
                ChatEvent::Usage(Usage::new(25, 1)),
                ChatEvent::Usage(Usage::new(0, 14)),
                ChatEvent::Done {
                    stop: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn cache_tokens_count_as_input() {
        let got = ok(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":5,"cache_creation_input_tokens":100,"cache_read_input_tokens":20,"output_tokens":1}}}),
            json!({"type":"message_delta","delta":{},"usage":{"output_tokens":3}}),
        ]);
        assert_eq!(
            got,
            vec![
                ChatEvent::Usage(Usage::new(125, 1)),
                ChatEvent::Usage(Usage::new(0, 2)),
            ]
        );
    }

    #[test]
    fn unknown_events_blocks_and_deltas_are_ignored() {
        let got = ok(&[
            json!({"type":"future_event","x":1}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"server_tool_use","id":"s","name":"web_search","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_stop"}),
        ]);
        assert_eq!(
            got,
            vec![ChatEvent::Done {
                stop: StopReason::EndTurn
            }]
        );
    }

    #[test]
    fn error_event_and_malformed_data() {
        let (items, done) = run(&[
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"a"}}),
            json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded KEY"}}),
        ]);
        assert!(!done);
        assert_eq!(items[0], Ok(ChatEvent::TextDelta("a".into())));
        assert_eq!(
            items[1],
            Err(AiError::Provider("overloaded: Overloaded ***".into()))
        );

        let mut m = SseMapper::default();
        let bad = SseEvent {
            event: None,
            data: "{not json".into(),
        };
        let e = m.map_event(&bad, "KEY", &mut VecDeque::new()).unwrap_err();
        assert!(matches!(&e, AiError::Protocol(s) if s.starts_with("malformed SSE event: ")));
        let wrong = SseEvent {
            event: None,
            data: r#"{"type":"content_block_delta","index":"x"}"#.into(),
        };
        assert!(matches!(
            m.map_event(&wrong, "KEY", &mut VecDeque::new()),
            Err(AiError::Protocol(_))
        ));
    }

    #[test]
    fn stop_reason_table() {
        use StopReason::{EndTurn, MaxTokens, Other, ToolUse};
        for (r, want) in [
            ("end_turn", EndTurn),
            ("stop_sequence", EndTurn),
            ("tool_use", ToolUse),
            ("max_tokens", MaxTokens),
            ("model_context_window_exceeded", MaxTokens),
            ("pause_turn", Other("pause_turn".into())),
            ("refusal", Other("refusal".into())),
        ] {
            assert_eq!(stop_reason(Some(r), false), want, "{r}");
        }
        assert_eq!(stop_reason(None, true), ToolUse);
        assert_eq!(stop_reason(None, false), EndTurn);
        // An explicit reason wins over the tool-call fallback.
        assert_eq!(stop_reason(Some("end_turn"), true), EndTurn);
    }

    #[test]
    fn model_table() {
        assert_eq!(
            model_capabilities_for("claude-sonnet-5-5"),
            Capabilities::new(true, false, true, Some(200_000))
        );
        assert_eq!(
            model_capabilities_for("gpt-x"),
            Capabilities::new(true, false, false, None)
        );
    }

    /// Recorded streams decoded with a cut at every byte offset map to the
    /// same `ChatEvent`s as the whole stream.
    #[test]
    fn mapper_output_is_independent_of_chunk_boundaries() {
        use crate::sse::SseDecoder;

        fn run(chunks: &[&[u8]]) -> Vec<String> {
            let mut d = SseDecoder::new();
            let mut m = SseMapper::default();
            let mut out = VecDeque::new();
            let mut evs = Vec::new();
            for c in chunks {
                evs.extend(d.push(c).0);
            }
            evs.extend(d.finish());
            for ev in &evs {
                if m.map_event(ev, "k", &mut out).unwrap_or(true) {
                    break;
                }
            }
            out.iter().map(|r| format!("{r:?}")).collect()
        }

        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
        for name in ["text.sse", "tool.sse", "final.sse", "error_event.sse"] {
            let bytes = std::fs::read(format!("{dir}{name}")).unwrap();
            let want = run(&[&bytes]);
            assert!(!want.is_empty(), "{name}");
            for cut in 0..=bytes.len() {
                let (a, b) = bytes.split_at(cut);
                assert_eq!(run(&[a, b]), want, "{name} cut {cut}");
            }
        }
    }
}
