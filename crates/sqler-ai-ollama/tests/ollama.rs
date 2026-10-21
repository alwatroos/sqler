// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Integration tests against a local mock HTTP server. The fixtures in
//! `tests/fixtures` are hand-written from the Ollama API documentation, not
//! captured from a real server.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use serde_json::json;
use sqler_ai_api::{
    AiError, AiProvider, Capabilities, ChatEvent, ChatRequest, Message, StopReason, ToolCall,
    ToolDefinition, ToolResult, Usage, collect,
};
use sqler_ai_ollama::OllamaProvider;
use support::{Finish, MockServer, Reply, Route, dead_port_url};

const TAGS: &str = include_str!("fixtures/tags.json");
const SHOW_LLAMA: &str = include_str!("fixtures/show_llama.json");
const SHOW_PLAIN: &str = include_str!("fixtures/show_plain.json");
const SHOW_NOCAPS: &str = include_str!("fixtures/show_nocaps_tools.json");
const CHAT_TEXT: &str = include_str!("fixtures/chat_text.ndjson");
const CHAT_TOOL: &str = include_str!("fixtures/chat_tool.ndjson");
const CHAT_FINAL: &str = include_str!("fixtures/chat_final.ndjson");

fn provider(server: &MockServer) -> OllamaProvider {
    OllamaProvider::new(Some(&server.url())).unwrap()
}

fn chat_route(reply: Reply) -> Vec<Route> {
    vec![Route::new("POST", "/api/chat", reply)]
}

fn request() -> ChatRequest {
    ChatRequest::new("llama3.2", vec![Message::user("SECRET-PROMPT")])
}

async fn first_error(p: &OllamaProvider) -> AiError {
    let mut s = p.stream(request());
    s.next_event().await.unwrap().unwrap_err()
}

#[tokio::test]
async fn lists_models_with_context_windows() {
    let server = MockServer::start(vec![
        Route::new("GET", "/api/tags", Reply::json(200, TAGS)),
        Route::new("POST", "/api/show", Reply::json(200, SHOW_LLAMA))
            .when_body_has("\"llama3.2:latest\""),
        Route::new("POST", "/api/show", Reply::json(500, r#"{"error":"boom"}"#))
            .when_body_has("\"broken:1b\""),
        Route::new("POST", "/api/show", Reply::json(200, SHOW_PLAIN)).when_body_has("\"plain:7b\""),
    ])
    .await;
    let models = provider(&server).list_models().await.unwrap();
    let got: Vec<_> = models
        .iter()
        .map(|m| (m.id.as_str(), m.context_window))
        .collect();
    assert_eq!(
        got,
        vec![
            ("llama3.2:latest", Some(131_072)),
            ("broken:1b", None),
            ("plain:7b", None)
        ]
    );
    let reqs = server.requests();
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].path, "/api/tags");
    assert_eq!(reqs.len(), 4);
    assert_eq!(reqs[1].json(), json!({"model":"llama3.2:latest"}));
}

#[tokio::test]
async fn tags_failure_is_an_error() {
    let server = MockServer::start(vec![Route::new(
        "GET",
        "/api/tags",
        Reply::json(500, r#"{"error":"down"}"#),
    )])
    .await;
    let e = provider(&server).list_models().await.unwrap_err();
    assert_eq!(e, AiError::Provider("HTTP 500: down".into()));
}

#[tokio::test]
async fn model_capabilities_with_and_without_a_list() {
    let server = MockServer::start(vec![
        Route::new("POST", "/api/show", Reply::json(200, SHOW_LLAMA)).when_body_has("\"full\""),
        Route::new("POST", "/api/show", Reply::json(200, SHOW_NOCAPS)).when_body_has("\"nocaps\""),
        Route::new("POST", "/api/show", Reply::json(200, SHOW_PLAIN)).when_body_has("\"plain\""),
    ])
    .await;
    let p = provider(&server);
    assert_eq!(
        p.model_capabilities("full").await.unwrap(),
        Capabilities::new(true, true, true, Some(131_072))
    );
    assert_eq!(
        p.model_capabilities("nocaps").await.unwrap(),
        Capabilities::new(false, true, false, Some(8192))
    );
    assert_eq!(
        p.model_capabilities("plain").await.unwrap(),
        Capabilities::new(true, true, false, None)
    );
}

#[tokio::test]
async fn text_stream_in_five_byte_chunks() {
    let server = MockServer::start(chat_route(Reply::ndjson(CHAT_TEXT, 5, Finish::Clean))).await;
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
    assert_eq!(reqs[0].path, "/api/chat");
    assert_eq!(
        reqs[0].json(),
        json!({
            "model":"llama3.2",
            "messages":[
                {"role":"system","content":"be brief"},
                {"role":"user","content":"SECRET-PROMPT"}
            ],
            "options":{"temperature":0.2,"num_predict":50},
            "stream":true
        })
    );
}

#[tokio::test]
async fn tool_call_then_tool_result() {
    // The first matching route wins: the tool-result request carries `tool_name`.
    let server = MockServer::start(vec![
        Route::new(
            "POST",
            "/api/chat",
            Reply::ndjson(CHAT_FINAL, 40, Finish::Clean),
        )
        .when_body_has("tool_name"),
        Route::new(
            "POST",
            "/api/chat",
            Reply::ndjson(CHAT_TOOL, 40, Finish::Clean),
        ),
    ])
    .await;
    let p = provider(&server);
    let tool = ToolDefinition::new("get_weather", "weather", json!({"type":"object"}));
    let req = ChatRequest::new("llama3.2", vec![Message::user("weather in Tokyo?")])
        .with_tools(vec![tool.clone()]);
    let mut s = p.stream(req);
    let r = collect(s.as_mut()).await.unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.usage, Usage::new(169, 15));
    assert_eq!(
        r.tool_calls,
        vec![ToolCall::new(
            "call_0",
            "get_weather",
            r#"{"city":"Tokyo"}"#
        )]
    );
    drop(s);

    let history = vec![
        Message::user("weather in Tokyo?"),
        r.into_message(),
        Message::tool_result(ToolResult::ok("call_0", "11 degrees")),
    ];
    let mut s = p.stream(ChatRequest::new("llama3.2", history).with_tools(vec![tool]));
    let r2 = collect(s.as_mut()).await.unwrap();
    assert_eq!(r2.text, "It is 11 degrees.");
    assert_eq!(r2.stop, StopReason::EndTurn);

    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[0].json()["tools"],
        json!([{"type":"function","function":{"name":"get_weather","description":"weather","parameters":{"type":"object"}}}])
    );
    assert_eq!(
        reqs[1].json()["messages"],
        json!([
            {"role":"user","content":"weather in Tokyo?"},
            {"role":"assistant","content":"","tool_calls":[{"function":{"name":"get_weather","arguments":{"city":"Tokyo"}}}]},
            {"role":"tool","content":"11 degrees","tool_name":"get_weather"}
        ])
    );
}

#[tokio::test]
async fn status_errors_are_the_first_item() {
    for (status, extra, want) in [
        (
            404,
            None,
            AiError::Provider("HTTP 404: model 'x' not found".into()),
        ),
        (401, None, AiError::Auth("HTTP 401".into())),
        (
            429,
            Some(("Retry-After", "3")),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(3)),
            },
        ),
        (
            400,
            None,
            AiError::Unsupported(
                "registry.ollama.ai/library/gemma:2b does not support tools".into(),
            ),
        ),
    ] {
        let body = match status {
            404 => r#"{"error":"model 'x' not found"}"#,
            400 => r#"{"error":"registry.ollama.ai/library/gemma:2b does not support tools"}"#,
            _ => r#"{"error":"nope"}"#,
        };
        let mut reply = Reply::json(status, body);
        if let Some((k, v)) = extra {
            reply = reply.with_header(k, v);
        }
        let server = MockServer::start(chat_route(reply)).await;
        let p = provider(&server);
        let mut s = p.stream(request());
        assert_eq!(s.next_event().await.unwrap().unwrap_err(), want, "{status}");
        assert!(s.next_event().await.is_none());
    }
}

#[tokio::test]
async fn connection_refused_is_a_scrubbed_network_error() {
    let url = dead_port_url().await;
    let p = OllamaProvider::new(Some(&url)).unwrap();
    let e = first_error(&p).await;
    let AiError::Network(m) = &e else {
        panic!("expected Network, got {e:?}");
    };
    assert_eq!(m, "cannot connect to the Ollama server");
    let shown = format!("{e} {e:?}");
    assert!(!shown.contains(&url) && !shown.contains("127.0.0.1"));
    assert!(!shown.contains("SECRET-PROMPT"));
}

#[tokio::test]
async fn mid_stream_disconnect() {
    let partial = CHAT_TEXT.lines().take(2).collect::<Vec<_>>().join("\n") + "\n";
    let server =
        MockServer::start(chat_route(Reply::ndjson(&partial, 64, Finish::Disconnect))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("Hel".into())
    );
    assert_eq!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta("lo ".into())
    );
    let e = s.next_event().await.unwrap().unwrap_err();
    assert_eq!(e, AiError::Network("response interrupted".into()));
    assert!(s.next_event().await.is_none());
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn clean_eof_without_done_is_a_protocol_error() {
    let partial = CHAT_TEXT.lines().next().unwrap().to_owned() + "\n";
    let server = MockServer::start(chat_route(Reply::ndjson(&partial, 64, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert!(matches!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta(_)
    ));
    assert_eq!(
        s.next_event().await.unwrap().unwrap_err(),
        AiError::Protocol("stream ended before done".into())
    );
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn oversize_line_after_complete_lines_delivers_them_first() {
    let first = CHAT_TEXT.lines().next().unwrap().to_owned() + "\n";
    let big = "x".repeat(9 * 1024 * 1024);
    let server = MockServer::start(chat_route(Reply::ndjson(
        &format!("{first}{big}"),
        10 * 1024 * 1024,
        Finish::Clean,
    )))
    .await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert!(matches!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta(_)
    ));
    assert!(matches!(
        s.next_event().await.unwrap().unwrap_err(),
        AiError::Protocol(_)
    ));
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn malformed_line_is_a_protocol_error() {
    let data = format!(
        "{}\n{{not json\n{}\n",
        CHAT_TEXT.lines().next().unwrap(),
        CHAT_TEXT.lines().last().unwrap()
    );
    let server = MockServer::start(chat_route(Reply::ndjson(&data, 1000, Finish::Clean))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert!(matches!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta(_)
    ));
    let e = s.next_event().await.unwrap().unwrap_err();
    assert!(matches!(e, AiError::Protocol(m) if m.starts_with("malformed NDJSON line: ")));
    assert!(s.next_event().await.is_none());
}

#[tokio::test]
async fn error_line_is_a_provider_error() {
    let data = "{\"error\":\"model runner has unexpectedly stopped\"}\n";
    let server = MockServer::start(chat_route(Reply::ndjson(data, 1000, Finish::Clean))).await;
    let p = provider(&server);
    let e = first_error(&p).await;
    assert_eq!(
        e,
        AiError::Provider("model runner has unexpectedly stopped".into())
    );
}

#[tokio::test]
async fn invalid_tool_arguments_fail_before_any_request() {
    let server = MockServer::start(vec![]).await;
    let p = provider(&server);
    let history = vec![Message::assistant_with_tools(
        None,
        vec![ToolCall::new("a", "t", "{broken")],
    )];
    let e = first_error_for(&p, ChatRequest::new("m", history)).await;
    assert!(matches!(e, AiError::Invalid(_)));
    assert!(server.requests().is_empty());
}

async fn first_error_for(p: &OllamaProvider, req: ChatRequest) -> AiError {
    let mut s = p.stream(req);
    let e = s.next_event().await.unwrap().unwrap_err();
    assert!(s.next_event().await.is_none());
    e
}

#[tokio::test]
async fn dropping_the_stream_closes_the_connection() {
    let first = CHAT_TEXT.lines().next().unwrap().to_owned() + "\n";
    let server = MockServer::start(chat_route(Reply::ndjson(&first, 1000, Finish::Hold))).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    assert!(matches!(
        s.next_event().await.unwrap().unwrap(),
        ChatEvent::TextDelta(_)
    ));
    drop(s);
    tokio::time::timeout(Duration::from_secs(5), server.client_closed.notified())
        .await
        .expect("the server saw the connection close");
}

#[test]
fn endpoint_with_credentials_is_rejected() {
    for url in [
        "http://user:secret@127.0.0.1:11434",
        "https://user@example.com",
    ] {
        let e = OllamaProvider::new(Some(url)).unwrap_err();
        assert!(matches!(e, AiError::Invalid(_)), "{url}");
        assert!(!format!("{e:?}{e}").contains("secret"));
    }
}

#[tokio::test]
async fn repolling_after_a_cancelled_error_body_read_ends_the_stream() {
    let mut reply = Reply::ndjson("{\"error\":\"x\"}", 4, Finish::Hold);
    reply.status = 500;
    let server = MockServer::start(chat_route(reply)).await;
    let p = provider(&server);
    let mut s = p.stream(request());
    let first = tokio::time::timeout(Duration::from_millis(300), s.next_event()).await;
    assert!(first.is_err(), "the error body never completes");
    let second = tokio::time::timeout(Duration::from_millis(300), s.next_event())
        .await
        .expect("a re-poll must not hang or panic");
    assert!(second.is_none());
}
