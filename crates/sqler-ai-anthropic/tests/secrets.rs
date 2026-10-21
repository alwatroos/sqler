// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The API key must never reach an error text, a `Debug` print, a `log`
//! record or a `tracing` event or span, whatever the failure.

#![allow(clippy::unwrap_used)]

mod support;

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once};

use sqler_ai_anthropic::AnthropicProvider;
use sqler_ai_api::{AiError, AiProvider, ChatRequest, Message};
use support::{Finish, MockServer, Reply, Route, dead_port_url};

const KEY: &str = "sk-ant-test-0123456789SECRET";

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
    ChatRequest::new("claude-sonnet-5-5", vec![Message::user("SECRET-PROMPT")])
}

fn assert_clean(what: &str, e: &AiError) {
    for shown in [e.to_string(), format!("{e:?}")] {
        assert!(!shown.contains(KEY), "{what}: key in {shown}");
        assert!(!shown.contains("SECRET"), "{what}: key piece in {shown}");
    }
}

async fn stream_errors(p: &AnthropicProvider) -> Vec<AiError> {
    let mut s = p.stream(request());
    let mut errors = Vec::new();
    while let Some(item) = s.next_event().await {
        if let Err(e) = item {
            errors.push(e);
        }
    }
    errors
}

fn error_body(kind: &str, message: &str) -> String {
    serde_json::json!({"type":"error","error":{"type":kind,"message":message}}).to_string()
}

/// Replies that carry the key back in some form.
fn echoing_replies() -> Vec<(&'static str, Reply)> {
    let long = format!("{}{KEY} tail", "x".repeat(190));
    let in_key = format!("bad credential {KEY}");
    let event_data = |data: &str| format!("event: error\ndata: {data}\n\n");
    vec![
        (
            "401",
            Reply::json(401, &error_body("authentication_error", &in_key)),
        ),
        ("500", Reply::json(500, &error_body("api_error", "boom"))),
        (
            "500 with key",
            Reply::json(500, &error_body("api_error", &in_key)),
        ),
        (
            "400 cut",
            Reply::json(400, &error_body("invalid_request_error", &long)),
        ),
        (
            "429",
            Reply::json(429, &error_body("rate_limit_error", &in_key))
                .with_header("Retry-After", "2"),
        ),
        (
            "529",
            Reply::json(529, &error_body("overloaded_error", &in_key)),
        ),
        (
            "error event",
            Reply::sse(
                &event_data(&error_body("api_error", &in_key)),
                16,
                Finish::Clean,
            ),
        ),
        (
            "error event type",
            Reply::sse(
                &event_data(&error_body(&in_key, &in_key)),
                16,
                Finish::Clean,
            ),
        ),
        (
            "malformed event",
            Reply::sse(
                &format!("data: {{\"type\":\"content_block_delta\",\"index\":\"{KEY}\"}}\n\n"),
                16,
                Finish::Clean,
            ),
        ),
    ]
}

#[tokio::test]
async fn the_key_never_leaks() {
    install_capture();
    let in_key = format!("bad credential {KEY}");
    for (name, reply) in echoing_replies() {
        let server = MockServer::start(vec![Route::new("POST", "/v1/messages", reply)]).await;
        let p = AnthropicProvider::new(KEY, Some(&server.url())).unwrap();
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
        assert_eq!(reqs[0].header("x-api-key"), Some(KEY), "{name}");
    }

    // Model listing: an error body that echoes the key.
    let server = MockServer::start(vec![Route::new(
        "GET",
        "/v1/models",
        Reply::json(500, &error_body("api_error", &in_key)),
    )])
    .await;
    let p = AnthropicProvider::new(KEY, Some(&server.url())).unwrap();
    let e = p.list_models().await.unwrap_err();
    assert_clean("models", &e);
    assert!(matches!(e, AiError::Provider(_)));
    assert_eq!(server.requests()[0].header("x-api-key"), Some(KEY));

    // Connection refused.
    let url = dead_port_url().await;
    let p = AnthropicProvider::new(KEY, Some(&url)).unwrap();
    let errors = stream_errors(&p).await;
    assert_eq!(errors.len(), 1);
    assert_clean("refused", &errors[0]);
    assert_clean("refused models", &p.list_models().await.unwrap_err());
    assert!(!format!("{p:?}").contains(KEY));

    // The constructor does not echo a bad key.
    let bad = format!("{KEY}\nmore");
    let e = AnthropicProvider::new(&bad, None).unwrap_err();
    assert_clean("bad key", &e);

    let logs = captured();
    assert!(
        !logs.is_empty(),
        "the capture saw nothing: the test would prove nothing"
    );
    assert!(!logs.contains(KEY), "key in log or tracing output:\n{logs}");
    assert!(
        !logs.contains("SECRET"),
        "key piece in log or tracing output:\n{logs}"
    );
    assert!(
        !logs.contains("SECRET-PROMPT"),
        "prompt in log or tracing output"
    );
}
