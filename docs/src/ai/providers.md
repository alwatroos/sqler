# AI providers

The provider contract lives in the `sqler-ai-api` crate (L1). It has no HTTP client and no async runtime. The design is recorded in [ADR-0034](../adr/0034-ai-provider-api.md).

## The traits

```rust
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait AiProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>>;
    fn stream(&self, request: ChatRequest) -> Box<dyn ChatStream + '_>;
    fn capabilities(&self) -> Capabilities;
}

pub trait ChatStream: Send {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>>;
}
```

`stream` is not async. A request-setup failure (auth, network) is the first `Err` item. `Capabilities` has `tools`, `json_mode`, `vision` and `max_context_tokens`; the context size of a single model is `ModelInfo::context_window`.

## Requests

`ChatRequest::new(model, messages)` plus the chainable `with_system`, `with_tools`, `with_max_output_tokens`, `with_temperature` and `with_response_format` (`ResponseFormat::Json` means JSON mode). The system prompt is a request field, not a role. Roles are `User`, `Assistant` and `Tool`.

`Message` has private fields. Build one with `user`, `user_parts` (only `Text` and `Image` parts; otherwise `AiError::Invalid`), `assistant`, `assistant_with_tools` or `tool_result`. A `ToolDefinition` has a name, a description and a JSON Schema in `parameters`. A `ToolCall` carries its `arguments` as raw JSON text; `parse_arguments` returns `AiError::Invalid` on bad JSON. A `ToolResult` is built with `ok` or `error`.

## Event protocol

A stream yields any number of `TextDelta`, `ToolCall` and `Usage` events in any order, then exactly one `Done { stop }`, then `None` forever. `StopReason` is `EndTurn`, `ToolUse`, `MaxTokens` or `Other`. An `Err` ends the stream and is followed by `None`.

Tool calls arrive complete: the provider buffers argument fragments. `collect(&mut stream)` concatenates the text, keeps tool calls in order, sums the usage and stops at `Done`. It returns `AiError::Protocol("stream ended before Done")` if the stream ends early and passes stream errors through unchanged.

Cancel by dropping the stream. The provider must tie the lifetime of the HTTP request to the stream, so a drop aborts the request and no event or error follows.

## Errors

| Variant | Meaning |
| ------- | ------- |
| `Auth` | authentication failed |
| `RateLimited` | rate limited; optional `retry_after` |
| `Network` | transport failure |
| `Provider` | the provider reported an error |
| `Cancelled` | request cancelled |
| `Unsupported` | the provider or model lacks a feature |
| `Invalid` | the request or tool arguments are invalid |
| `Protocol` | the provider broke the stream protocol |

The strings never carry an API key, request headers or prompt text.

## Token accounting

Each `ChatEvent::Usage` is an increment (`Usage { input_tokens, output_tokens }`). A provider that receives cumulative counts sends the difference. `Usage` adds with saturation, and `collect` returns the total in `ChatResponse::usage`.

## Testing with the mock

Enable the feature in the consumer's `[dev-dependencies]`:

```toml
sqler-ai-api = { workspace = true, features = ["test-support"] }
```

`sqler_ai_api::mock::ScriptedProvider` replays one script per `stream` call (FIFO). It does not add `Done` itself. `requests()` returns every request received, and `open_streams()` counts live streams, which checks cancel by drop. With no script left, the stream yields `Err(AiError::Provider("mock: no script left"))`.

## Adding a provider (T3 to T6)

1. Implement `AiProvider` in its own crate; map the provider's wire format to `ChatEvent` only.
2. Return setup failures as the first `Err` item of the stream.
3. Buffer tool-call argument fragments and emit one complete `ToolCall`.
4. Emit `Usage` as increments, and exactly one `Done`.
5. Tie the HTTP request to the stream so a drop cancels it.
6. Map HTTP errors to `AiError` without secrets, headers or prompt text.
7. Report honest `Capabilities` and `ModelInfo::context_window`.
8. Test the protocol with a local server and check the result with `collect`.
