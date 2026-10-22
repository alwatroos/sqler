// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Integration tests against a local mock HTTP server. The fixtures in
//! `tests/fixtures` are hand-written from the Mistral API reference (the
//! `OpenAPI` document), not captured from the real service.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use serde_json::json;
use sqler_ai_api::{
    AiError, AiProvider, Capabilities, ChatEvent, ChatRequest, Message, StopReason, ToolCall,
    ToolDefinition, ToolResult, Usage, collect,
};
use sqler_ai_mistral::MistralProvider;
use support::{Finish, MockServer, Reply, Route, dead_port_url};

const TEXT: &str = include_str!("fixtures/text.sse");
const TOOLS: &str = include_str!("fixtures/tools.sse");
const FINAL: &str = include_str!("fixtures/final.sse");
const CUTOFF: &str = include_str!("fixtures/cutoff.sse");
const ERROR_CHUNK: &str = include_str!("fixtures/error_chunk.sse");
const MODELS: &str = include_str!("fixtures/models.json");
const MODEL_CARD: &str = include_str!("fixtures/model_card.json");

const KEY: &str = "mst-TESTKEY-123";
const CHAT_PATH: &str = "/v1/chat/completions";

fn provider(server: &MockServer) -> MistralProvider {
    MistralProvider::new(KEY, Some(&server.url())).unwrap()
}

fn chat_route(reply: Reply) -> Vec<Route> {
    vec![Route::new("POST", CHAT_PATH, reply)]
}

fn request() -> ChatRequest {
    ChatRequest::new("mistral-small-latest", vec![Message::user("SECRET-PROMPT")])
}

async fn first_error(p: &MistralProvider) -> AiError {
    let mut s = p.stream(request());
    let e = s.next_event().await.unwrap().unwrap_err();
    assert!(s.next_event().await.is_none());
    e
}

/// The first `n` events of a transcript, with their blank lines.
fn head(transcript: &str, n: usize) -> String {
    transcript
        .split("\n\n")
        .take(n)
        .collect::<Vec<_>>()
        .join("\n\n")
        + "\n\n"
}

#[tokio::test]
async fn text_stream_in_seven_byte_chunks() {
    let server = MockServer::start(chat_route(Reply::sse(TEXT, 7, Finish::Clean))).await;
    let p = provider(&server);
    let req = request()
        .with_system("be brief")
        .with_temperature(0.2)
        .with_max_output_tokens(50);
    let mut s = p.stream(req);
    let r = collect(s.as_mut()).await.unwrap();
    // The `thinking` chunk of the transcript is dropped.
    assert_eq!(r.text, "Hello zażółć 日本");
    assert_eq!(r.usage, Usage::new(26, 9));
    assert_eq!(r.stop, StopReason::EndTurn);
    assert_eq!(r.tool_calls.len(), 0);
    assert!(s.next_event().await.is_none());
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, CHAT_PATH);
    assert_eq!(
        reqs[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(
        reqs[0].json(),
        json!({
            "model":"mistral-small-latest",
            "messages":[
                {"role":"system","content":"be brief"},
                {"role":"user","content":"SECRET-PROMPT"}
            ],
            "stream":true,
            "temperature":0.2,
            "max_tokens":50
        })
    );
    assert!(reqs[0].json().get("stream_options").is_none());
}

#[tokio::test]
async fn deltas_arrive_before_the_stream_ends() {
    let server = MockServer::start(chat_route(Reply::sse(TEXT, 64, Finish::Hold))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Hello".into())
    );
}

fn tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new("get_weather", "weather", json!({"type":"object"})),
        ToolDefinition::new("get_time", "time", json!({"type":"object"})),
    ]
}

#[tokio::test]
async fn tool_calls_then_tool_results() {
    // The first matching route wins: the tool-result request carries `tool_call_id`.
    let server = MockServer::start(vec![
        Route::new("POST", CHAT_PATH, Reply::sse(FINAL, 40, Finish::Clean))
            .when_body_has("tool_call_id"),
        Route::new("POST", CHAT_PATH, Reply::sse(TOOLS, 40, Finish::Clean)),
    ])
    .await;
    let p = provider(&server);
    let req = ChatRequest::new(
        "mistral-small-latest",
        vec![Message::user("weather and time in Tokyo?")],
    )
    .with_tools(tools());
    let mut s = p.stream(req);
    let r = collect(s.as_mut()).await.unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.usage, Usage::new(80, 31));
    // Two calls with the same `index` 0 and distinct ids; the second one
    // sent its arguments as a JSON object.
    assert_eq!(
        r.tool_calls,
        vec![
            ToolCall::new("D681PevKs", "get_weather", r#"{"city": "Tokyo"}"#),
            ToolCall::new("yZ3kQp0wB", "get_time", r#"{"tz":"JST"}"#),
        ]
    );
    drop(s);

    let history = vec![
        Message::user("weather and time in Tokyo?"),
        r.into_message(),
        Message::tool_result(ToolResult::ok("D681PevKs", "11 degrees")),
        Message::tool_result(ToolResult::error("yZ3kQp0wB", "no clock")),
    ];
    let mut s = p.stream(ChatRequest::new("mistral-small-latest", history).with_tools(tools()));
    let r2 = collect(s.as_mut()).await.unwrap();
    assert_eq!(r2.text, "It is 11 degrees.");
    assert_eq!(r2.stop, StopReason::EndTurn);

    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[0].json()["tools"][0],
        json!({"type":"function","function":{"name":"get_weather","description":"weather","parameters":{"type":"object"}}})
    );
    for r in &reqs {
        assert!(r.json().get("stream_options").is_none());
    }
    assert_eq!(
        reqs[1].json()["messages"],
        json!([
            {"role":"user","content":"weather and time in Tokyo?"},
            {"role":"assistant","content":"","tool_calls":[
                {"id":"D681PevKs","type":"function","function":{"name":"get_weather","arguments":"{\"city\": \"Tokyo\"}"}},
                {"id":"yZ3kQp0wB","type":"function","function":{"name":"get_time","arguments":"{\"tz\":\"JST\"}"}}
            ]},
            {"role":"tool","content":"11 degrees","tool_call_id":"D681PevKs","name":"get_weather"},
            {"role":"tool","content":"error: no clock","tool_call_id":"yZ3kQp0wB","name":"get_time"}
        ])
    );
}

#[tokio::test]
async fn call_ids_that_break_the_rule_are_normalised_in_pairs() {
    let server = MockServer::start(chat_route(Reply::sse(FINAL, 40, Finish::Clean))).await;
    let p = provider(&server);
    let history = vec![
        Message::user("q"),
        Message::assistant_with_tools(
            None,
            vec![
                ToolCall::new("call_0", "first", "{}"),
                ToolCall::new("call_1", "second", ""),
            ],
        ),
        Message::tool_result(ToolResult::ok("call_0", "a")),
        Message::tool_result(ToolResult::ok("call_1", "b")),
    ];
    let mut s = p.stream(ChatRequest::new("m", history));
    collect(s.as_mut()).await.unwrap();
    let v = server.requests()[0].json();
    let calls = v["messages"][1]["tool_calls"].as_array().unwrap();
    let id0 = calls[0]["id"].as_str().unwrap();
    let id1 = calls[1]["id"].as_str().unwrap();
    for id in [id0, id1] {
        assert_eq!(id.len(), 9, "{id}");
        assert!(id.bytes().all(|b| b.is_ascii_alphanumeric()), "{id}");
    }
    assert_ne!(id0, id1);
    assert_eq!(calls[1]["function"]["arguments"], "{}");
    assert_eq!(v["messages"][2]["tool_call_id"], id0);
    assert_eq!(v["messages"][2]["name"], "first");
    assert_eq!(v["messages"][3]["tool_call_id"], id1);
    assert_eq!(v["messages"][3]["name"], "second");
}

#[tokio::test]
async fn mid_stream_error_chunk() {
    let server = MockServer::start(chat_route(Reply::sse(ERROR_CHUNK, 30, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Par".into())
    );
    let e = s.next_event().await.unwrap().unwrap_err();
    assert_eq!(
        e,
        AiError::Provider("Service tier capacity exceeded for this model.".into())
    );
    assert!(s.next_event().await.is_none());
    assert!(s.next_event().await.is_none());
}

type ErrorCase = (u16, Option<(&'static str, &'static str)>, String, AiError);

#[tokio::test]
async fn http_errors_are_the_first_item() {
    let cases: Vec<ErrorCase> = vec![
        (
            401,
            None,
            format!(r#"{{"message":"Unauthorized {KEY}","request_id":"r1"}}"#),
            AiError::Auth("HTTP 401".into()),
        ),
        (
            403,
            None,
            r#"{"object":"error","message":"Forbidden","type":"forbidden","code":"403"}"#.into(),
            AiError::Auth("HTTP 403".into()),
        ),
        (
            422,
            None,
            r#"{"detail":[{"type":"missing","loc":["body","model"],"msg":"Field required","input":{}}]}"#
                .into(),
            AiError::Provider("HTTP 422: Field required".into()),
        ),
        (
            422,
            None,
            r#"{"object":"error","message":{"detail":[{"msg":"Tools are not supported by this model"}]},"type":"invalid_request_invalid_args","code":"3051"}"#
                .into(),
            AiError::Unsupported("Tools are not supported by this model".into()),
        ),
        (
            429,
            Some(("Retry-After", "3")),
            r#"{"message":"Rate limit exceeded"}"#.into(),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(3)),
            },
        ),
        (
            429,
            Some(("Retry-After", "18446744073709551615")),
            r#"{"message":"Rate limit exceeded"}"#.into(),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(3600)),
            },
        ),
        (
            429,
            Some(("Retry-After", "Wed, 21 Oct 2026 07:28:00 GMT")),
            "{}".into(),
            AiError::RateLimited { retry_after: None },
        ),
        (
            500,
            None,
            r#"{"object":"error","message":"Internal server error","type":"internal_server_error","code":"500"}"#
                .into(),
            AiError::Provider("HTTP 500: Internal server error".into()),
        ),
        (
            404,
            None,
            r#"{"object":"error","message":"Invalid model: x","type":"invalid_model","code":"1500"}"#
                .into(),
            AiError::Provider("HTTP 404: Invalid model: x".into()),
        ),
    ];
    for (status, extra, body, want) in cases {
        let mut reply = Reply::json(status, &body);
        if let Some((k, v)) = extra {
            reply = reply.with_header(k, v);
        }
        let server = MockServer::start(chat_route(reply)).await;
        let p = provider(&server);
        let e = first_error(&p).await;
        assert_eq!(e, want, "{status}");
        let shown = format!("{e} {e:?}");
        assert!(!shown.contains(KEY), "{status}");
        assert!(!shown.contains("SECRET-PROMPT"), "{status}");
    }
}

#[tokio::test]
async fn key_in_a_server_error_text_is_redacted() {
    let body = format!(r#"{{"message":"upstream rejected {KEY}"}}"#);
    let server = MockServer::start(chat_route(Reply::json(500, &body))).await;
    let e = first_error(&provider(&server)).await;
    assert_eq!(
        e,
        AiError::Provider("HTTP 500: upstream rejected ***".into())
    );
}

#[tokio::test]
async fn mid_stream_disconnect() {
    let server = MockServer::start(chat_route(Reply::sse(
        &head(TEXT, 3),
        64,
        Finish::Disconnect,
    )))
    .await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Hello".into())
    );
    let e = s.next_event().await.unwrap().unwrap_err();
    assert_eq!(e, AiError::Network("response interrupted".into()));
    assert!(s.next_event().await.is_none());
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn clean_eof_before_a_finish_reason_is_a_protocol_error() {
    // Only the role chunk (empty content) arrives, so the first item is the error.
    let server = MockServer::start(chat_route(Reply::sse(&head(TEXT, 1), 64, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap_err(),
        AiError::Protocol("stream ended before [DONE]".into())
    );
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn clean_eof_after_a_finish_reason_without_done_is_accepted() {
    let data = TEXT.split("data: [DONE]").next().unwrap().to_owned();
    let server = MockServer::start(chat_route(Reply::sse(&data, 64, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    let r = collect(s.as_mut()).await.unwrap();
    assert_eq!(r.text, "Hello zażółć 日本");
    assert_eq!(r.usage, Usage::new(26, 9));
}

#[tokio::test]
async fn a_tool_call_cut_off_by_the_token_limit_is_a_protocol_error() {
    let server = MockServer::start(chat_route(Reply::sse(CUTOFF, 40, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request().with_tools(tools()));
    let mut seen = Vec::new();
    while let Some(item) = s.next_event().await {
        seen.push(item);
    }
    assert_eq!(
        seen,
        vec![Err(AiError::Protocol(
            "tool call cut off by the token limit".into()
        ))]
    );
}

#[tokio::test]
async fn connection_refused_is_a_scrubbed_network_error() {
    let url = dead_port_url().await;
    let p = MistralProvider::new(KEY, Some(&url)).unwrap();
    let e = first_error(&p).await;
    assert_eq!(e, AiError::Network("cannot connect to the server".into()));
    let shown = format!("{e} {e:?}");
    assert!(!shown.contains(&url) && !shown.contains("127.0.0.1"));
    assert!(!shown.contains(KEY));
    assert!(!shown.contains("SECRET-PROMPT"));
}

#[tokio::test]
async fn model_list_is_filtered_sorted_and_has_windows() {
    let server = MockServer::start(vec![Route::new(
        "GET",
        "/v1/models",
        Reply::json(200, MODELS),
    )])
    .await;
    let models = provider(&server).list_models().await.unwrap();
    let got: Vec<_> = models
        .iter()
        .map(|m| (m.id.as_str(), m.display_name.as_deref(), m.context_window))
        .collect();
    assert_eq!(
        got,
        vec![
            ("bare-model", None, None),
            ("ft:open-mistral-7b:abc", None, Some(32768)),
            // 99999999999 does not fit a u32.
            ("huge", None, None),
            // The name equals the id, so there is no display name.
            ("mistral-large-latest", None, Some(131_072)),
            (
                "mistral-small-latest",
                Some("mistral-small-2506"),
                Some(131_072)
            ),
        ]
    );
    let reqs = server.requests();
    assert_eq!(reqs[0].path, "/v1/models");
    assert_eq!(
        reqs[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
}

#[tokio::test]
async fn models_failure_is_an_error() {
    let server = MockServer::start(vec![Route::new(
        "GET",
        "/v1/models",
        Reply::json(500, r#"{"message":"down"}"#),
    )])
    .await;
    let e = provider(&server).list_models().await.unwrap_err();
    assert_eq!(e, AiError::Provider("HTTP 500: down".into()));
}

#[tokio::test]
async fn model_capabilities_from_the_card() {
    let server = MockServer::start(vec![
        Route::new(
            "GET",
            "/v1/models/mistral-small-latest",
            Reply::json(200, MODEL_CARD),
        ),
        Route::new(
            "GET",
            "/v1/models/plain",
            Reply::json(200, r#"{"id":"plain"}"#),
        ),
        Route::new(
            "GET",
            "/v1/models/nofn",
            Reply::json(
                200,
                r#"{"id":"nofn","capabilities":{"completion_chat":true},"max_context_length":4096}"#,
            ),
        ),
        Route::new(
            "GET",
            "/v1/models/missing",
            Reply::json(404, r#"{"object":"error","message":"Invalid model: missing"}"#),
        ),
    ])
    .await;
    let p = provider(&server);
    assert_eq!(
        p.model_capabilities("mistral-small-latest").await.unwrap(),
        Capabilities::new(true, true, true, Some(131_072))
    );
    // No `capabilities` object: tools on, vision off.
    assert_eq!(
        p.model_capabilities("plain").await.unwrap(),
        Capabilities::new(true, true, false, None)
    );
    // An object without the flags: both off.
    assert_eq!(
        p.model_capabilities("nofn").await.unwrap(),
        Capabilities::new(false, true, false, Some(4096))
    );
    assert_eq!(
        p.model_capabilities("missing").await.unwrap_err(),
        AiError::Provider("HTTP 404: Invalid model: missing".into())
    );
    let reqs = server.requests();
    assert_eq!(
        reqs[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
}

#[tokio::test]
async fn a_hostile_model_id_stays_one_path_segment() {
    let server = MockServer::start(vec![Route::new(
        "GET",
        "/v1/models/a%2F..%2Fb%3Fx",
        Reply::json(200, MODEL_CARD),
    )])
    .await;
    let p = provider(&server);
    p.model_capabilities("a/../b?x").await.unwrap();
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/v1/models/a%2F..%2Fb%3Fx");
    // Ids that are no segment at all are refused without a request.
    for bad in ["", ".", ".."] {
        let e = p.model_capabilities(bad).await.unwrap_err();
        assert!(matches!(e, AiError::Invalid(_)), "{bad:?}");
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let server = MockServer::start(vec![
        Route::new(
            "POST",
            CHAT_PATH,
            Reply::json(307, "{}").with_header("Location", "/elsewhere"),
        ),
        Route::new("POST", "/elsewhere", Reply::sse(FINAL, 40, Finish::Clean)),
    ])
    .await;
    let e = first_error(&provider(&server)).await;
    assert_eq!(e, AiError::Provider("HTTP 307".into()));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn dropping_the_stream_closes_the_connection() {
    let server =
        MockServer::start(chat_route(Reply::sse(&head(TEXT, 2), 1000, Finish::Hold))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Hello".into())
    );
    drop(s);
    tokio::time::timeout(Duration::from_secs(5), server.client_closed.notified())
        .await
        .expect("the server saw the connection close");
}

#[tokio::test]
async fn silent_stream_times_out() {
    let server = MockServer::start(chat_route(Reply::sse("", 64, Finish::Hold))).await;
    let p = provider(&server)
        .with_read_timeout(Duration::from_millis(200))
        .unwrap();
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap_err(),
        AiError::Network("timed out".into())
    );
    assert!(s.next_event().await.is_none());
}

#[test]
fn debug_of_the_provider_has_no_key() {
    let p = MistralProvider::new(KEY, None).unwrap();
    assert!(!format!("{p:?}").contains(KEY));
    assert!(!p.is_local());
}

#[test]
fn key_over_plain_http_to_a_remote_host_is_refused() {
    let e = MistralProvider::new(KEY, Some("http://example.com")).unwrap_err();
    assert!(matches!(e, AiError::Invalid(_)));
    assert!(!format!("{e}{e:?}").contains(KEY));
    assert!(!format!("{e}{e:?}").contains("example.com"));
}
