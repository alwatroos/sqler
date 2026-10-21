// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Sampled chunk sizes (1, 7 and 255) must give the same events. An
//! exhaustive 1..=len run passed once but takes minutes, so it is not kept.

#![allow(clippy::unwrap_used)]

mod support;

use sqler_ai_api::{AiProvider, ChatEvent, ChatRequest, Message};
use sqler_ai_openai::OpenAiProvider;
use support::{Finish, MockServer, Reply, Route};

const TOOLS: &str = include_str!("fixtures/chat_tools_parallel.sse");
const TEXT: &str = include_str!("fixtures/chat_text.sse");

async fn events(body: &'static str, chunk: usize) -> Vec<ChatEvent> {
    let server = MockServer::start(vec![Route::new(
        "POST",
        "/v1/chat/completions",
        Reply::sse(body, chunk, Finish::Clean),
    )])
    .await;
    let p = OpenAiProvider::new(Some(&format!("{}/v1", server.url())), None).unwrap();
    let mut s = p.stream(ChatRequest::new("m", vec![Message::user("hi")]));
    let mut out = Vec::new();
    while let Some(e) = s.next_event().await {
        out.push(e.unwrap());
    }
    out
}

#[tokio::test]
async fn every_chunk_size_gives_identical_events() {
    for body in [TOOLS, TEXT] {
        let want = events(body, body.len() + 1).await;
        assert!(want.len() > 2);
        for chunk in [1, 7, 255] {
            let got = events(body, chunk).await;
            // Text deltas may be re-split only if the decoder were wrong; compare exactly.
            assert_eq!(got, want, "chunk size {chunk}");
        }
    }
}
