// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Integration tests against a local mock HTTP server. The fixtures in
//! `tests/fixtures` are hand-written from the `OpenAI` API reference and the
//! compatible-server documentation, not captured from a real server.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use serde_json::json;
use sqler_ai_api::{
    AiError, AiProvider, ChatEvent, ChatRequest, Message, StopReason, ToolCall, ToolDefinition,
    ToolResult, Usage, collect,
};
use sqler_ai_openai::{Flavor, OpenAiProvider};
use support::{Finish, MockServer, Reply, Route, dead_port_url};

const CHAT_TEXT: &str = include_str!("fixtures/chat_text.sse");
const CHAT_TOOLS: &str = include_str!("fixtures/chat_tools_parallel.sse");
const CHAT_FINAL: &str = include_str!("fixtures/chat_final.sse");
const CHAT_ERROR: &str = include_str!("fixtures/chat_error.sse");
const MODELS_OPENAI: &str = include_str!("fixtures/models_openai.json");
const MODELS_VLLM: &str = include_str!("fixtures/models_vllm.json");

const KEY: &str = "sk-test-key-0123456789";
const CHAT_PATH: &str = "/v1/chat/completions";

fn base(server: &MockServer) -> String {
    format!("{}/v1", server.url())
}

fn provider(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::new(Some(&base(server)), Some(KEY)).unwrap()
}

fn chat_route(reply: Reply) -> Vec<Route> {
    vec![Route::new("POST", CHAT_PATH, reply)]
}

fn request() -> ChatRequest {
    ChatRequest::new("gpt-test", vec![Message::user("SECRET-PROMPT")])
}

async fn first_error(p: &OpenAiProvider) -> AiError {
    let mut s = p.stream(request());
    let e = s.next_event().await.unwrap().unwrap_err();
    assert!(s.next_event().await.is_none());
    e
}

#[tokio::test]
async fn text_stream_in_seven_byte_chunks() {
    let server = MockServer::start(chat_route(Reply::sse(CHAT_TEXT, 7, Finish::Clean))).await;
    let p = provider(&server);
    let req = request()
        .with_system("be brief")
        .with_temperature(0.2)
        .with_max_output_tokens(50);
    let mut s = p.stream(req);
    let r = collect(s.as_mut()).await.unwrap();
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
            "model":"gpt-test",
            "messages":[
                {"role":"system","content":"be brief"},
                {"role":"user","content":"SECRET-PROMPT"}
            ],
            "stream":true,
            "stream_options":{"include_usage":true},
            "temperature":0.2,
            "max_tokens":50
        })
    );
}

#[tokio::test]
async fn deltas_arrive_before_the_stream_ends() {
    let server = MockServer::start(chat_route(Reply::sse(CHAT_TEXT, 64, Finish::Hold))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Hello".into())
    );
}

#[tokio::test]
async fn parallel_tool_calls_then_tool_results() {
    // The first matching route wins: the tool-result request carries `tool_call_id`.
    let server = MockServer::start(vec![
        Route::new("POST", CHAT_PATH, Reply::sse(CHAT_FINAL, 40, Finish::Clean))
            .when_body_has("tool_call_id"),
        Route::new("POST", CHAT_PATH, Reply::sse(CHAT_TOOLS, 40, Finish::Clean)),
    ])
    .await;
    let p = provider(&server);
    let tools = vec![
        ToolDefinition::new("get_weather", "weather", json!({"type":"object"})),
        ToolDefinition::new("get_time", "time", json!({"type":"object"})),
    ];
    let req = ChatRequest::new(
        "gpt-test",
        vec![Message::user("weather and time in Tokyo?")],
    )
    .with_tools(tools.clone());
    let mut s = p.stream(req);
    let r = collect(s.as_mut()).await.unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.usage, Usage::new(80, 31));
    assert_eq!(
        r.tool_calls,
        vec![
            ToolCall::new("call_abc", "get_weather", r#"{"city": "Tokyo"}"#),
            ToolCall::new("call_def", "get_time", r#"{"tz":"JST"}"#),
        ]
    );
    drop(s);

    let history = vec![
        Message::user("weather and time in Tokyo?"),
        r.into_message(),
        Message::tool_result(ToolResult::ok("call_abc", "11 degrees")),
        Message::tool_result(ToolResult::error("call_def", "no clock")),
    ];
    let mut s = p.stream(ChatRequest::new("gpt-test", history).with_tools(tools));
    let r2 = collect(s.as_mut()).await.unwrap();
    assert_eq!(r2.text, "It is 11 degrees.");
    assert_eq!(r2.stop, StopReason::EndTurn);

    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[0].json()["tools"][0],
        json!({"type":"function","function":{"name":"get_weather","description":"weather","parameters":{"type":"object"}}})
    );
    assert_eq!(
        reqs[1].json()["messages"],
        json!([
            {"role":"user","content":"weather and time in Tokyo?"},
            {"role":"assistant","content":null,"tool_calls":[
                {"id":"call_abc","type":"function","function":{"name":"get_weather","arguments":"{\"city\": \"Tokyo\"}"}},
                {"id":"call_def","type":"function","function":{"name":"get_time","arguments":"{\"tz\":\"JST\"}"}}
            ]},
            {"role":"tool","content":"11 degrees","tool_call_id":"call_abc"},
            {"role":"tool","content":"error: no clock","tool_call_id":"call_def"}
        ])
    );
}

#[tokio::test]
async fn mid_stream_error_event() {
    let server = MockServer::start(chat_route(Reply::sse(CHAT_ERROR, 30, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Par".into())
    );
    let e = s.next_event().await.unwrap().unwrap_err();
    assert_eq!(
        e,
        AiError::Provider(
            "The server had an error while processing your request. Sorry about that!".into()
        )
    );
    assert!(s.next_event().await.is_none());
    assert!(s.next_event().await.is_none());
}

type ErrorCase = (u16, Option<(&'static str, &'static str)>, String, AiError);

#[tokio::test]
async fn http_errors_are_the_first_item() {
    let masked = format!(
        r#"{{"error":{{"message":"Incorrect API key provided: {KEY}. You can find your API key at ...","type":"invalid_request_error","code":"invalid_api_key"}}}}"#
    );
    let cases: Vec<ErrorCase> = vec![
        (401, None, masked, AiError::Auth("HTTP 401".into())),
        (
            429,
            Some(("Retry-After", "3")),
            r#"{"error":{"message":"Rate limit reached","code":"rate_limit_exceeded"}}"#.into(),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(3)),
            },
        ),
        (
            429,
            None,
            r#"{"error":{"message":"You exceeded your current quota","code":"insufficient_quota"}}"#
                .into(),
            AiError::Provider("HTTP 429: You exceeded your current quota".into()),
        ),
        (
            400,
            None,
            r#"{"error":{"message":"Unsupported parameter: 'temperature'","code":"unsupported_parameter"}}"#
                .into(),
            AiError::Unsupported("Unsupported parameter: 'temperature'".into()),
        ),
        (
            404,
            None,
            r#"{"error":{"message":"The model `x` does not exist","code":"model_not_found"}}"#
                .into(),
            AiError::Provider("HTTP 404: The model `x` does not exist".into()),
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
    let body = format!(r#"{{"error":{{"message":"upstream rejected {KEY}"}}}}"#);
    let server = MockServer::start(chat_route(Reply::json(500, &body))).await;
    let e = first_error(&provider(&server)).await;
    assert_eq!(
        e,
        AiError::Provider("HTTP 500: upstream rejected ***".into())
    );
}

#[tokio::test]
async fn mid_stream_disconnect() {
    let partial = CHAT_TEXT
        .split("\n\n")
        .take(3)
        .collect::<Vec<_>>()
        .join("\n\n")
        + "\n\n";
    let server = MockServer::start(chat_route(Reply::sse(&partial, 64, Finish::Disconnect))).await;
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
    let partial = CHAT_TEXT.split("\n\n").next().unwrap().to_owned() + "\n\n";
    let server = MockServer::start(chat_route(Reply::sse(&partial, 64, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    // Drain the stream: whatever was delivered first, the last item is the error.
    let mut items = Vec::new();
    while let Some(item) = s.next_event().await {
        items.push(item);
    }
    assert_eq!(
        items.pop().unwrap().unwrap_err(),
        AiError::Protocol("stream ended before [DONE]".into())
    );
    assert!(items.iter().all(Result::is_ok));
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn clean_eof_after_a_finish_reason_without_done_is_accepted() {
    let data = CHAT_TEXT.split("data: [DONE]").next().unwrap().to_owned();
    let server = MockServer::start(chat_route(Reply::sse(&data, 64, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    let r = collect(s.as_mut()).await.unwrap();
    assert_eq!(r.text, "Hello zażółć 日本");
    assert_eq!(r.usage, Usage::new(26, 9));
}

#[tokio::test]
async fn connection_refused_is_a_scrubbed_network_error() {
    let url = dead_port_url().await;
    let p = OpenAiProvider::new(Some(&format!("{url}/v1")), Some(KEY)).unwrap();
    let e = first_error(&p).await;
    assert_eq!(e, AiError::Network("cannot connect to the server".into()));
    let shown = format!("{e} {e:?}");
    assert!(!shown.contains(&url) && !shown.contains("127.0.0.1"));
    assert!(!shown.contains(KEY));
    assert!(!shown.contains("SECRET-PROMPT"));
}

#[tokio::test]
async fn compatible_server_without_a_key() {
    let server = MockServer::start(vec![
        Route::new("GET", "/v1/models", Reply::json(200, MODELS_VLLM)),
        Route::new("POST", CHAT_PATH, Reply::sse(CHAT_FINAL, 40, Finish::Clean)),
    ])
    .await;
    let p = OpenAiProvider::new(Some(&base(&server)), None).unwrap();
    assert_eq!(p.flavor(), Flavor::Compatible);
    assert!(p.is_local());
    let models = p.list_models().await.unwrap();
    let got: Vec<_> = models
        .iter()
        .map(|m| (m.id.as_str(), m.context_window))
        .collect();
    assert_eq!(
        got,
        vec![
            ("../models/qwen.gguf", Some(32768)),
            ("huge", None),
            ("lmstudio-community/other", None),
            ("meta-llama/Llama-3.1-8B-Instruct", Some(131_072)),
        ]
    );
    let mut s = p.stream(request().with_max_output_tokens(10));
    let r = collect(s.as_mut()).await.unwrap();
    assert_eq!(r.text, "It is 11 degrees.");
    // No usage chunk in this transcript.
    assert_eq!(r.usage, Usage::default());
    let reqs = server.requests();
    assert_eq!(reqs[0].path, "/v1/models");
    assert_eq!(reqs[0].header("authorization"), None);
    assert_eq!(reqs[1].header("authorization"), None);
    assert_eq!(reqs[1].json()["max_tokens"], 10);
    assert!(reqs[1].json().get("max_completion_tokens").is_none());
}

#[tokio::test]
async fn openai_model_list_is_sorted_with_unknown_windows() {
    let server = MockServer::start(vec![Route::new(
        "GET",
        "/v1/models",
        Reply::json(200, MODELS_OPENAI),
    )])
    .await;
    let models = provider(&server).list_models().await.unwrap();
    let got: Vec<_> = models
        .iter()
        .map(|m| (m.id.as_str(), m.context_window))
        .collect();
    assert_eq!(
        got,
        vec![
            ("gpt-4o", None),
            ("gpt-4o-mini", None),
            ("text-embedding-3-small", None)
        ]
    );
    let reqs = server.requests();
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
        Reply::json(500, r#"{"error":{"message":"down"}}"#),
    )])
    .await;
    let e = provider(&server).list_models().await.unwrap_err();
    assert_eq!(e, AiError::Provider("HTTP 500: down".into()));
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let server = MockServer::start(vec![
        Route::new(
            "POST",
            CHAT_PATH,
            Reply::json(307, "{}").with_header("Location", "/elsewhere"),
        ),
        Route::new(
            "POST",
            "/elsewhere",
            Reply::sse(CHAT_FINAL, 40, Finish::Clean),
        ),
    ])
    .await;
    let e = first_error(&provider(&server)).await;
    assert_eq!(e, AiError::Provider("HTTP 307".into()));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn dropping_the_stream_closes_the_connection() {
    let first = CHAT_TEXT
        .split("\n\n")
        .take(2)
        .collect::<Vec<_>>()
        .join("\n\n")
        + "\n\n";
    let server = MockServer::start(chat_route(Reply::sse(&first, 1000, Finish::Hold))).await;
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

#[test]
fn debug_of_the_provider_has_no_key() {
    let p = OpenAiProvider::new(None, Some(KEY)).unwrap();
    assert!(!format!("{p:?}").contains(KEY));
}

#[test]
fn key_over_plain_http_to_a_remote_host_is_refused() {
    let e = OpenAiProvider::new(Some("http://example.com/v1"), Some(KEY)).unwrap_err();
    assert!(matches!(e, AiError::Invalid(_)));
    assert!(!format!("{e}{e:?}").contains(KEY));
    assert!(!format!("{e}{e:?}").contains("example.com"));
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
}
