// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The chat stream: a state machine over one HTTP response.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use reqwest::Response;
use sqler_ai_api::{AiError, BoxFuture, ChatEvent, ChatStream};

use crate::convert::LineMapper;
use crate::error::{from_reqwest, from_response};
use crate::ndjson::LineDecoder;

pub(crate) type SendFuture = Pin<Box<dyn Future<Output = reqwest::Result<Response>> + Send>>;

enum State {
    /// The request could not be built; the error is the first item.
    Failed(AiError),
    /// The request is being sent.
    Sending(SendFuture),
    /// The response body is being read. The response lives here, so dropping
    /// the stream drops the hyper body and closes the socket.
    Reading(Reading),
    /// After `Done` or any error.
    Finished,
}

struct Reading {
    resp: Response,
    decoder: LineDecoder,
    mapper: LineMapper,
    queue: VecDeque<Result<ChatEvent, AiError>>,
    /// The `done` line was mapped; nothing more is read.
    done: bool,
}

/// The `ChatStream` of an `OllamaProvider`.
pub(crate) struct OllamaStream {
    state: State,
}

impl OllamaStream {
    pub(crate) fn failed(e: AiError) -> Self {
        Self {
            state: State::Failed(e),
        }
    }

    pub(crate) fn sending(fut: SendFuture) -> Self {
        Self {
            state: State::Sending(fut),
        }
    }

    async fn advance(&mut self) -> Option<Result<ChatEvent, AiError>> {
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
                            self.state = State::Reading(Reading {
                                resp,
                                decoder: LineDecoder::new(),
                                mapper: LineMapper::default(),
                                queue: VecDeque::new(),
                                done: false,
                            });
                        }
                        Ok(resp) => {
                            // Finished first: a dropped `next_event` must not
                            // leave the completed send future to be polled again.
                            self.state = State::Finished;
                            let e = from_response(resp).await;
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
                        // Cannot happen: the done line queues `Done` last.
                        self.state = State::Finished;
                        return None;
                    }
                    let chunk = r.resp.chunk().await;
                    match chunk {
                        Ok(Some(bytes)) => {
                            let (lines, err) = r.decoder.push(&bytes);
                            r.map_lines(lines);
                            // Events before the oversize line come first; a
                            // `done` or a bad line before it wins.
                            if let Some(e) = err
                                && !r.done
                                && !matches!(r.queue.back(), Some(Err(_)))
                            {
                                r.queue.push_back(Err(e));
                            }
                        }
                        Ok(None) => {
                            if let Some(last) = r.decoder.finish() {
                                r.map_lines(vec![last]);
                            }
                            if r.queue.is_empty() && !r.done {
                                r.queue.push_back(Err(AiError::Protocol(
                                    "stream ended before done".to_owned(),
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
    /// Maps lines to events; stops at the first error or at the `done` line.
    fn map_lines(&mut self, lines: Vec<Vec<u8>>) {
        for line in lines {
            match self.mapper.map_line(&line, &mut self.queue) {
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

impl ChatStream for OllamaStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>> {
        Box::pin(self.advance())
    }
}
