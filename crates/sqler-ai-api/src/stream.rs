// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use crate::error::AiError;
use crate::message::Message;
use crate::provider::BoxFuture;
use crate::tool::ToolCall;
use crate::usage::Usage;

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// The model finished its turn.
    EndTurn,
    /// The model wants tool results.
    ToolUse,
    /// The output hit the token cap.
    MaxTokens,
    /// A provider-specific reason.
    Other(String),
}

/// A normalised event of a chat stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChatEvent {
    /// A piece of text.
    TextDelta(String),
    /// A complete tool call. Providers buffer argument fragments.
    ToolCall(ToolCall),
    /// A token-count increment.
    Usage(Usage),
    /// The end of the response.
    Done {
        /// Why the model stopped.
        stop: StopReason,
    },
}

/// A stream of chat events.
///
/// Protocol: any number of `TextDelta`, `ToolCall` and `Usage` events in any
/// order, then exactly one `Done`, then `None` forever. An `Err` ends the
/// stream and is followed by `None`. Dropping the stream aborts the
/// underlying request; no event and no error comes after a drop.
pub trait ChatStream: Send {
    /// The next event, or `None` when the stream is over.
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>>;
}

/// A whole response, assembled from a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatResponse {
    /// The concatenated text.
    pub text: String,
    /// The tool calls, in order.
    pub tool_calls: Vec<ToolCall>,
    /// The summed usage.
    pub usage: Usage,
    /// Why the model stopped.
    pub stop: StopReason,
}

impl ChatResponse {
    /// Creates a response.
    #[must_use]
    pub fn new(
        text: impl Into<String>,
        tool_calls: Vec<ToolCall>,
        usage: Usage,
        stop: StopReason,
    ) -> Self {
        Self {
            text: text.into(),
            tool_calls,
            usage,
            stop,
        }
    }

    /// Converts the response to an assistant message. Empty text becomes `None`.
    #[must_use]
    pub fn into_message(self) -> Message {
        let text = if self.text.is_empty() {
            None
        } else {
            Some(self.text)
        };
        Message::assistant_with_tools(text, self.tool_calls)
    }
}

/// Reads a stream to its `Done` and assembles the response.
///
/// # Errors
///
/// Returns [`AiError::Protocol`] when the stream ends before `Done`, and passes
/// stream errors through unchanged.
pub async fn collect(stream: &mut (dyn ChatStream + '_)) -> Result<ChatResponse, AiError> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = Usage::default();
    loop {
        match stream.next_event().await {
            None => return Err(AiError::Protocol("stream ended before Done".to_owned())),
            Some(Err(e)) => return Err(e),
            Some(Ok(ChatEvent::TextDelta(t))) => text.push_str(&t),
            Some(Ok(ChatEvent::ToolCall(c))) => tool_calls.push(c),
            Some(Ok(ChatEvent::Usage(u))) => usage += u,
            Some(Ok(ChatEvent::Done { stop })) => {
                return Ok(ChatResponse {
                    text,
                    tool_calls,
                    usage,
                    stop,
                });
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    use super::*;
    use crate::message::Role;

    /// Polls a future to completion; the mocks are always ready.
    pub(crate) fn block_on<F: Future>(fut: F) -> F::Output {
        let mut fut = std::pin::pin!(fut);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    struct VecStream(std::vec::IntoIter<Result<ChatEvent, AiError>>);

    impl VecStream {
        fn new(events: Vec<Result<ChatEvent, AiError>>) -> Self {
            Self(events.into_iter())
        }
    }

    impl ChatStream for VecStream {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>> {
            let next = self.0.next();
            Box::pin(std::future::ready(next))
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn done() -> Result<ChatEvent, AiError> {
        Ok(ChatEvent::Done {
            stop: StopReason::EndTurn,
        })
    }

    #[allow(clippy::unnecessary_wraps)]
    fn text(t: &str) -> Result<ChatEvent, AiError> {
        Ok(ChatEvent::TextDelta(t.to_owned()))
    }

    #[test]
    fn happy_path() {
        let mut s = VecStream::new(vec![text("he"), text("llo"), done()]);
        let r = block_on(collect(&mut s)).unwrap();
        assert_eq!(r.text, "hello");
        assert_eq!(r.stop, StopReason::EndTurn);
        assert_eq!(r.tool_calls.len(), 0);
    }

    #[test]
    fn interleaved_tool_call_and_text() {
        let mut s = VecStream::new(vec![
            text("a"),
            Ok(ChatEvent::ToolCall(ToolCall::new("1", "t1", "{}"))),
            text("b"),
            Ok(ChatEvent::ToolCall(ToolCall::new("2", "t2", "{}"))),
            Ok(ChatEvent::Done {
                stop: StopReason::ToolUse,
            }),
        ]);
        let r = block_on(collect(&mut s)).unwrap();
        assert_eq!(r.text, "ab");
        assert_eq!(r.tool_calls.len(), 2);
        assert_eq!(r.tool_calls[1].id, "2");
        let m = r.into_message();
        assert_eq!(m.role(), Role::Assistant);
        assert_eq!(m.parts().len(), 3);
    }

    #[test]
    fn end_before_done() {
        let mut s = VecStream::new(vec![text("a")]);
        let e = block_on(collect(&mut s)).unwrap_err();
        assert_eq!(e, AiError::Protocol("stream ended before Done".to_owned()));
    }

    #[test]
    fn error_passthrough() {
        let mut s = VecStream::new(vec![text("a"), Err(AiError::Auth("no".into())), done()]);
        let e = block_on(collect(&mut s)).unwrap_err();
        assert_eq!(e, AiError::Auth("no".into()));
    }

    #[test]
    fn usage_is_summed() {
        let mut s = VecStream::new(vec![
            Ok(ChatEvent::Usage(Usage::new(10, 0))),
            text("x"),
            Ok(ChatEvent::Usage(Usage::new(0, 3))),
            Ok(ChatEvent::Usage(Usage::new(0, 4))),
            done(),
        ]);
        let r = block_on(collect(&mut s)).unwrap();
        assert_eq!(r.usage, Usage::new(10, 7));
    }

    #[test]
    fn empty_text_becomes_none() {
        let r = ChatResponse {
            text: String::new(),
            tool_calls: vec![],
            usage: Usage::default(),
            stop: StopReason::EndTurn,
        };
        assert_eq!(r.into_message().parts().len(), 0);
    }
}
