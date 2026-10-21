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

## Ollama

The `sqler-ai-ollama` crate implements `AiProvider` as `OllamaProvider` over Ollama's native API. It uses `reqwest` 0.12 on rustls (see `docs/src/dependencies.md`), so the futures and streams must be polled inside a Tokio runtime with I/O and time enabled.

### Endpoint and locality

`OllamaProvider::new(None)` uses `DEFAULT_ENDPOINT`, `http://127.0.0.1:11434`; `new(Some(url))` takes any `http` or `https` URL with a host (anything else is `AiError::Invalid`, as is a URL with a user name or password, so a secret never reaches a `Debug` print). `endpoint()` returns the URL as configured, without a trailing `/`. Redirects are not followed, the connect timeout is 10 s, and a chat has no overall timeout. `/api/tags` and `/api/show` time out after 30 s.

`is_local()` is decided by the endpoint host: `localhost` (ASCII case-insensitive) or a loopback address (`127.0.0.0/8`, `::1`) is local; every other host, including a LAN address, is remote. Use it for the privacy indicator. A local endpoint is also reached without an environment proxy. `is_local` and `model_capabilities` are inherent methods, so a caller that needs them builds the concrete `OllamaProvider`.

### Wire format

Checked on 2026-10-21 against the Ollama API document (`docs/api.md` in the Ollama repository).

| Call | Use |
| ---- | --- |
| `GET /api/tags` | the model names |
| `POST /api/show` `{"model"}` | `capabilities` (`tools`, `vision`, ...) and the first `*.context_length` of `model_info` |
| `POST /api/chat` | `{model, messages, tools?, format?, options?, stream: true}` |

- The system prompt is the first message with role `system`. Images are base64 in `images`. Assistant tool calls are sent with `arguments` as a JSON object (bad JSON is `AiError::Invalid` as the first stream item). A tool result is a `tool` message with `tool_name`, taken from the latest earlier assistant tool call with the same `call_id` (no match is `Invalid`); a failed result is prefixed with `error: `. JSON mode is `format: "json"`; `temperature` and `max_output_tokens` go to `options.temperature` and `options.num_predict`. `think` and `keep_alive` are not sent.
- The reply is NDJSON, one object per line, split on `\n` bytes. Tool calls arrive complete. A call without an `id` (the documented shape has none) gets `call_<n>`, `n` counting the calls of the response from 0. The final `done: true` line gives `done_reason` and the counts `prompt_eval_count` and `eval_count`, which become one `Usage` event before `Done`.
- `done_reason` `length` is `MaxTokens`; `stop` or none is `ToolUse` if the response had a tool call, else `EndTurn`; anything else is `Other`.

No deviation from the document was found. The optional fields (`tool_calls[].id`, `tool_name`, `capabilities`) are read as optional, because the document does not show `capabilities` in the `/api/show` example although current servers send it.

### Capabilities

`capabilities()` is the provider in general: tools, JSON mode and vision on, no context limit. `model_capabilities(model)` asks `/api/show`: `tools` and `vision` are true when the `capabilities` list contains them, `json_mode` is always true, and `max_context_tokens` is the first `*.context_length` of `model_info`. When the server sends no `capabilities` list the defaults are tools on, JSON mode on, vision off. `list_models` calls `show` once per model, one after another, to fill `context_window`; a failed `show` leaves it `None` and is not an error.

### Errors

| Case | `AiError` |
| ---- | --------- |
| HTTP 401 or 403 | `Auth` |
| HTTP 429 | `RateLimited` with `Retry-After` seconds |
| HTTP 400 with "does not support" (a model without tools) | `Unsupported` |
| other non-2xx | `Provider("HTTP <code>: <error>")` |
| timeout, connection refused, body cut off, other transport failure | `Network("timed out" \| "cannot connect to the Ollama server" \| "response interrupted" \| "request failed")` |
| malformed line, line over 8 MiB, end of stream without `done` | `Protocol` |
| `{"error": "..."}` line | `Provider` |

The text comes from Ollama's `error` field, cut to 200 characters; it never holds the URL, a header or the prompt. After `Done` or any `Err` the stream returns `None`. Dropping the stream drops the HTTP response and closes the connection.

Tests run against a loopback mock server with hand-written fixtures. A real run against Ollama with one tool-capable model is pending (maintainer).

