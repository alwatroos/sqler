// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The chat stream: a state machine over one HTTP response.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use reqwest::Response;
use reqwest::header::HeaderValue;
use sqler_ai_api::{AiError, BoxFuture, ChatEvent, ChatStream};

use crate::convert::SseMapper;
use crate::error::{from_reqwest, from_response};
use crate::sse::{SseDecoder, SseEvent};

pub(crate) type SendFuture = Pin<Box<dyn Future<Output = reqwest::Result<Response>> + Send>>;

enum State {
    /// The request could not be built; the error is the first item.
    Failed(AiError),
    /// The request is being sent.
    Sending(SendFuture),
    /// The response body is being read. The response lives here, so dropping
    /// the stream drops the hyper body and closes the socket.
    Reading(Box<Reading>),
    /// After `Done` or any error.
    Finished,
}

struct Reading {
    resp: Response,
    decoder: SseDecoder,
    mapper: SseMapper,
    queue: VecDeque<Result<ChatEvent, AiError>>,
    /// `message_stop` was mapped; nothing more is read.
    done: bool,
}

/// The `ChatStream` of an `AnthropicProvider`.
pub(crate) struct AnthropicStream {
    state: State,
    /// Only used to scrub error text.
    key: HeaderValue,
}

impl AnthropicStream {
    pub(crate) fn failed(e: AiError, key: HeaderValue) -> Self {
        Self {
            state: State::Failed(e),
            key,
        }
    }

    pub(crate) fn sending(fut: SendFuture, key: HeaderValue) -> Self {
        Self {
            state: State::Sending(fut),
            key,
        }
    }

    async fn advance(&mut self) -> Option<Result<ChatEvent, AiError>> {
        let key = self.key.to_str().unwrap_or_default();
        loop {
            match &mut self.state {
                State::Finished => return None,
                State::Failed(_) => {
                    let State::Failed(e) = std::mem::replace(&mut self.state, State::Finished)
                    else {
                        return None;
                    };
                    return Some(Err(e));
                }
                State::Sending(fut) => {
                    let sent = fut.as_mut().await;
                    match sent {
                        Ok(resp) if resp.status().is_success() => {
                            self.state = State::Reading(Box::new(Reading {
                                resp,
                                decoder: SseDecoder::new(),
                                mapper: SseMapper::default(),
                                queue: VecDeque::new(),
                                done: false,
                            }));
                        }
                        Ok(resp) => {
                            // Finished first: a dropped `next_event` must not
                            // leave the completed send future to be polled again.
                            self.state = State::Finished;
                            let e = from_response(resp, key).await;
                            return Some(Err(e));
                        }
                        Err(e) => {
                            self.state = State::Finished;
                            return Some(Err(from_reqwest(e)));
                        }
                    }
                }
                State::Reading(r) => {
                    if let Some(item) = r.queue.pop_front() {
                        if item.is_err() || matches!(item, Ok(ChatEvent::Done { .. })) {
                            self.state = State::Finished;
                        }
                        return Some(item);
                    }
                    if r.done {
                        // Cannot happen: `message_stop` queues `Done` last.
                        self.state = State::Finished;
                        return None;
                    }
                    let chunk = r.resp.chunk().await;
                    match chunk {
                        Ok(Some(bytes)) => {
                            let (events, err) = r.decoder.push(&bytes);
                            r.map_events(events, key);
                            // Events before the oversize one come first; a
                            // `message_stop` or a bad event before it wins.
                            if let Some(e) = err
                                && !r.done
                                && !matches!(r.queue.back(), Some(Err(_)))
                            {
                                r.queue.push_back(Err(e));
                            }
                        }
                        Ok(None) => {
                            if let Some(last) = r.decoder.finish() {
                                r.map_events(vec![last], key);
                            }
                            if !r.done && !matches!(r.queue.back(), Some(Err(_))) {
                                r.queue.push_back(Err(AiError::Protocol(
                                    "stream ended before message_stop".to_owned(),
                                )));
                            }
                        }
                        Err(e) => r.queue.push_back(Err(from_reqwest(e))),
                    }
                }
            }
        }
    }
}

impl Reading {
    /// Maps events; stops at the first error or at `message_stop`.
    fn map_events(&mut self, events: Vec<SseEvent>, key: &str) {
        for ev in events {
            match self.mapper.map_event(&ev, key, &mut self.queue) {
                Ok(true) => {
                    self.done = true;
                    return;
                }
                Ok(false) => {}
                Err(e) => {
                    self.queue.push_back(Err(e));
                    return;
                }
            }
        }
    }
}

impl ChatStream for AnthropicStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>> {
        Box::pin(self.advance())
    }
}
