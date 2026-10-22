// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The API key must never reach an error text, a `Debug` print, a `log`
//! record or a `tracing` event or span, whatever the failure.

#![allow(clippy::unwrap_used)]

mod support;

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once};

use serde_json::json;
use sqler_ai_api::{AiError, AiProvider, ChatRequest, Message};
use sqler_ai_mistral::MistralProvider;
use support::{Finish, MockServer, Reply, Route, dead_port_url};

const KEY: &str = "mst-TESTKEY-123";

static CAPTURED: Mutex<String> = Mutex::new(String::new());

fn capture(line: &str) {
    let mut c = CAPTURED.lock().unwrap();
    c.push_str(line);
    c.push('\n');
}

fn captured() -> String {
    CAPTURED.lock().unwrap().clone()
}

struct LogSink;

impl log::Log for LogSink {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, r: &log::Record<'_>) {
        capture(&format!("log {} {} {}", r.level(), r.target(), r.args()));
    }

    fn flush(&self) {}
}

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        write!(self.0, " {}={value:?}", field.name()).unwrap();
    }
}

struct Sink {
    next_id: AtomicU64,
}

impl tracing::Subscriber for Sink {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut f = Fields(String::new());
        attrs.record(&mut f);
        capture(&format!("span {}{}", attrs.metadata().name(), f.0));
        tracing::span::Id::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        let mut f = Fields(String::new());
        values.record(&mut f);
        capture(&format!("span-record{}", f.0));
    }

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut f = Fields(String::new());
        event.record(&mut f);
        capture(&format!(
            "event {} {}{}",
            event.metadata().level(),
            event.metadata().target(),
            f.0
        ));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

fn install_capture() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        log::set_logger(&LogSink).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
        tracing::subscriber::set_global_default(Sink {
            next_id: AtomicU64::new(1),
        })
        .unwrap();
    });
}

fn request() -> ChatRequest {
    ChatRequest::new("mistral-small-latest", vec![Message::user("SECRET-PROMPT")])
}

fn assert_clean(what: &str, e: &AiError) {
    for shown in [e.to_string(), format!("{e:?}")] {
        assert!(!shown.contains(KEY), "{what}: key in {shown}");
        assert!(!shown.contains("TESTKEY"), "{what}: key piece in {shown}");
        assert!(
            !shown.contains("SECRET-PROMPT"),
            "{what}: prompt in {shown}"
        );
    }
}

async fn stream_errors(p: &MistralProvider) -> Vec<AiError> {
    let mut s = p.stream(request());
    let mut errors = Vec::new();
    while let Some(item) = s.next_event().await {
        if let Err(e) = item {
            errors.push(e);
        }
    }
    errors
}

/// Replies that carry the key back in some form.
fn echoing_replies() -> Vec<(&'static str, Reply)> {
    let in_key = format!("bad credential {KEY}");
    // The key straddles character 200 of the text.
    let long = format!("{}{KEY} tail", "x".repeat(195));
    let chunk = |data: &str| format!("data: {data}\n\n");
    vec![
        (
            "401",
            Reply::json(401, &json!({"message": in_key}).to_string()),
        ),
        (
            "403",
            Reply::json(403, &json!({"message": in_key}).to_string()),
        ),
        (
            "500 message",
            Reply::json(
                500,
                &json!({"object":"error","message":in_key,"type":"t","code":"1"}).to_string(),
            ),
        ),
        (
            "500 object message",
            Reply::json(
                500,
                &json!({"message":{"detail":[{"msg":in_key}]}}).to_string(),
            ),
        ),
        (
            "500 error.message",
            Reply::json(500, &json!({"error":{"message":in_key}}).to_string()),
        ),
        (
            "422 detail",
            Reply::json(422, &json!({"detail":[{"msg":in_key}]}).to_string()),
        ),
        (
            "400 cut",
            Reply::json(400, &json!({"message": long}).to_string()),
        ),
        (
            "429",
            Reply::json(429, &json!({"message": in_key}).to_string())
                .with_header("Retry-After", "2"),
        ),
        (
            "error chunk",
            Reply::sse(
                &chunk(&json!({"object":"error","message":in_key,"code":"1"}).to_string()),
                16,
                Finish::Clean,
            ),
        ),
        (
            "error member",
            Reply::sse(
                &chunk(&json!({"error":{"message":long}}).to_string()),
                16,
                Finish::Clean,
            ),
        ),
        (
            "malformed chunk",
            Reply::sse(&chunk(&format!("{{\"choices\": \"{KEY}\"}}")), 16, Finish::Clean),
        ),
        (
            "cut-off tool call",
            Reply::sse(
                &(chunk(&json!({"choices":[{"delta":{"tool_calls":[{"id":KEY,"function":{"name":KEY,"arguments":"{\"k\":"}}]}}]}).to_string())
                    + &chunk(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#)),
                16,
                Finish::Clean,
            ),
        ),
        (
            "finish error",
            Reply::sse(
                &chunk(&format!(
                    r#"{{"choices":[{{"delta":{{"content":"{KEY}"}},"finish_reason":"error"}}]}}"#
                )),
                16,
                Finish::Clean,
            ),
        ),
        (
            "disconnect",
            Reply::sse("data: {\"choices\":[", 16, Finish::Disconnect),
        ),
    ]
}

#[tokio::test]
async fn the_key_never_leaks() {
    install_capture();
    let in_key = format!("bad credential {KEY}");
    for (name, reply) in echoing_replies() {
        let server =
            MockServer::start(vec![Route::new("POST", "/v1/chat/completions", reply)]).await;
        let p = MistralProvider::new(KEY, Some(&server.url())).unwrap();
        let errors = stream_errors(&p).await;
        assert!(!errors.is_empty(), "{name}: expected an error");
        for e in &errors {
            assert_clean(name, e);
        }
        assert!(
            !format!("{p:?}").contains(KEY),
            "{name}: key in provider Debug"
        );
        // The test is only meaningful if the key really went on the wire.
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "{name}");
        assert_eq!(
            reqs[0].header("authorization"),
            Some(format!("Bearer {KEY}").as_str()),
            "{name}"
        );
    }

    // Model listing and one model: error bodies that echo the key.
    let server = MockServer::start(vec![
        Route::new(
            "GET",
            "/v1/models",
            Reply::json(500, &json!({"message": in_key}).to_string()),
        ),
        Route::new(
            "GET",
            "/v1/models/m",
            Reply::json(500, &json!({"message": in_key}).to_string()),
        ),
    ])
    .await;
    let p = MistralProvider::new(KEY, Some(&server.url())).unwrap();
    let e = p.list_models().await.unwrap_err();
    assert_clean("models", &e);
    assert!(matches!(e, AiError::Provider(_)));
    let e = p.model_capabilities("m").await.unwrap_err();
    assert_clean("model card", &e);
    assert!(matches!(e, AiError::Provider(_)));
    assert_eq!(
        server.requests()[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );

    // Connection refused.
    let url = dead_port_url().await;
    let p = MistralProvider::new(KEY, Some(&url)).unwrap();
    let errors = stream_errors(&p).await;
    assert_eq!(errors.len(), 1);
    assert_clean("refused", &errors[0]);
    assert_clean("refused models", &p.list_models().await.unwrap_err());
    assert_clean(
        "refused card",
        &p.model_capabilities("m").await.unwrap_err(),
    );
    assert!(!format!("{p:?}").contains(KEY));

    // A silent server times out.
    let server = MockServer::start(vec![Route::new(
        "POST",
        "/v1/chat/completions",
        Reply::sse("", 8, Finish::Hold),
    )])
    .await;
    let p = MistralProvider::new(KEY, Some(&server.url()))
        .unwrap()
        .with_read_timeout(std::time::Duration::from_millis(150))
        .unwrap();
    let errors = stream_errors(&p).await;
    assert_eq!(errors, vec![AiError::Network("timed out".into())]);
    assert_clean("timeout", &errors[0]);

    // The constructor does not echo a bad key or a bad endpoint.
    let bad = format!("{KEY}\nmore");
    assert_clean("bad key", &MistralProvider::new(&bad, None).unwrap_err());
    assert_clean(
        "bad endpoint",
        &MistralProvider::new(KEY, Some(&format!("http://{KEY}@localhost"))).unwrap_err(),
    );
    assert_clean(
        "remote http",
        &MistralProvider::new(KEY, Some("http://example.com")).unwrap_err(),
    );

    let logs = captured();
    assert!(
        !logs.is_empty(),
        "the capture saw nothing: the test would prove nothing"
    );
    assert!(!logs.contains(KEY), "key in log or tracing output:\n{logs}");
    assert!(
        !logs.contains("TESTKEY"),
        "key piece in log or tracing output:\n{logs}"
    );
    assert!(
        !logs.contains("SECRET-PROMPT"),
        "prompt in log or tracing output"
    );
}
