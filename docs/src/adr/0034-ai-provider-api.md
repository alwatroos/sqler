# 0034 AI provider API: boxed futures, event stream, cancel by drop

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

M5 adds AI features that talk to several providers (`Ollama`, `Anthropic`, `OpenAI` and others, T3 to T6). The UI and the agent loop must work against one contract and not know which provider answers. The contract lives in `sqler-ai-api` (L1), which must not depend on a runtime, an HTTP client or `sqler-driver-api`. AGENTS.md §10.2 requires an ADR before a new public API.

## Decision drivers

- Dyn-compatible: callers hold `Arc<dyn AiProvider>` and `Box<dyn ChatStream>`.
- Streaming: text, tool calls and token usage arrive as events.
- Cancellable: the user can stop a response at any time.
- Minimal dependencies and no abstraction leak (a provider SDK's types must not appear in the contract).
- Testable without a runtime or a network.

## Considered options

1. `async-trait` plus a `BoxStream` of events, as written in prompt §7.1.
2. Boxed futures (`BoxFuture`) plus a crate-owned `ChatStream` trait with `next_event()`.
3. An existing LLM crate (`rig`, `genai`, `async-openai`).

## Decision outcome

Chosen: option 2, in the style of `sqler-driver-api` (ADR-0004).

- `AiProvider::list_models` returns a `BoxFuture`. `AiProvider::stream` is not async: it takes `&self` and returns `Box<dyn ChatStream + '_>`. A request-setup failure (auth, network) is the first `Err` item of the stream.
- `ChatStream::next_event` yields normalised `ChatEvent`s: `TextDelta`, complete `ToolCall` (providers buffer argument fragments), `Usage` and one final `Done { stop }`, then `None` forever. An `Err` ends the stream and is followed by `None`. `collect` enforces this protocol and builds a `ChatResponse`.
- Cancel by drop: dropping the stream box is the only way to cancel. A provider ties the lifetime of its HTTP request to the stream, so a drop aborts the request and no event or error comes after it.
- The system prompt is the `ChatRequest::system` field and not a `Role::System` message. It maps to the top-level field of `Anthropic` and to a first message for the others.
- Tool call arguments are raw JSON text (`ToolCall::arguments`). A model's malformed JSON reaches the tool layer (T7) and is not a stream error; `parse_arguments` returns `AiError::Invalid`.
- Usage is reported as increments: each `ChatEvent::Usage` adds to the total. A provider that receives cumulative counts sends the difference.
- Errors are a typed `AiError` (`thiserror`): `Auth`, `RateLimited`, `Network`, `Provider`, `Cancelled`, `Unsupported`, `Invalid`, `Protocol`. The strings never carry an API key, request headers or prompt text.
- `Capabilities` (tools, JSON mode, vision, max context) are provider-level, as in §7.1. The per-model context size is `ModelInfo::context_window`.
- Public structs and enums are `#[non_exhaustive]` and have constructors. `Message` has private fields and only validating constructors, so every message is valid (a user message never holds a tool part).
- A `ScriptedProvider` mock (feature `test-support`, also compiled under `cfg(test)`) replays scripts for the tests of every consumer.

## Consequences

- Good: dyn-compatible, runtime-agnostic, no HTTP client in L1, no new crate (`thiserror` and `serde_json` are already in the tree), and tests need no executor.
- Good: one event model for all providers; the agent loop and the UI see no provider type.
- Bad: one heap allocation per future; negligible next to a network round-trip.
- Bad: `#[non_exhaustive]` forces downstream code to use constructors and wildcard match arms.
- Bad: each provider must buffer tool-call fragments and normalise usage itself; the provider page has a checklist and the mock helps test it.
- Neutral: JSON validation of tool arguments moves to the tool layer.

## Pros and cons of the options

- `async-trait` plus `BoxStream`: matches the prompt text, but adds a proc-macro dependency for what plain boxing does, and `futures-core::Stream` needs `StreamExt` for `next()`. Rejected.
- Boxed futures plus a crate-owned stream trait: chosen, see above.
- `rig`, `genai`, `async-openai`: they bring their own HTTP stack and message model, which is an abstraction leak (§6), and pull a large dependency tree into L1. Rejected.

## Addendum: AiError::Overloaded

`AiError` gets a variant `Overloaded { retry_after: Option<Duration> }` (display text `provider overloaded`; the enum is `non_exhaustive`, so this is additive). It carries no server message, like `RateLimited`. The Anthropic provider maps HTTP 529 (with the `Retry-After` header clamped to 1 hour) and a stream `overloaded_error` event (no `retry_after`) to it. Other providers keep mapping their overload status (such as HTTP 503) to `Provider`. A client that matches `AiError` must keep a wildcard arm.
