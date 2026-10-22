// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Sampled chunk sizes (1, 7 and 255) must give the same events, including
//! for a stream that starts with a byte order mark.

#![allow(clippy::unwrap_used)]

mod support;

use sqler_ai_api::{AiProvider, ChatEvent, ChatRequest, Message};
use sqler_ai_mistral::MistralProvider;
use support::{Finish, MockServer, Reply, Route};

const TOOLS: &str = include_str!("fixtures/tools.sse");
const TEXT: &str = include_str!("fixtures/text.sse");
const TEXT_BOM: &str = include_str!("fixtures/text_bom.sse");

async fn events(body: &'static str, chunk: usize) -> Vec<ChatEvent> {
    let server = MockServer::start(vec![Route::new(
        "POST",
        "/v1/chat/completions",
        Reply::sse(body, chunk, Finish::Clean),
    )])
    .await;
    let p = MistralProvider::new("mst-TESTKEY-123", Some(&server.url())).unwrap();
    let mut s = p.stream(ChatRequest::new("m", vec![Message::user("hi")]));
    let mut out = Vec::new();
    while let Some(e) = s.next_event().await {
        out.push(e.unwrap());
    }
    out
}

#[tokio::test]
async fn every_chunk_size_gives_identical_events() {
    for body in [TOOLS, TEXT, TEXT_BOM] {
        let want = events(body, body.len() + 1).await;
        assert!(want.len() > 2);
        for chunk in [1, 7, 255] {
            assert_eq!(events(body, chunk).await, want, "chunk size {chunk}");
        }
    }
}

#[tokio::test]
async fn the_bom_does_not_change_the_events() {
    assert!(TEXT_BOM.starts_with('\u{feff}'));
    assert_eq!(events(TEXT_BOM, 2).await, events(TEXT, 2).await);
}
