// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The turn loop of an exploration session.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sqler_ai_api::{
    AiError, AiProvider, ChatEvent, ChatRequest, Message, Role, StopReason, ToolCall, Usage,
};
use thiserror::Error;

use crate::approve::Approver;
use crate::session::approval::SessionApprover;
use crate::session::cancel::{Cancelled, SessionCancel, cancellable};
use crate::session::config::SessionConfig;
use crate::session::events::{EventHub, SessionEvent, SessionEvents, SessionState};
use crate::session::prompt::{prompt_for, schema_section};
use crate::session::repeat::RepeatGuard;
use crate::session::restore::{PriorChat, prior_history};
use crate::session::schema::{FocusTable, build_schema_context};
use crate::session::transcript::{NotRunReason, Notice, Transcript, TranscriptEntry};
use crate::source::ConnectionInfo;
use crate::toolset::ToolSet;

/// Which limit stopped a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitKind {
    /// The most model steps of a turn.
    ModelSteps,
    /// The most tool calls of a turn.
    ToolCalls,
    /// The token budget of the session.
    Tokens,
    /// The wall-clock limit of a turn.
    Time,
}

/// Why a session could not be built or a turn failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The provider failed.
    #[error("{0}")]
    Provider(AiError),
    /// The provider cannot call tools, so it cannot explore a database.
    #[error("the provider does not support tool calls")]
    ToolsUnsupported,
}

/// How a user turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TurnOutcome {
    /// The model ended its turn.
    Completed,
    /// A limit stopped the turn.
    LimitReached(LimitKind),
    /// The model repeated the same call too often.
    LoopDetected,
    /// The turn was cancelled.
    Cancelled,
    /// The provider failed.
    Failed(SessionError),
}

/// The longest the schema context of a turn may take to build.
const SCHEMA_CONTEXT_TIMEOUT: Duration = Duration::from_secs(10);

/// What one model step produced.
struct StepData {
    text: String,
    calls: Vec<ToolCall>,
    stop: StopReason,
}

/// A chat with a model about one connection. Not `Clone`: the history is one conversation.
pub struct ExplorationSession {
    provider: Arc<dyn AiProvider>,
    tools: ToolSet,
    config: SessionConfig,
    prompt: String,
    history: Vec<Message>,
    transcript: Transcript,
    usage: Usage,
    state: Arc<Mutex<SessionState>>,
    waited: Arc<Mutex<Duration>>,
    events: Arc<EventHub>,
    cancel: SessionCancel,
    repeat: RepeatGuard,
    info: ConnectionInfo,
    focus: Vec<FocusTable>,
    schema: Option<String>,
}

impl std::fmt::Debug for ExplorationSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplorationSession")
            .field("state", &self.state())
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}

/// Marks a turn cancelled when its `send` future is dropped before it ended.
struct TurnGuard<'a> {
    session: &'a mut ExplorationSession,
    ended: bool,
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        if !self.ended {
            self.session.conclude(TurnOutcome::Cancelled);
        }
    }
}

impl ExplorationSession {
    /// A session bound to `connection`.
    ///
    /// `tools` must lend only that connection (the host builds the source) and should audit with
    /// `Surface::Session`. With an `approver`, statements that need approval ask it; without
    /// one the tool set refuses them.
    ///
    /// # Errors
    /// [`SessionError::ToolsUnsupported`] if the provider cannot call tools.
    #[allow(clippy::needless_pass_by_value)] // the contract takes the description by value
    pub fn new(
        provider: Arc<dyn AiProvider>,
        tools: ToolSet,
        connection: ConnectionInfo,
        approver: Option<Arc<dyn Approver>>,
        config: SessionConfig,
    ) -> Result<Self, SessionError> {
        if !provider.capabilities().tools {
            return Err(SessionError::ToolsUnsupported);
        }
        let prompt = prompt_for(&connection, config.prompt);
        let state = Arc::new(Mutex::new(SessionState::Idle));
        let waited = Arc::new(Mutex::new(Duration::ZERO));
        let events = EventHub::new();
        let tools = match approver {
            Some(inner) => tools.with_approver(Arc::new(SessionApprover {
                inner,
                events: events.clone(),
                state: Arc::clone(&state),
                waited: Arc::clone(&waited),
            })),
            None => tools,
        };
        let repeat = RepeatGuard::new(config.limits.max_identical_calls);
        Ok(Self {
            provider,
            tools,
            config,
            prompt,
            history: Vec::new(),
            transcript: Transcript::default(),
            usage: Usage::default(),
            state,
            waited,
            events,
            cancel: SessionCancel::default(),
            repeat,
            info: connection,
            focus: Vec::new(),
            schema: None,
        })
    }

    /// Sends session events to `events` (the default drops them).
    #[must_use]
    pub fn with_events(self, events: Arc<dyn SessionEvents>) -> Self {
        self.events.set(events);
        self
    }

    /// Seeds the session with a restored chat: its entries become the transcript (the stable
    /// numbers continue from `prior.first_seq`) and its user and assistant text becomes the
    /// history the model sees. Nothing is emitted, and no provider state is resumed.
    #[must_use]
    pub fn with_prior(mut self, prior: PriorChat) -> Self {
        self.transcript = Transcript::restore(prior.entries, prior.first_seq);
        self.history = prior_history(self.transcript.entries());
        self
    }

    /// A handle that cancels the running turn from anywhere.
    #[must_use]
    pub fn cancel_handle(&self) -> SessionCancel {
        self.cancel.clone()
    }

    /// Sets the tables the user is looking at; the next turn's schema context lists them first.
    pub fn set_focus(&mut self, focus: Vec<FocusTable>) {
        self.focus = focus;
    }

    /// The schema context of the last turn (the wrapped envelope), if one was built.
    #[must_use]
    pub fn schema_context(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// The transcript so far.
    #[must_use]
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// The state of the session.
    #[must_use]
    pub fn state(&self) -> SessionState {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The tokens used by the whole session.
    #[must_use]
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Runs one user turn and returns how it ended.
    ///
    /// Dropping the returned future cancels the turn. A message sent after a turn that left no
    /// answer (a cancel, a failure) is merged into the pending user message.
    pub async fn send(&mut self, text: impl Into<String>) -> TurnOutcome {
        let text = text.into();
        self.cancel.reset();
        self.repeat.reset();
        *self.waited.lock().unwrap_or_else(PoisonError::into_inner) = Duration::ZERO;
        self.add_entry(TranscriptEntry::User { text: text.clone() });
        self.push_user(&text);
        let mut guard = TurnGuard {
            session: self,
            ended: false,
        };
        let outcome = match guard.session.load_schema(&text).await {
            Ok(()) => guard.session.turn().await,
            Err(outcome) => outcome,
        };
        guard.ended = true;
        guard.session.conclude(outcome)
    }

    /// Builds the schema context of this turn. `Err` is the outcome that ends the turn: only a
    /// cancel does; a timeout or an empty listing leaves the turn without a schema.
    async fn load_schema(&mut self, text: &str) -> Result<(), TurnOutcome> {
        self.schema = None;
        let Some(options) = self.config.schema_context else {
            return Ok(());
        };
        let tools = self.tools.clone();
        let info = self.info.clone();
        let focus = self.focus.clone();
        let build = build_schema_context(&tools, &info, text, &focus, options);
        match cancellable(
            &self.cancel,
            tokio::time::timeout(SCHEMA_CONTEXT_TIMEOUT, build),
        )
        .await
        {
            Err(Cancelled) => Err(TurnOutcome::Cancelled),
            Ok(Err(_elapsed)) => Ok(()),
            Ok(Ok(context)) => {
                self.schema = context;
                Ok(())
            }
        }
    }

    fn push_user(&mut self, text: &str) {
        match self.history.last() {
            Some(last) if last.role() == Role::User => {
                let merged = format!("{}\n\n{text}", last.text());
                if let Some(slot) = self.history.last_mut() {
                    *slot = Message::user(merged);
                }
            }
            _ => self.history.push(Message::user(text)),
        }
    }

    fn emit(&self, event: SessionEvent) {
        self.events.emit(event);
    }

    fn set_state(&self, state: SessionState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
        self.emit(SessionEvent::State(state));
    }

    fn add_entry(&mut self, entry: TranscriptEntry) {
        self.transcript.push(entry.clone());
        self.emit(SessionEvent::Entry(entry));
    }

    fn add_notice(&mut self, notice: Notice) {
        self.add_entry(TranscriptEntry::Notice(notice));
    }

    /// Emits the entries appended since index `from`.
    fn emit_since(&self, from: usize) {
        for entry in &self.transcript.entries()[from..] {
            self.emit(SessionEvent::Entry(entry.clone()));
        }
    }

    /// The end of every turn: the closing notice, the state and the `TurnEnded` event.
    fn conclude(&mut self, outcome: TurnOutcome) -> TurnOutcome {
        let state = match &outcome {
            TurnOutcome::Completed | TurnOutcome::LimitReached(_) | TurnOutcome::LoopDetected => {
                SessionState::Done
            }
            TurnOutcome::Cancelled => SessionState::Cancelled,
            TurnOutcome::Failed(_) => SessionState::Failed,
        };
        match &outcome {
            TurnOutcome::LimitReached(kind) => self.add_notice(Notice::LimitReached(*kind)),
            TurnOutcome::Cancelled => self.add_notice(Notice::Cancelled),
            _ => {}
        }
        self.set_state(state);
        self.emit(SessionEvent::TurnEnded(outcome.clone()));
        outcome
    }

    fn waited(&self) -> Duration {
        *self.waited.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The turn time used so far, without approval waits.
    fn active(&self, started: Instant) -> Duration {
        started.elapsed().saturating_sub(self.waited())
    }

    fn limit_hit(&self, steps: u32, started: Instant) -> Option<LimitKind> {
        let limits = &self.config.limits;
        if steps >= limits.max_model_steps {
            Some(LimitKind::ModelSteps)
        } else if self.usage.total() >= limits.token_budget {
            Some(LimitKind::Tokens)
        } else if self.active(started) >= limits.turn_timeout {
            Some(LimitKind::Time)
        } else {
            None
        }
    }

    fn request(&self) -> ChatRequest {
        let mut request = ChatRequest::new(self.config.model.clone(), self.history.clone());
        request.system = Some(if self.config.schema_context.is_some() {
            format!("{}{}", self.prompt, schema_section(self.schema.as_deref()))
        } else {
            self.prompt.clone()
        });
        request.tools = self.tools.definitions();
        request.max_output_tokens = self.config.max_output_tokens;
        request.temperature = self.config.temperature;
        request
    }

    /// The loop of one user turn.
    async fn turn(&mut self) -> TurnOutcome {
        let started = Instant::now();
        let mut steps = 0u32;
        let mut tool_calls = 0u32;
        loop {
            if let Some(kind) = self.limit_hit(steps, started) {
                return TurnOutcome::LimitReached(kind);
            }
            steps += 1;
            let step = match self.step(started).await {
                Ok(step) => step,
                Err(outcome) => return outcome,
            };
            if !step.text.is_empty() {
                self.add_entry(TranscriptEntry::Assistant {
                    text: step.text.clone(),
                });
            }
            if step.stop == StopReason::MaxTokens {
                self.add_notice(Notice::OutputTruncated);
            }
            if step.calls.is_empty() {
                if !step.text.is_empty() {
                    self.history.push(Message::assistant(step.text));
                }
                return TurnOutcome::Completed;
            }
            if let Some(outcome) = self.run_calls(step, &mut tool_calls).await {
                return outcome;
            }
        }
    }

    /// Runs the calls of a step in order. `Some` ends the turn.
    async fn run_calls(&mut self, step: StepData, tool_calls: &mut u32) -> Option<TurnOutcome> {
        let StepData { text, calls, .. } = step;
        let mut dispatched = Vec::new();
        let mut results = Vec::new();
        let mut halt: Option<(TurnOutcome, NotRunReason)> = None;
        for call in &calls {
            if let Some((_, reason)) = &halt {
                self.transcript.push_not_run(call, *reason);
                self.emit_since(self.transcript.entries().len() - 1);
                continue;
            }
            let refusal = if *tool_calls >= self.config.limits.max_tool_calls {
                Some((
                    TurnOutcome::LimitReached(LimitKind::ToolCalls),
                    NotRunReason::Limit(LimitKind::ToolCalls),
                ))
            } else if self.repeat.admit(call) {
                None
            } else {
                Some((TurnOutcome::LoopDetected, NotRunReason::Repeated))
            };
            if let Some((outcome, reason)) = refusal {
                self.transcript.push_not_run(call, reason);
                self.emit_since(self.transcript.entries().len() - 1);
                if reason == NotRunReason::Repeated {
                    self.add_notice(Notice::LoopDetected {
                        tool: call.name.clone(),
                    });
                }
                halt = Some((outcome, reason));
                continue;
            }
            self.set_state(SessionState::ToolCallPending);
            let tools = self.tools.clone();
            if let Ok(report) = cancellable(&self.cancel, tools.call_detailed(call)).await {
                *tool_calls += 1;
                let from = self.transcript.entries().len();
                self.transcript.push_report(call, &report);
                self.emit_since(from);
                dispatched.push(call.clone());
                results.push(report.result);
            } else {
                // A cancel while the user is still asked cannot have run anything. After that
                // the call may have sent statements; the tool set recorded the details in one
                // `cancelled` audit entry when it dropped the call.
                if self.state() == SessionState::AwaitingApproval {
                    self.transcript.push_not_run(call, NotRunReason::Cancelled);
                } else {
                    self.transcript.push_cancelled(call);
                }
                self.emit_since(self.transcript.entries().len() - 1);
                halt = Some((TurnOutcome::Cancelled, NotRunReason::Cancelled));
            }
        }
        // Only calls that ran reach the history, each with its result.
        let text = (!text.is_empty()).then_some(text);
        if dispatched.is_empty() {
            if let Some(text) = text {
                self.history.push(Message::assistant(text));
            }
        } else {
            self.history
                .push(Message::assistant_with_tools(text, dispatched));
            self.history
                .extend(results.into_iter().map(Message::tool_result));
        }
        halt.map(|(outcome, _)| outcome)
    }

    /// One model step, with retries. `Err` is the outcome that ends the turn.
    async fn step(&mut self, started: Instant) -> Result<StepData, TurnOutcome> {
        let mut attempt = 0u32;
        loop {
            self.set_state(SessionState::Streaming);
            let provider = Arc::clone(&self.provider);
            let mut stream = provider.stream(self.request());
            let mut text = String::new();
            let mut calls = Vec::new();
            let mut step_usage = Usage::default();
            let error = loop {
                let remaining = self
                    .config
                    .limits
                    .turn_timeout
                    .saturating_sub(self.active(started));
                let next = cancellable(
                    &self.cancel,
                    tokio::time::timeout(remaining, stream.next_event()),
                )
                .await;
                let event = match next {
                    Err(Cancelled) => {
                        self.keep_partial(text);
                        return Err(TurnOutcome::Cancelled);
                    }
                    Ok(Err(_elapsed)) => {
                        self.keep_partial(text);
                        return Err(TurnOutcome::LimitReached(LimitKind::Time));
                    }
                    Ok(Ok(event)) => event,
                };
                match event {
                    None => break AiError::Protocol("stream ended before Done".to_owned()),
                    Some(Err(e)) => break e,
                    Some(Ok(ChatEvent::TextDelta(delta))) => {
                        self.emit(SessionEvent::TextDelta(delta.clone()));
                        text.push_str(&delta);
                    }
                    Some(Ok(ChatEvent::ToolCall(call))) => calls.push(call),
                    Some(Ok(ChatEvent::Usage(usage))) => {
                        step_usage += usage;
                        self.usage += usage;
                        self.emit(SessionEvent::Usage {
                            step: step_usage,
                            session: self.usage,
                        });
                        if self.usage.total() >= self.config.limits.token_budget {
                            self.keep_partial(text);
                            return Err(TurnOutcome::LimitReached(LimitKind::Tokens));
                        }
                    }
                    Some(Ok(ChatEvent::Done { stop })) => {
                        return Ok(StepData { text, calls, stop });
                    }
                    Some(Ok(_)) => {}
                }
            };
            drop(stream);
            let retry = text.is_empty()
                && calls.is_empty()
                && attempt < self.config.limits.retry.max_retries;
            let delay = match (&error, retry) {
                (
                    AiError::RateLimited { retry_after } | AiError::Overloaded { retry_after },
                    true,
                ) => self.config.limits.retry.delay(attempt + 1, *retry_after),
                (AiError::Network(_), true) => self.config.limits.retry.delay(attempt + 1, None),
                _ => None,
            };
            let Some(delay) = delay else {
                self.keep_partial(text);
                self.add_notice(Notice::ProviderError {
                    message: error.to_string(),
                });
                return Err(TurnOutcome::Failed(SessionError::Provider(error)));
            };
            attempt += 1;
            self.add_notice(Notice::Retrying { attempt, delay });
            if cancellable(&self.cancel, tokio::time::sleep(delay))
                .await
                .is_err()
            {
                return Err(TurnOutcome::Cancelled);
            }
        }
    }

    /// Keeps the text of a step that stopped early in the transcript (not in the history).
    fn keep_partial(&mut self, text: String) {
        if !text.is_empty() {
            self.add_entry(TranscriptEntry::Assistant { text });
        }
    }
}
