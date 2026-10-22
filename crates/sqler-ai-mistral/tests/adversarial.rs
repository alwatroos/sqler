// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Adversarial checks against a local mock server: a split at every byte
//! offset (LF and CRLF versions of every fixture), hostile tool-call ids,
//! hostile `Retry-After` values and hostile model ids. No real network.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use sqler_ai_api::{AiError, AiProvider, ChatEvent, ChatRequest, Message, ToolCall, ToolResult};
use sqler_ai_mistral::MistralProvider;
use support::{Body, Finish, MockServer, Reply, Route};

const FIXTURES: [(&str, &str); 6] = [
    ("tools", include_str!("fixtures/tools.sse")),
    ("text", include_str!("fixtures/text.sse")),
    ("text_bom", include_str!("fixtures/text_bom.sse")),
    ("final", include_str!("fixtures/final.sse")),
    ("cutoff", include_str!("fixtures/cutoff.sse")),
    ("error_chunk", include_str!("fixtures/error_chunk.sse")),
];

type Items = Vec<Result<ChatEvent, AiError>>;

async fn run(body: Vec<Vec<u8>>, end: Finish) -> Items {
    let server = MockServer::start(vec![Route::new(
        "POST",
        "/v1/chat/completions",
        Reply {
            status: 200,
            headers: vec![("Content-Type".into(), "text/event-stream".into())],
            body: Body::Chunked { chunks: body, end },
        },
    )])
    .await;
    let p = MistralProvider::new("mst-TESTKEY-123", Some(&server.url())).unwrap();
    let mut s = p.stream(ChatRequest::new("m", vec![Message::user("hi")]));
    let mut out = Vec::new();
    while let Some(e) = s.next_event().await {
        out.push(e);
    }
    out
}

async fn sse(data: &str) -> Items {
    run(vec![data.as_bytes().to_vec()], Finish::Clean).await
}

/// Every split of every fixture, LF and CRLF, on one server and one provider.
/// Routes are told apart by the model id in the request body.
#[tokio::test]
async fn a_split_at_every_offset_gives_the_same_events_lf_and_crlf() {
    let mut routes = Vec::new();
    let mut cases = Vec::new(); // (model, label, name)
    for (name, lf) in &FIXTURES {
        assert!(!lf.contains('\r'), "{name}: fixture must be LF (eol=lf)");
        let crlf = lf.replace('\n', "\r\n");
        for (variant, text) in [("lf", lf.to_string()), ("crlf", crlf)] {
            let bytes = text.as_bytes();
            let mut splits: Vec<Vec<Vec<u8>>> = vec![vec![bytes.to_vec()]];
            splits.extend(
                (1..bytes.len()).map(|at| vec![bytes[..at].to_vec(), bytes[at..].to_vec()]),
            );
            for (at, chunks) in splits.into_iter().enumerate() {
                let model = format!("split-{}", cases.len());
                routes.push(
                    Route::new(
                        "POST",
                        "/v1/chat/completions",
                        Reply {
                            status: 200,
                            headers: vec![("Content-Type".into(), "text/event-stream".into())],
                            body: Body::Chunked {
                                chunks,
                                end: Finish::Clean,
                            },
                        },
                    )
                    .when_body_has(&format!("\"{model}\"")),
                );
                cases.push((model, format!("{name} {variant} offset {at}"), *name));
            }
        }
    }
    let server = MockServer::start(routes).await;
    let p =
        std::sync::Arc::new(MistralProvider::new("mst-TESTKEY-123", Some(&server.url())).unwrap());
    let mut results = std::collections::BTreeMap::new();
    for batch in cases.chunks(48) {
        let mut set = tokio::task::JoinSet::new();
        for (model, label, name) in batch.iter().cloned() {
            let p = std::sync::Arc::clone(&p);
            set.spawn(async move {
                let mut s = p.stream(ChatRequest::new(model, vec![Message::user("hi")]));
                let mut out = Vec::new();
                while let Some(e) = s.next_event().await {
                    out.push(e);
                }
                (name, label, out)
            });
        }
        while let Some(r) = set.join_next().await {
            let (name, label, got) = r.unwrap();
            // The first case of each fixture and variant is the unsplit one.
            let want = results.entry(name).or_insert_with(|| got.clone());
            assert!(!want.is_empty(), "{label}");
            assert_eq!(&got, want, "{label}");
        }
    }
}

fn frame(delta: &str, finish: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{delta},\"finish_reason\":{finish}}}]}}\n\n"
    )
}

fn calls(items: &Items) -> Vec<(String, String, String)> {
    items
        .iter()
        .filter_map(|i| match i {
            Ok(ChatEvent::ToolCall(c)) => Some((c.id.clone(), c.name.clone(), c.arguments.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn hostile_ids_in_a_response_never_panic() {
    let long = "L".repeat(100_000);
    for id in ["x", "é€😀", long.as_str(), "a b\"\\"] {
        let d = serde_json::json!({"tool_calls":[{"id":id,"index":0,"function":{"name":"f","arguments":"{}"}}]});
        let items = sse(&(frame(&d.to_string(), "null")
            + &frame("{}", "\"tool_calls\"")
            + "data: [DONE]\n\n"))
        .await;
        let c = calls(&items);
        assert_eq!(c.len(), 1, "{items:?}");
        assert_eq!(c[0].0, id);
        assert!(items.iter().all(Result::is_ok), "{items:?}");
    }
}

#[tokio::test]
async fn duplicate_ids_and_repeated_index_in_a_response() {
    let mk = |id: &str, idx: u32, args: &str| {
        let d = serde_json::json!({"tool_calls":[{"id":id,"index":idx,"function":{"name":"f","arguments":args}}]});
        frame(&d.to_string(), "null")
    };
    // Distinct ids, repeated index 0: two calls.
    let items = sse(&(mk("aaaaaaaaa", 0, "{\"a\":1}")
        + &mk("bbbbbbbbb", 0, "{\"b\":2}")
        + &frame("{}", "\"tool_calls\"")
        + "data: [DONE]\n\n"))
    .await;
    assert_eq!(calls(&items).len(), 2, "{items:?}");
    // Same id twice with complete objects each: observed, must not panic and
    // must not emit truncated/invalid arguments silently.
    let items = sse(&(mk("dupdupdup", 0, "{\"a\":1}")
        + &mk("dupdupdup", 1, "{\"b\":2}")
        + &frame("{}", "\"tool_calls\"")
        + "data: [DONE]\n\n"))
    .await;
    for (_, _, args) in calls(&items) {
        assert!(
            serde_json::from_str::<serde_json::Value>(&args).is_ok(),
            "invalid args emitted: {args}"
        );
    }
    eprintln!("duplicate-id observation: {items:?}");
}

#[tokio::test]
async fn arguments_object_or_string_and_thinking_chunks() {
    let obj = frame(
        r#"{"tool_calls":[{"id":"abcdefghi","index":0,"function":{"name":"f","arguments":{"k":[1,2]}}}]}"#,
        "null",
    );
    let thinking = frame(
        r#"{"content":[{"type":"thinking","thinking":[{"type":"text","text":"SECRET-THOUGHT"}]},{"type":"text","text":"hi"}]}"#,
        "null",
    );
    let items = sse(&(thinking + &obj + &frame("{}", "\"tool_calls\"") + "data: [DONE]\n\n")).await;
    assert_eq!(
        calls(&items),
        vec![("abcdefghi".into(), "f".into(), "{\"k\":[1,2]}".into())]
    );
    assert!(
        items.contains(&Ok(ChatEvent::TextDelta("hi".into()))),
        "{items:?}"
    );
    assert!(
        !format!("{items:?}").contains("SECRET-THOUGHT"),
        "thinking leaked into events"
    );
}

#[tokio::test]
async fn finish_reason_error_mid_stream_is_an_error_and_ends() {
    let items = sse(&(frame(r#"{"content":"a"}"#, "null")
        + &frame(r#"{"content":"b"}"#, "\"error\"")
        + "data: [DONE]\n\n"))
    .await;
    assert!(matches!(items.last(), Some(Err(_))), "{items:?}");
    assert!(
        !items
            .iter()
            .any(|i| matches!(i, Ok(ChatEvent::Done { .. }))),
        "{items:?}"
    );
}

#[tokio::test]
async fn hostile_retry_after_values_never_panic() {
    for v in [
        "-1",
        "0",
        "1e9",
        "1.5",
        "99999999999999999999999",
        "18446744073709551615",
        "18446744073709551616",
        "Wed, 21 Oct 2015 07:28:00 GMT",
        "",
        " ",
        "NaN",
        "+5",
        "0x10",
        "é",
    ] {
        let server = MockServer::start(vec![Route::new(
            "POST",
            "/v1/chat/completions",
            Reply::json(429, "{}").with_header("Retry-After", v),
        )])
        .await;
        let p = MistralProvider::new("k", Some(&server.url())).unwrap();
        let mut s = p.stream(ChatRequest::new("m", vec![Message::user("hi")]));
        match s.next_event().await.unwrap().unwrap_err() {
            AiError::RateLimited { retry_after } => {
                if let Some(d) = retry_after {
                    assert!(d <= Duration::from_secs(3600), "{v:?} -> {d:?}");
                }
            }
            other => panic!("{v:?}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn tool_ids_in_history_stay_paired_and_valid() {
    let long = "z".repeat(5000);
    let ids = ["x", "é€😀", long.as_str(), "call_0", "call_1", "dup", "dup"];
    let tcs: Vec<ToolCall> = ids.iter().map(|i| ToolCall::new(*i, "f", "{}")).collect();
    let mut history = vec![Message::user("q"), Message::assistant_with_tools(None, tcs)];
    for i in ids {
        history.push(Message::tool_result(ToolResult::ok(i, "r")));
    }
    let server = MockServer::start(vec![Route::new(
        "POST",
        "/v1/chat/completions",
        Reply::sse("data: [DONE]\n\n", 64, Finish::Clean),
    )])
    .await;
    let p = MistralProvider::new("k", Some(&server.url())).unwrap();
    let mut s = p.stream(ChatRequest::new("m", history));
    while s.next_event().await.is_some() {}
    let v = server.requests()[0].json();
    let sent: Vec<String> = v["messages"][1]["tool_calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect();
    for id in &sent {
        assert!(
            id.len() == 9 && id.bytes().all(|b| b.is_ascii_alphanumeric()),
            "{id}"
        );
    }
    for (i, id) in sent.iter().enumerate() {
        assert_eq!(v["messages"][2 + i]["tool_call_id"].as_str().unwrap(), id);
    }
    // Distinct inputs should stay distinct (the last two are the same input).
    let distinct: std::collections::BTreeSet<_> = sent[..6].iter().collect();
    assert_eq!(distinct.len(), 6, "normalisation collision: {sent:?}");
}

#[tokio::test]
async fn a_literal_id_equal_to_another_ids_hash_does_not_collide() {
    // normalize("call_0") == "ckTj2MTct" (pinned in the unit tests); a model
    // that really issued the id "ckTj2MTct" next to "call_0" must keep two ids.
    let tcs = vec![
        ToolCall::new("ckTj2MTct", "f", "{}"),
        ToolCall::new("call_0", "g", "{}"),
    ];
    let history = vec![
        Message::user("q"),
        Message::assistant_with_tools(None, tcs),
        Message::tool_result(ToolResult::ok("ckTj2MTct", "r")),
        Message::tool_result(ToolResult::ok("call_0", "r")),
    ];
    let server = MockServer::start(vec![Route::new(
        "POST",
        "/v1/chat/completions",
        Reply::sse("data: [DONE]\n\n", 64, Finish::Clean),
    )])
    .await;
    let p = MistralProvider::new("k", Some(&server.url())).unwrap();
    let mut s = p.stream(ChatRequest::new("m", history));
    while s.next_event().await.is_some() {}
    let v = server.requests()[0].json();
    let c = v["messages"][1]["tool_calls"].as_array().unwrap();
    assert_ne!(
        c[0]["id"], c[1]["id"],
        "two distinct call ids collapsed to one"
    );
}

#[tokio::test]
async fn hostile_model_ids_in_the_path() {
    let server = MockServer::start(vec![]).await;
    let p = MistralProvider::new("k", Some(&server.url())).unwrap();
    for id in [
        "a/b", "a/../b", "%2e", "%2e%2e", "a b", "a%2Fb", "é", "a?b#c", "a\\b", "a\nb",
    ] {
        let _ = p.model_capabilities(id).await;
    }
    for r in server.requests() {
        let rest = r
            .path
            .strip_prefix("/v1/models/")
            .unwrap_or_else(|| panic!("{}", r.path));
        assert!(
            !rest.contains('/')
                && !rest.contains(' ')
                && !rest.contains('?')
                && !rest.contains('#'),
            "{}",
            r.path
        );
        assert!(rest != "." && rest != "..", "{}", r.path);
        assert!(
            !rest.to_ascii_lowercase().contains("%2e%2e") || rest.contains("%252e"),
            "{}",
            r.path
        );
    }
    for id in ["", ".", ".."] {
        assert!(p.model_capabilities(id).await.is_err());
    }
    eprintln!(
        "paths: {:?}",
        server
            .requests()
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn auth_echoed_in_error_body_and_sse_frame_is_scrubbed() {
    let key = "mst-ECHO-KEY-987";
    let echo = format!("Bearer {key}");
    for reply in [
        Reply::json(
            500,
            &serde_json::json!({"message": format!("bad header Authorization: {echo}")})
                .to_string(),
        ),
        Reply::json(401, &format!("Authorization: {echo}")),
        Reply::sse(
            &format!(
                "data: {}\n\n",
                serde_json::json!({"object":"error","message":format!("Authorization: {echo}")})
            ),
            9,
            Finish::Clean,
        ),
        Reply::sse(&format!("data: {echo}\n\n"), 9, Finish::Clean),
    ] {
        let server =
            MockServer::start(vec![Route::new("POST", "/v1/chat/completions", reply)]).await;
        let p = MistralProvider::new(key, Some(&server.url())).unwrap();
        let mut s = p.stream(ChatRequest::new("m", vec![Message::user("hi")]));
        while let Some(i) = s.next_event().await {
            let shown = format!("{i:?}");
            assert!(!shown.contains(key) && !shown.contains("ECHO"), "{shown}");
            if let Err(e) = i {
                assert!(!e.to_string().contains("ECHO"));
            }
        }
        assert!(!format!("{p:?}").contains("ECHO"));
    }
}
