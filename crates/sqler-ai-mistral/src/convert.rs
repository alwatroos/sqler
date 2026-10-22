// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ChatRequest` to the Chat Completions body, and SSE events to `ChatEvent`s.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use sqler_ai_api::{
    AiError, ChatEvent, ChatRequest, Part, ResponseFormat, Role, StopReason, ToolCall, Usage,
};

use crate::error::{malformed, scrub, stream_error, truncate};
use crate::sse::SseEvent;
use crate::wire::{
    ChatBody, Chunk, ToolCallChunk, WireCall, WireContent, WireMessage, WirePart, WireTool,
    WireToolCall, WireToolFunction, WireUsage,
};

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Maps a tool-call id to one the API accepts: exactly nine ASCII letters or
/// digits. A matching id is returned unchanged. Any other id becomes the
/// FNV-1a 64 hash of its bytes in base 62, least significant digit first,
/// cut to nine characters. The same function is applied to the call and to its
/// result, so the pair stays matched.
#[cfg(test)]
pub(crate) fn normalize_call_id(id: &str) -> String {
    if matches_rule(id) {
        return id.to_owned();
    }
    hashed_id(id, 0)
}

fn matches_rule(id: &str) -> bool {
    id.len() == 9 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The hashed form of `id`; `salt` > 0 gives the next candidate after a collision.
fn hashed_id(id: &str, salt: u32) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    if salt > 0 {
        for b in salt.to_le_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    for b in id.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let mut out = String::with_capacity(9);
    for _ in 0..9 {
        let digit = usize::try_from(hash % 62).unwrap_or(0);
        out.push(char::from(BASE62[digit]));
        hash /= 62;
    }
    out
}

/// Wire ids for every call id of a request: ids that already match the rule
/// keep their value; the others are hashed, and re-hashed with a growing salt
/// while the result is taken, so two distinct ids never share a wire id.
fn wire_ids(req: &ChatRequest) -> HashMap<String, String> {
    let mut originals: Vec<&str> = Vec::new();
    for m in &req.messages {
        for p in m.parts() {
            match p {
                Part::ToolCall(c) => originals.push(&c.id),
                Part::ToolResult(r) => originals.push(&r.call_id),
                _ => {}
            }
        }
    }
    let mut map: HashMap<String, String> = HashMap::new();
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    for id in &originals {
        if matches_rule(id) {
            map.insert((*id).to_owned(), (*id).to_owned());
            taken.insert((*id).to_owned());
        }
    }
    for id in originals {
        if map.contains_key(id) {
            continue;
        }
        let mut salt = 0;
        let wire = loop {
            let candidate = hashed_id(id, salt);
            if !taken.contains(&candidate) {
                break candidate;
            }
            salt += 1;
        };
        taken.insert(wire.clone());
        map.insert(id.to_owned(), wire);
    }
    map
}

/// Builds the JSON body of `POST /v1/chat/completions` with `stream: true`.
pub(crate) fn request_body(req: &ChatRequest) -> Result<Vec<u8>, AiError> {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = &req.system {
        messages.push(plain("system", system.clone()));
    }
    // The name of each assistant tool call, by its original id, for the
    // `name` of the matching tool message.
    let mut call_names: HashMap<String, String> = HashMap::new();
    let wire = wire_ids(req);
    let wire_id = |id: &str| wire.get(id).cloned().unwrap_or_else(|| hashed_id(id, 0));
    for m in &req.messages {
        match m.role() {
            Role::User => messages.push(user_message(m.parts(), &m.text())),
            Role::Assistant => {
                let tool_calls: Vec<WireToolCall> = m
                    .parts()
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolCall(c) => {
                            call_names.insert(c.id.clone(), c.name.clone());
                            Some(WireToolCall {
                                id: wire_id(&c.id),
                                kind: "function",
                                function: WireCall {
                                    name: c.name.clone(),
                                    arguments: if c.arguments.trim().is_empty() {
                                        "{}".to_owned()
                                    } else {
                                        c.arguments.clone()
                                    },
                                },
                            })
                        }
                        _ => None,
                    })
                    .collect();
                let mut msg = plain("assistant", m.text());
                msg.tool_calls = tool_calls;
                messages.push(msg);
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
                        msg.tool_call_id = Some(wire_id(&r.call_id));
                        msg.name = call_names.get(&r.call_id).cloned();
                        messages.push(msg);
                    }
                }
            }
            _ => return Err(AiError::Invalid("unsupported message role".to_owned())),
        }
    }
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
        temperature: req.temperature,
        max_tokens: req.max_output_tokens,
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
        content: WireContent::Text(content),
        tool_calls: Vec::new(),
        tool_call_id: None,
        name: None,
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
                image_url: format!(
                    "data:{};base64,{}",
                    img.media_type,
                    STANDARD.encode(&img.data)
                ),
            }),
            _ => None,
        })
        .collect();
    WireMessage {
        role: "user",
        content: WireContent::Parts(wire),
        tool_calls: Vec::new(),
        tool_call_id: None,
        name: None,
    }
}

/// Maps `finish_reason` to a stop reason; `tools_emitted` says whether the
/// response contained a tool call. `error` is handled before this is called.
pub(crate) fn stop_reason(reason: Option<&str>, tools_emitted: bool) -> StopReason {
    match reason {
        Some("tool_calls") => StopReason::ToolUse,
        Some("length" | "model_length") => StopReason::MaxTokens,
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
    index: Option<u32>,
    name: Option<String>,
    args: String,
}

impl Partial {
    /// The arguments are a complete JSON object.
    fn args_complete(&self) -> bool {
        serde_json::from_str::<Value>(&self.args).is_ok_and(|v| v.is_object())
    }

    /// Arguments that may be sent: a JSON object, or nothing at all.
    fn args_usable(&self) -> bool {
        self.args.trim().is_empty() || self.args_complete()
    }
}

/// Per-response state of the chunk mapper.
#[derive(Default)]
pub(crate) struct ChunkMapper {
    /// The API key, scrubbed out of server error text.
    key: Option<String>,
    /// Tool calls in the order they were opened.
    partials: Vec<Partial>,
    /// The slot that last used each `index`.
    by_index: BTreeMap<u32, usize>,
    /// The slot the last fragment went to.
    last_slot: Option<usize>,
    emitted_calls: usize,
    /// Ids made up for calls without one, counted per mapper.
    generated: u64,
    /// When set, ids are numbered from this counter, which spans every stream
    /// of the process, so two turns cannot produce the same made-up id.
    shared_ids: Option<&'static AtomicU64>,
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
    #[cfg(test)]
    pub(crate) fn new(key: Option<String>) -> Self {
        Self {
            key,
            ..Self::default()
        }
    }

    /// Like `new`, with made-up call ids numbered from `ids`.
    pub(crate) fn with_shared_ids(key: Option<String>, ids: &'static AtomicU64) -> Self {
        Self {
            key,
            shared_ids: Some(ids),
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
        let chunk: Chunk = serde_json::from_str(data).map_err(|e| malformed("SSE data", &e))?;
        if chunk.object.as_deref() == Some("error")
            || chunk.error.as_ref().is_some_and(|e| !e.is_null())
        {
            let v: Value = serde_json::from_str(data).unwrap_or(Value::Null);
            return Err(stream_error(&v, self.key.as_deref()));
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
            push_text(delta.content, out);
            for frag in delta.tool_calls.unwrap_or_default() {
                self.add_fragment(frag);
            }
        }
        if let Some(reason) = choice.finish_reason.filter(|r| !r.is_empty())
            && self.finish.is_none()
        {
            if reason == "error" {
                self.partials.clear();
                self.finish = Some(reason);
                return Err(AiError::Provider(
                    "the model stopped with an error".to_owned(),
                ));
            }
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

    /// Routes a fragment to a slot. A non-empty id not seen before opens a new
    /// slot, whatever its `index`. A fragment without an id goes to the slot
    /// that last used its `index`, otherwise to the last slot.
    fn add_fragment(&mut self, frag: ToolCallChunk) {
        let id = frag.id.filter(|i| !i.is_empty());
        let slot = match &id {
            Some(id) => self.partials.iter().position(|p| {
                p.id.as_deref() == Some(id.as_str())
                    && (p.index.is_none() || frag.index.is_none() || p.index == frag.index)
            }),
            None => frag
                .index
                .and_then(|i| self.by_index.get(&i).copied())
                .or(self.last_slot),
        };
        // The same id on another index is a second call, not more of the
        // first; it gets an id of its own when it is flushed.
        let reused = id.as_ref().is_some_and(|id| {
            slot.is_none()
                && self
                    .partials
                    .iter()
                    .any(|p| p.id.as_deref() == Some(id.as_str()))
        });
        let id = if reused { None } else { id };
        let slot = slot.unwrap_or_else(|| {
            self.partials.push(Partial::default());
            self.partials.len() - 1
        });
        self.last_slot = Some(slot);
        if let Some(i) = frag.index {
            self.by_index.insert(i, slot);
        }
        let p = &mut self.partials[slot];
        if let Some(id) = id {
            p.id = Some(id);
        }
        if p.index.is_none() {
            p.index = frag.index;
        }
        if let Some(f) = frag.function {
            if let Some(name) = f.name.filter(|n| !n.is_empty()) {
                p.name = Some(name);
            }
            match f.arguments {
                Some(Value::String(s)) => p.args.push_str(&s),
                // An object is the whole argument list; send it compact.
                Some(v @ Value::Object(_)) => p.args.push_str(&v.to_string()),
                _ => {}
            }
        }
    }

    /// Emits the collected tool calls in the order they were opened.
    fn flush(&mut self, out: &mut VecDeque<Result<ChatEvent, AiError>>) -> Result<(), AiError> {
        let calls = std::mem::take(&mut self.partials);
        self.by_index.clear();
        self.last_slot = None;
        // Half-written arguments (a cut-off response, or a server that sent
        // garbage) never reach the tool layer, whatever the finish reason.
        if calls.iter().any(|p| !p.args_usable()) {
            let cut = matches!(
                self.finish.as_deref(),
                None | Some("length" | "model_length")
            );
            return Err(AiError::Protocol(
                if cut {
                    "tool call cut off by the token limit"
                } else {
                    "tool call arguments are not a JSON object"
                }
                .to_owned(),
            ));
        }
        if calls.iter().any(|p| p.name.is_none()) {
            return Err(AiError::Protocol("tool call without a name".to_owned()));
        }
        for p in calls {
            let id = p.id.unwrap_or_else(|| self.made_up_id());
            let name = p.name.unwrap_or_default();
            self.emitted_calls += 1;
            out.push_back(Ok(ChatEvent::ToolCall(ToolCall::new(id, name, p.args))));
        }
        Ok(())
    }

    fn made_up_id(&mut self) -> String {
        let n = if let Some(shared) = self.shared_ids {
            shared.fetch_add(1, Ordering::Relaxed)
        } else {
            self.generated += 1;
            self.generated - 1
        };
        format!("call{n}")
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

/// Pushes the text of `delta.content`: a string, or the `text` chunks of an
/// array. Other chunk types (`thinking`, ...) are skipped.
fn push_text(content: Option<Value>, out: &mut VecDeque<Result<ChatEvent, AiError>>) {
    let mut push = |t: &str| {
        if !t.is_empty() {
            out.push_back(Ok(ChatEvent::TextDelta(t.to_owned())));
        }
    };
    match content {
        Some(Value::String(s)) => push(&s),
        Some(Value::Array(items)) => {
            for item in &items {
                if item.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(t) = item.get("text").and_then(Value::as_str)
                {
                    push(t);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_wraps)]
mod tests {
    use serde_json::json;
    use sqler_ai_api::{Image, Message, ToolDefinition, ToolResult};

    use super::*;

    fn body(req: &ChatRequest) -> Value {
        serde_json::from_slice(&request_body(req).unwrap()).unwrap()
    }

    // ---- ids ----

    #[test]
    fn matching_ids_pass_through() {
        for id in ["abc123XYZ", "000000000", "ZZZZZZZZZ", "call12345"] {
            assert_eq!(normalize_call_id(id), id);
        }
    }

    #[test]
    fn other_ids_become_nine_alphanumerics_deterministically() {
        for id in [
            "",
            "call_0",
            "call_1",
            "abcdefgh",
            "abcdefghij",
            "abc-12345",
            "toolu_01A09q90qw90lq917835lq9",
            "é\u{1f980}",
            "call_abc",
        ] {
            let a = normalize_call_id(id);
            assert!(matches_rule(&a), "{id:?} -> {a}");
            assert_eq!(a, normalize_call_id(id));
        }
        assert_ne!(normalize_call_id("call_0"), normalize_call_id("call_1"));
        assert_ne!(normalize_call_id("a"), normalize_call_id("b"));
        // Pinned: a change in the hash would re-pair stored conversations.
        assert_eq!(normalize_call_id("call_0"), "ckTj2MTct");
    }

    // ---- request ----

    #[test]
    fn minimal_request_has_no_stream_options() {
        let v = body(&ChatRequest::new("m", vec![Message::user("hi")]));
        assert_eq!(
            v,
            json!({"model":"m","messages":[{"role":"user","content":"hi"}],"stream":true})
        );
    }

    #[test]
    fn output_cap_is_max_tokens() {
        let req = ChatRequest::new("m", vec![Message::user("hi")]).with_max_output_tokens(64);
        let v = body(&req);
        assert_eq!(v["max_tokens"], 64);
        assert!(v.get("max_completion_tokens").is_none());
        assert!(v.get("stream_options").is_none());
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
            body(&req),
            json!({
                "model":"m",
                "messages":[
                    {"role":"system","content":"sys"},
                    {"role":"user","content":[
                        {"type":"text","text":"look "},
                        {"type":"image_url","image_url":"data:image/png;base64,AQID"},
                        {"type":"text","text":"here"}
                    ]}
                ],
                "tools":[{"type":"function","function":{"name":"t","description":"d","parameters":{"type":"object"}}}],
                "stream":true,
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
                        ToolCall::new("call_a", "first", "{\"x\":1}"),
                        ToolCall::new("abc123XYZ", "second", ""),
                    ],
                ),
                Message::tool_result(result),
            ],
        )
    }

    #[test]
    fn assistant_tool_calls_have_empty_content_and_normalised_ids() {
        let v = body(&tool_turn(ToolResult::ok("call_a", "42")));
        let id = normalize_call_id("call_a");
        assert_eq!(
            v["messages"][1],
            json!({"role":"assistant","content":"","tool_calls":[
                {"id":id,"type":"function","function":{"name":"first","arguments":"{\"x\":1}"}},
                {"id":"abc123XYZ","type":"function","function":{"name":"second","arguments":"{}"}}
            ]})
        );
        // The result carries the same normalised id and the call's name.
        assert_eq!(
            v["messages"][2],
            json!({"role":"tool","content":"42","tool_call_id":id,"name":"first"})
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
        assert_eq!(body(&req)["messages"][0]["content"], "let me check");
    }

    #[test]
    fn tool_error_is_prefixed() {
        let v = body(&tool_turn(ToolResult::error("abc123XYZ", "boom")));
        assert_eq!(
            v["messages"][2],
            json!({"role":"tool","content":"error: boom","tool_call_id":"abc123XYZ","name":"second"})
        );
    }

    #[test]
    fn tool_name_is_left_out_without_a_matching_call() {
        let req = ChatRequest::new(
            "m",
            vec![
                Message::tool_result(ToolResult::ok("abc123XYZ", "1")),
                Message::tool_result(ToolResult::ok("b", "2")),
            ],
        );
        let v = body(&req);
        assert_eq!(v["messages"].as_array().unwrap().len(), 2);
        assert!(v["messages"][0].get("name").is_none());
        assert_eq!(v["messages"][1]["tool_call_id"], normalize_call_id("b"));
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
        let mut m = ChunkMapper::new(Some("mst-secret".into()));
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

    fn cut_off() -> Result<ChatEvent, AiError> {
        Err(AiError::Protocol(
            "tool call cut off by the token limit".into(),
        ))
    }

    #[test]
    fn mapper_debug_hides_the_key() {
        let m = ChunkMapper::new(Some("mst-secret".to_owned()));
        let d = format!("{m:?}");
        assert!(!d.contains("mst-secret"));
        assert!(d.contains("***"));
    }

    #[test]
    fn text_usage_and_done() {
        let got = run(&[
            r#"{"id":"1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":" there"},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"total_tokens":11,"completion_tokens":2}}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                Ok(ChatEvent::TextDelta("Hi".into())),
                Ok(ChatEvent::TextDelta(" there".into())),
                Ok(ChatEvent::Usage(Usage::new(9, 2))),
                done(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn array_content_keeps_text_and_skips_thinking() {
        let got = run(&[
            r#"{"choices":[{"delta":{"content":[{"type":"thinking","thinking":[{"type":"text","text":"hmm"}]},{"type":"text","text":"Answer"},{"type":"reference","reference_ids":[1]},{"type":"text","text":"!"}]}}]}"#,
            r#"{"choices":[{"delta":{"content":[{"type":"thinking","thinking":[]}]},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                Ok(ChatEvent::TextDelta("Answer".into())),
                Ok(ChatEvent::TextDelta("!".into())),
                done(StopReason::EndTurn)
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
    fn a_whole_call_with_string_arguments() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"D681PevKs","function":{"name":"f","arguments":"{\"a\": 1}"},"index":0}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":3}}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                tc("D681PevKs", "f", "{\"a\": 1}"),
                Ok(ChatEvent::Usage(Usage::new(5, 3))),
                done(StopReason::ToolUse)
            ]
        );
    }

    #[test]
    fn object_arguments_are_serialised_compactly() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"x","function":{"name":"f","arguments":{"city": "Paris", "n": [1, 2]}}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                tc("x", "f", r#"{"city":"Paris","n":[1,2]}"#),
                done(StopReason::ToolUse)
            ]
        );
    }

    #[test]
    fn parallel_calls_with_repeated_index_zero_and_distinct_ids() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"aaaaaaaaa","index":0,"function":{"name":"a","arguments":"{\"x\":1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"bbbbbbbbb","index":0,"function":{"name":"b","arguments":"{\"y\":2}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                tc("aaaaaaaaa", "a", "{\"x\":1}"),
                tc("bbbbbbbbb", "b", "{\"y\":2}"),
                done(StopReason::ToolUse)
            ]
        );
    }

    #[test]
    fn fragments_without_an_id_follow_their_index_or_the_last_slot() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"p","index":0,"function":{"name":"one","arguments":"{"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"q","index":1,"function":{"name":"two","arguments":"{"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":" "}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"q","function":{"arguments":""}}]}}]}"#,
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
    fn missing_id_becomes_call_n() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(got, vec![tc("call0", "f", "{}"), done(StopReason::ToolUse)]);
    }

    #[test]
    fn invalid_arguments_are_an_error_for_every_finish_reason() {
        for reason in ["stop", "tool_calls"] {
            let fin = format!(
                r#"{{"choices":[{{"delta":{{"tool_calls":[{{"id":"a","function":{{"name":"f","arguments":"{{\"x\":"}}}}]}},"finish_reason":"{reason}"}}]}}"#
            );
            let got = run(&[&fin, "[DONE]"]);
            assert!(
                matches!(got.last(), Some(Err(AiError::Protocol(_)))),
                "{got:?}"
            );
            assert!(!got.iter().any(|e| matches!(e, Ok(ChatEvent::ToolCall(_)))));
        }
        // A fragment after the first finish_reason is checked at [DONE].
        let got = run(&[
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"name":"f","arguments":"nope"}}]}}]}"#,
            "[DONE]",
        ]);
        assert!(
            matches!(got.last(), Some(Err(AiError::Protocol(_)))),
            "{got:?}"
        );
    }

    #[test]
    fn shared_counter_keeps_made_up_ids_apart_across_streams() {
        static IDS: AtomicU64 = AtomicU64::new(0);
        let one = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#;
        let mut seen = Vec::new();
        for _ in 0..2 {
            let mut m = ChunkMapper::with_shared_ids(None, &IDS);
            let mut q = VecDeque::new();
            m.map_event(&ev(one), &mut q).unwrap();
            if let Some(Ok(ChatEvent::ToolCall(c))) = q.pop_front() {
                seen.push(c.id);
            }
        }
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0], seen[1]);
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
        assert_eq!(stop_reason(Some("length"), true), MaxTokens);
        assert_eq!(stop_reason(Some("model_length"), false), MaxTokens);
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
    fn tool_call_cut_by_length_or_model_length_is_an_error() {
        for reason in ["length", "model_length"] {
            let finish = format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"{reason}"}}]}}"#);
            let got = run(&[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"q","arguments":"{\"sql\":\"SEL"}}]}}]}"#,
                &finish,
            ]);
            assert_eq!(got, vec![cut_off()], "{reason}");
        }
    }

    #[test]
    fn tool_call_without_a_finish_reason_at_done_is_an_error() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"q","arguments":"{\"a\""}}]}}]}"#,
            "[DONE]",
        ]);
        assert_eq!(got, vec![cut_off()]);
    }

    #[test]
    fn complete_tool_call_followed_by_length_is_delivered() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"q","arguments":"{\"a\":1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![tc("c", "q", "{\"a\":1}"), done(StopReason::MaxTokens)]
        );
    }

    #[test]
    fn finish_reason_error_is_a_provider_error() {
        let got = run(&[
            r#"{"choices":[{"delta":{"content":"part"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"error"}]}"#,
            "[DONE]",
        ]);
        assert_eq!(
            got,
            vec![
                Ok(ChatEvent::TextDelta("part".into())),
                Err(AiError::Provider("the model stopped with an error".into()))
            ]
        );
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
    fn usage_is_emitted_once_and_before_done() {
        let got = run(&[
            r#"{"choices":[{"delta":{"content":"a"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":6}}"#,
            "[DONE]",
        ]);
        let usages = got
            .iter()
            .filter(|e| matches!(e, Ok(ChatEvent::Usage(_))))
            .count();
        assert_eq!(usages, 1);
        assert_eq!(got[got.len() - 2], Ok(ChatEvent::Usage(Usage::new(4, 6))));
        assert_eq!(got.last().unwrap(), &done(StopReason::EndTurn));
    }

    #[test]
    fn error_payloads_and_error_events() {
        let got = run(&[
            r#"{"object":"error","message":"boom mst-secret","type":"server_error","code":"500"}"#,
        ]);
        assert_eq!(got, vec![Err(AiError::Provider("boom ***".into()))]);
        let got = run(&[r#"{"error":{"message":"inner mst-secret"}}"#]);
        assert_eq!(got, vec![Err(AiError::Provider("inner ***".into()))]);
        let got = run(&[r#"{"object":"error"}"#]);
        assert_eq!(got, vec![Err(AiError::Provider("stream error".into()))]);
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
    fn done_flushes_complete_calls_never_finished() {
        let got = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"f","arguments":"{}"}}]}}]}"#,
            "[DONE]",
        ]);
        assert_eq!(got, vec![tc("a", "f", "{}"), done(StopReason::ToolUse)]);
    }
}
