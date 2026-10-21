// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A scripted provider for tests.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::error::AiError;
use crate::message::ChatRequest;
use crate::provider::{AiProvider, BoxFuture, Capabilities, ModelInfo};
use crate::stream::{ChatEvent, ChatStream};

type Script = Vec<Result<ChatEvent, AiError>>;

#[derive(Default)]
struct State {
    models: Vec<ModelInfo>,
    capabilities: Capabilities,
    scripts: VecDeque<Script>,
    requests: Vec<ChatRequest>,
}

/// A provider that replays scripts, one per `stream` call.
///
/// It does not add `Done` itself; a script is replayed exactly, except that
/// the stream yields `None` after the first `Err`, as the protocol requires. When no script
/// is left, the stream yields `Err(AiError::Provider("mock: no script left"))`
/// and then `None`.
pub struct ScriptedProvider {
    id: &'static str,
    state: Mutex<State>,
    open: Arc<AtomicUsize>,
}

impl ScriptedProvider {
    /// Creates a provider with this id and no scripts.
    #[must_use]
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            state: Mutex::new(State::default()),
            open: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the models `list_models` returns.
    #[must_use]
    pub fn with_models(self, models: Vec<ModelInfo>) -> Self {
        self.lock().models = models;
        self
    }

    /// Sets the capabilities.
    #[must_use]
    pub fn with_capabilities(self, capabilities: Capabilities) -> Self {
        self.lock().capabilities = capabilities;
        self
    }

    /// Queues a script for the next `stream` call that has none.
    pub fn push_script(&self, script: Vec<Result<ChatEvent, AiError>>) {
        self.lock().scripts.push_back(script);
    }

    /// Every request received, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.lock().requests.clone()
    }

    /// The number of streams that are alive.
    #[must_use]
    pub fn open_streams(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }
}

struct ScriptedStream {
    events: std::vec::IntoIter<Result<ChatEvent, AiError>>,
    failed: bool,
    open: Arc<AtomicUsize>,
}

impl ChatStream for ScriptedStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>> {
        let next = if self.failed {
            None
        } else {
            self.events.next()
        };
        if matches!(next, Some(Err(_))) {
            self.failed = true;
        }
        Box::pin(std::future::ready(next))
    }
}

impl Drop for ScriptedStream {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AiProvider for ScriptedProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
        let models = self.lock().models.clone();
        Box::pin(std::future::ready(Ok(models)))
    }

    fn stream(&self, request: ChatRequest) -> Box<dyn ChatStream + '_> {
        let script = {
            let mut st = self.lock();
            st.requests.push(request);
            st.scripts.pop_front()
        };
        let script = script
            .unwrap_or_else(|| vec![Err(AiError::Provider("mock: no script left".to_owned()))]);
        self.open.fetch_add(1, Ordering::SeqCst);
        Box::new(ScriptedStream {
            events: script.into_iter(),
            failed: false,
            open: Arc::clone(&self.open),
        })
    }

    fn capabilities(&self) -> Capabilities {
        self.lock().capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::stream::tests::block_on;
    use crate::stream::{StopReason, collect};
    use crate::tool::ToolCall;
    use crate::usage::Usage;

    fn req(q: &str) -> ChatRequest {
        ChatRequest::new("m", vec![Message::user(q)])
    }

    #[test]
    fn replays_tool_turn_then_text_turn() {
        let p = ScriptedProvider::new("mock");
        p.push_script(vec![
            Ok(ChatEvent::ToolCall(ToolCall::new("1", "t", "{}"))),
            Ok(ChatEvent::Usage(Usage::new(5, 1))),
            Ok(ChatEvent::Done {
                stop: StopReason::ToolUse,
            }),
        ]);
        p.push_script(vec![
            Ok(ChatEvent::TextDelta("ok".into())),
            Ok(ChatEvent::Done {
                stop: StopReason::EndTurn,
            }),
        ]);
        let mut s = p.stream(req("a"));
        let r1 = block_on(collect(s.as_mut())).unwrap();
        assert_eq!(r1.tool_calls.len(), 1);
        assert_eq!(r1.usage, Usage::new(5, 1));
        assert_eq!(r1.stop, StopReason::ToolUse);
        assert!(block_on(s.next_event()).is_none());
        drop(s);
        let mut s = p.stream(req("b"));
        let r2 = block_on(collect(s.as_mut())).unwrap();
        assert_eq!(r2.text, "ok");
        assert_eq!(p.requests().len(), 2);
        assert_eq!(p.requests()[1].messages[0].text(), "b");
    }

    #[test]
    fn stops_after_first_err() {
        let p = ScriptedProvider::new("mock");
        p.push_script(vec![
            Ok(ChatEvent::TextDelta("a".into())),
            Err(AiError::Provider("boom".to_owned())),
            Ok(ChatEvent::TextDelta("never".into())),
        ]);
        let mut s = p.stream(req("a"));
        assert!(block_on(s.next_event()).unwrap().is_ok());
        assert!(block_on(s.next_event()).unwrap().is_err());
        assert!(block_on(s.next_event()).is_none());
    }

    #[test]
    fn open_streams_tracks_drop() {
        let p = ScriptedProvider::new("mock");
        assert_eq!(p.open_streams(), 0);
        let s = p.stream(req("a"));
        assert_eq!(p.open_streams(), 1);
        drop(s);
        assert_eq!(p.open_streams(), 0);
    }

    #[test]
    fn exhausted_script_errors_then_ends() {
        let p = ScriptedProvider::new("mock");
        let mut s = p.stream(req("a"));
        let e = block_on(s.next_event()).unwrap().unwrap_err();
        assert_eq!(e, AiError::Provider("mock: no script left".to_owned()));
        assert!(block_on(s.next_event()).is_none());
    }

    #[test]
    fn models_capabilities_and_id() {
        let caps = Capabilities::new(true, true, false, Some(4));
        let p = ScriptedProvider::new("mock")
            .with_models(vec![ModelInfo::new("m")])
            .with_capabilities(caps);
        assert_eq!(p.id(), "mock");
        assert_eq!(p.capabilities(), caps);
        let models = block_on(p.list_models()).unwrap();
        assert_eq!(models, vec![ModelInfo::new("m")]);
    }

    #[test]
    fn is_send_sync_and_shareable() {
        fn check<T: Send + Sync>() {}
        check::<ScriptedProvider>();
        let p: Arc<dyn AiProvider> = Arc::new(ScriptedProvider::new("mock"));
        assert_eq!(p.id(), "mock");
    }
}
