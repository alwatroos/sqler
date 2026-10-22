// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Integration tests against a local mock HTTP server. The fixtures in
//! `tests/fixtures` are hand-written from the example transcripts of the
//! Anthropic API documentation, not captured from the real API.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use serde_json::json;
use sqler_ai_anthropic::{API_VERSION, AnthropicProvider};
use sqler_ai_api::{
    AiError, AiProvider, ChatEvent, ChatRequest, Message, StopReason, ToolCall, ToolDefinition,
    ToolResult, Usage, collect,
};
use support::{Finish, MockServer, Reply, Route, dead_port_url};

const KEY: &str = "sk-ant-test-0123456789";
const TEXT: &str = include_str!("fixtures/text.sse");
const TOOL: &str = include_str!("fixtures/tool.sse");
const FINAL: &str = include_str!("fixtures/final.sse");
const ERROR_EVENT: &str = include_str!("fixtures/error_event.sse");
const MODELS_P1: &str = include_str!("fixtures/models_p1.json");
const MODELS_P2: &str = include_str!("fixtures/models_p2.json");
const ERROR_400: &str = include_str!("fixtures/error_400.json");
const ERROR_401: &str = include_str!("fixtures/error_401.json");
const ERROR_429: &str = include_str!("fixtures/error_429.json");
const ERROR_500: &str = include_str!("fixtures/error_500.json");
const ERROR_529: &str = include_str!("fixtures/error_529.json");

fn provider(server: &MockServer) -> AnthropicProvider {
    AnthropicProvider::new(KEY, Some(&server.url())).unwrap()
}

fn messages_route(reply: Reply) -> Vec<Route> {
    vec![Route::new("POST", "/v1/messages", reply)]
}

fn request() -> ChatRequest {
    ChatRequest::new("claude-sonnet-5-5", vec![Message::user("SECRET-PROMPT")])
}

async fn first_error(p: &AnthropicProvider) -> AiError {
    let mut s = p.stream(request());
    let e = s.next_event().await.unwrap().unwrap_err();
    assert!(s.next_event().await.is_none());
    e
}

#[tokio::test]
async fn text_stream_in_seven_byte_chunks() {
    let server = MockServer::start(messages_route(Reply::sse(TEXT, 7, Finish::Clean))).await;
    let p = provider(&server);
    let req = request()
        .with_system("be brief")
        .with_temperature(0.5)
        .with_max_output_tokens(50);
    let mut s = p.stream(req);
    let r = collect(s.as_mut()).await.unwrap();
    assert_eq!(r.text, "Hello! é🦀");
    assert_eq!(r.usage, Usage::new(25, 15));
    assert_eq!(r.stop, StopReason::EndTurn);
    assert_eq!(r.tool_calls, vec![]);
    assert!(s.next_event().await.is_none());

    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/v1/messages");
    assert_eq!(reqs[0].header("x-api-key"), Some(KEY));
    assert_eq!(reqs[0].header("anthropic-version"), Some(API_VERSION));
    assert_eq!(reqs[0].header("content-type"), Some("application/json"));
    assert_eq!(reqs[0].header("accept"), Some("text/event-stream"));
    assert_eq!(reqs[0].header("anthropic-beta"), None);
    assert_eq!(
        reqs[0].json(),
        json!({
            "model":"claude-sonnet-5-5","max_tokens":50,"system":"be brief","temperature":0.5,
            "stream":true,
            "messages":[{"role":"user","content":[{"type":"text","text":"SECRET-PROMPT"}]}]
        })
    );
}

#[tokio::test]
async fn max_tokens_defaults_to_4096() {
    let server = MockServer::start(messages_route(Reply::sse(TEXT, 100, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    collect(s.as_mut()).await.unwrap();
    assert_eq!(server.requests()[0].json()["max_tokens"], 4096);
}

#[tokio::test]
async fn tool_call_with_split_partial_json_then_tool_result() {
    // The tool-result request carries `tool_result`; the first matching route wins.
    let server = MockServer::start(vec![
        Route::new("POST", "/v1/messages", Reply::sse(FINAL, 40, Finish::Clean))
            .when_body_has("tool_result"),
        Route::new("POST", "/v1/messages", Reply::sse(TOOL, 7, Finish::Clean)),
    ])
    .await;
    let p = provider(&server);
    let tool = ToolDefinition::new(
        "get_weather",
        "Get the weather",
        json!({"type":"object","properties":{"location":{"type":"string"}}}),
    );
    let req = ChatRequest::new("claude-sonnet-5-5", vec![Message::user("weather in SF?")])
        .with_tools(vec![tool.clone()]);
    let mut s = p.stream(req);
    let r = collect(s.as_mut()).await.unwrap();
    assert_eq!(r.text, "Okay, let me check the weather.");
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.usage, Usage::new(472, 89));
    assert_eq!(r.tool_calls.len(), 1);
    let call = &r.tool_calls[0];
    assert_eq!(call.id, "toolu_01T1x1fJ34qAmk2tNTrN7Up6");
    assert_eq!(call.name, "get_weather");
    assert_eq!(
        call.parse_arguments().unwrap(),
        json!({"location":"San Francisco, CA","unit":"fahrenheit"})
    );
    drop(s);

    let history = vec![
        Message::user("weather in SF?"),
        r.into_message(),
        Message::tool_result(ToolResult::ok(
            "toolu_01T1x1fJ34qAmk2tNTrN7Up6",
            "15 degrees",
        )),
    ];
    let mut s = p.stream(ChatRequest::new("claude-sonnet-5-5", history).with_tools(vec![tool]));
    let r2 = collect(s.as_mut()).await.unwrap();
    assert_eq!(r2.text, "It is 15 degrees.");
    assert_eq!(r2.stop, StopReason::EndTurn);

    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[0].json()["tools"],
        json!([{"name":"get_weather","description":"Get the weather",
                "input_schema":{"type":"object","properties":{"location":{"type":"string"}}}}])
    );
    let msgs = reqs[1].json()["messages"].clone();
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(
        msgs[1]["content"][1],
        json!({"type":"tool_use","id":"toolu_01T1x1fJ34qAmk2tNTrN7Up6","name":"get_weather",
               "input":{"location":"San Francisco, CA","unit":"fahrenheit"}})
    );
    assert_eq!(
        msgs[2],
        json!({"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_01T1x1fJ34qAmk2tNTrN7Up6","content":"15 degrees"}
        ]})
    );
}

#[tokio::test]
async fn lists_models_across_two_pages() {
    let server = MockServer::start(vec![
        Route::new("GET", "/v1/models", Reply::json(200, MODELS_P2)).when_query_has("after_id="),
        Route::new("GET", "/v1/models", Reply::json(200, MODELS_P1)),
    ])
    .await;
    let p = provider(&server);
    let models = p.list_models().await.unwrap();
    let got: Vec<_> = models
        .iter()
        .map(|m| (m.id.as_str(), m.display_name.as_deref(), m.context_window))
        .collect();
    assert_eq!(
        got,
        vec![
            ("claude-opus-5-5", Some("Claude Opus 5.5"), Some(1_000_000)),
            (
                "claude-haiku-4-5-20251001",
                Some("Claude Haiku 4.5"),
                Some(200_000)
            ),
            // `max_input_tokens` is null: the table's lower bound.
            (
                "claude-sonnet-4-5-20250929",
                Some("Claude Sonnet 4.5"),
                Some(200_000)
            ),
            // Empty display name and no window.
            ("experimental-x", None, None),
        ]
    );
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].path, "/v1/models");
    assert_eq!(reqs[0].query.as_deref(), Some("limit=1000"));
    assert_eq!(
        reqs[1].query.as_deref(),
        Some("limit=1000&after_id=claude-haiku-4-5-20251001")
    );
    for r in &reqs {
        assert_eq!(r.header("x-api-key"), Some(KEY));
        assert_eq!(r.header("anthropic-version"), Some(API_VERSION));
    }
}

#[tokio::test]
async fn model_listing_stops_after_ten_pages() {
    // A server that always says `has_more` must not loop forever.
    let server = MockServer::start(vec![Route::new(
        "GET",
        "/v1/models",
        Reply::json(200, MODELS_P1),
    )])
    .await;
    let models = provider(&server).list_models().await.unwrap();
    assert_eq!(models.len(), 20);
    assert_eq!(server.requests().len(), 10);
}

#[tokio::test]
async fn models_failure_is_an_error() {
    let server = MockServer::start(vec![Route::new(
        "GET",
        "/v1/models",
        Reply::json(401, ERROR_401),
    )])
    .await;
    let e = provider(&server).list_models().await.unwrap_err();
    assert_eq!(e, AiError::Auth("HTTP 401".into()));
}

#[tokio::test]
async fn status_errors_are_the_first_item() {
    for (status, body, extra, want) in [
        (401, ERROR_401, None, AiError::Auth("HTTP 401".into())),
        (403, ERROR_401, None, AiError::Auth("HTTP 403".into())),
        (
            429,
            ERROR_429,
            Some(("Retry-After", "7")),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(7)),
            },
        ),
        (
            429,
            ERROR_429,
            None,
            AiError::RateLimited { retry_after: None },
        ),
        (
            529,
            ERROR_529,
            None,
            AiError::Overloaded { retry_after: None },
        ),
        (
            529,
            ERROR_529,
            Some(("Retry-After", "99999")),
            AiError::Overloaded {
                retry_after: Some(Duration::from_secs(3600)),
            },
        ),
        (
            400,
            ERROR_400,
            None,
            AiError::Provider("HTTP 400: max_tokens: Field required".into()),
        ),
        (
            500,
            ERROR_500,
            None,
            AiError::Provider("HTTP 500: Internal server error".into()),
        ),
        (
            502,
            "<html>bad gateway</html>",
            None,
            AiError::Provider("HTTP 502".into()),
        ),
    ] {
        let mut reply = Reply::json(status, body);
        if let Some((k, v)) = extra {
            reply = reply.with_header(k, v);
        }
        let server = MockServer::start(messages_route(reply)).await;
        let p = provider(&server);
        assert_eq!(first_error(&p).await, want, "{status}");
    }
}

#[tokio::test]
async fn error_event_after_a_text_delta() {
    let server = MockServer::start(messages_route(Reply::sse(ERROR_EVENT, 9, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::Usage(Usage::new(10, 1))
    );
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Partial".into())
    );
    assert_eq!(
        s.next_event().await.unwrap().unwrap_err(),
        AiError::Overloaded { retry_after: None }
    );
    assert!(s.next_event().await.is_none());
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn connection_refused_is_a_scrubbed_network_error() {
    let url = dead_port_url().await;
    let p = AnthropicProvider::new(KEY, Some(&url)).unwrap();
    let e = first_error(&p).await;
    assert_eq!(
        e,
        AiError::Network("cannot connect to the Anthropic API".into())
    );
    let shown = format!("{e} {e:?}");
    assert!(!shown.contains(&url) && !shown.contains("127.0.0.1"));
    assert!(!shown.contains("SECRET-PROMPT") && !shown.contains(KEY));
}

#[tokio::test]
async fn mid_stream_disconnect() {
    // Cut after the first text delta.
    let cut = TEXT.find("event: content_block_stop").unwrap();
    let server = MockServer::start(messages_route(Reply::sse(
        &TEXT[..cut],
        64,
        Finish::Disconnect,
    )))
    .await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert!(matches!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::Usage(_)
    ));
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Hello".into())
    );
    assert!(matches!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta(_)
    ));
    let e = s.next_event().await.unwrap().unwrap_err();
    assert_eq!(e, AiError::Network("response interrupted".into()));
    assert!(s.next_event().await.is_none());
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn clean_eof_without_message_stop_is_a_protocol_error() {
    let cut = TEXT.find("event: message_delta").unwrap();
    let server =
        MockServer::start(messages_route(Reply::sse(&TEXT[..cut], 64, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    let e = loop {
        if let Err(e) = s.next_event().await.unwrap() {
            break e;
        }
    };
    assert_eq!(
        e,
        AiError::Protocol("stream ended before message_stop".into())
    );
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn malformed_event_is_a_protocol_error() {
    let data = "event: message_start\ndata: {not json\n\n";
    let server = MockServer::start(messages_route(Reply::sse(data, 1000, Finish::Clean))).await;
    let p = provider(&server);
    let e = first_error(&p).await;
    assert!(matches!(e, AiError::Protocol(m) if m.starts_with("malformed SSE event: ")));
}

#[tokio::test]
async fn oversize_event_after_complete_events_delivers_them_first() {
    let first = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"a\"}}\n\n";
    let big = format!("data: {}", "x".repeat(9 * 1024 * 1024));
    let server = MockServer::start(messages_route(Reply::sse(
        &format!("{first}{big}"),
        10 * 1024 * 1024,
        Finish::Clean,
    )))
    .await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("a".into())
    );
    assert!(matches!(
        s.next_event().await.unwrap().unwrap_err(),
        AiError::Protocol(_)
    ));
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn invalid_requests_fail_before_any_request() {
    let server = MockServer::start(vec![]).await;
    let p = provider(&server);
    let bad_args = vec![Message::assistant_with_tools(
        None,
        vec![ToolCall::new("a", "t", "{broken")],
    )];
    let e = {
        let mut s = p.stream(ChatRequest::new("m", bad_args));
        let e = s.next_event().await.unwrap().unwrap_err();
        assert!(s.next_event().await.is_none());
        e
    };
    assert!(matches!(e, AiError::Invalid(_)));

    let json_mode = request().with_response_format(sqler_ai_api::ResponseFormat::Json);
    let mut s = p.stream(json_mode);
    assert_eq!(
        s.next_event().await.unwrap().unwrap_err(),
        AiError::Unsupported("JSON mode is not available for Anthropic".into())
    );
    assert!(s.next_event().await.is_none());
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn dropping_the_stream_closes_the_connection() {
    let cut = TEXT.find("event: content_block_stop").unwrap();
    let server =
        MockServer::start(messages_route(Reply::sse(&TEXT[..cut], 1000, Finish::Hold))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert!(matches!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::Usage(_)
    ));
    drop(s);
    tokio::time::timeout(Duration::from_secs(5), server.client_closed.notified())
        .await
        .expect("the server saw the connection close");
}

#[tokio::test]
async fn repolling_after_a_cancelled_error_body_read_ends_the_stream() {
    let mut reply = Reply::sse("{\"type\":\"error\"}", 4, Finish::Hold);
    reply.status = 500;
    let server = MockServer::start(messages_route(reply)).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    let first = tokio::time::timeout(Duration::from_millis(300), s.next_event()).await;
    assert!(first.is_err(), "the error body never completes");
    let second = tokio::time::timeout(Duration::from_millis(300), s.next_event())
        .await
        .expect("a re-poll must not hang or panic");
    assert!(second.is_none());
}

#[test]
fn constructor_rules() {
    for (key, endpoint) in [
        ("", None),
        ("   ", None),
        ("bad\nkey", None),
        (KEY, Some("http://example.com")),
        (KEY, Some("ftp://x")),
        (KEY, Some("https://user:secret@example.com")),
    ] {
        let e = AnthropicProvider::new(key, endpoint).unwrap_err();
        assert!(matches!(e, AiError::Invalid(_)), "{key:?} {endpoint:?}");
        assert!(!format!("{e}{e:?}").contains("secret"));
    }
    assert!(AnthropicProvider::new(KEY, Some("http://127.0.0.1:1")).is_ok());
}

#[tokio::test]
async fn silent_stream_times_out() {
    let server = MockServer::start(messages_route(Reply::sse("", 64, Finish::Hold))).await;
    let p = provider(&server)
        .with_read_timeout(Duration::from_millis(50))
        .unwrap();
    let mut s = p.stream(request());
    let first = tokio::time::timeout(Duration::from_secs(5), s.next_event())
        .await
        .expect("the read timeout must end the wait");
    assert_eq!(
        first.unwrap().unwrap_err(),
        AiError::Network("timed out".into())
    );
}
