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

## Mistral

The `sqler-ai-mistral` crate implements `AiProvider` as `MistralProvider` over the Mistral API (`/v1/models`, `/v1/chat/completions`). It reuses the `reqwest` 0.12 stack of the Ollama provider (no new crate), so the futures and streams must be polled inside a Tokio runtime with I/O and time enabled. The crate is not linked into the app yet: the `ai-mistral` feature of `sqler-app` comes with the app wiring (T11).

The chat shape is close to OpenAI's, but the crate is its own copy: layering forbids one provider crate depending on another, and Mistral differs in the places listed under Wire format.

### Endpoint and key

`MistralProvider::new(api_key, endpoint)`. `None` selects `DEFAULT_ENDPOINT`, `https://api.mistral.ai`; the provider appends `/v1/...`, so a gateway with a path prefix works (`https://gw.example.com/mistral`). The key is required: it is trimmed, and an empty key or a byte that is not visible ASCII is `AiError::Invalid`. The endpoint is `Invalid` when it is over 2048 bytes, has whitespace or a control character, contains `@`, `?` or `#`, does not parse, is not `http` or `https`, has no host, or has user info, a query or a fragment. `http` is allowed only for a loopback host (`localhost`, `127.0.0.0/8`, `::1`), so the key never travels in clear text; a loopback host is also reached without an environment proxy. The messages never quote the key or the URL.

Every request carries `Authorization: Bearer <key>`, held as a sensitive header value. The key is not in `Debug` (`api_key: "***"`), not in any error (server text has every occurrence replaced by `***` before it is cut to 200 characters, so a cut cannot leave half of it), and the crate never logs; `tests/secrets.rs` captures `log` and `tracing` output over every failure path and checks it. A JSON parse error never quotes the offending value either, only its category and position. Redirects are not followed, so the key never goes to a second host. The key is not zeroized in memory.

`is_local()` is always false: Mistral is a remote provider, even when the endpoint is a loopback test server. The connect timeout is 10 s, `/v1/models` calls time out after 30 s, and a chat has no overall timeout but fails after 120 s of silence (`with_read_timeout` changes that, which also covers a stream that stalls).

### Wire format

Checked on 2026-10-21 against the Mistral OpenAPI document (`https://docs.mistral.ai/openapi.yaml`), the API reference and the function-calling page. Nothing was sent to the API and no key was used.

| Call | Use |
| ---- | --- |
| `GET /v1/models` | `{data:[{id, name?, max_context_length?, capabilities?, ...}]}` |
| `GET /v1/models/{id}` | one such card |
| `POST /v1/chat/completions` | `{model, messages, tools?, temperature?, max_tokens?, response_format?: {type: "json_object"}, stream: true}` |

- **No `stream_options`**: the field does not exist in the schema, and usage arrives in the last chunk on its own. The output cap is `max_tokens`.
- The system prompt is the first message with role `system`. A user message is a string, or an array of `text` and `image_url` parts when it has an image; the `image_url` is the data URL as a plain string (`data:<mime>;base64,...`), one of the two forms the schema allows. An assistant message has `content` (`""` rather than `null` when it only has tool calls) and `tool_calls` with `arguments` as text (empty becomes `{}`). A tool result is a `tool` message with `content`, `tool_call_id` and `name`, the name of the earlier assistant call with the same id (left out when there is none); a failed result is prefixed with `error: `.
- **Tool-call ids** must be nine ASCII letters or digits. An id that matches is sent unchanged; any other id (`call_0`, an id from another provider) becomes a stable nine-character id (FNV-1a 64 of the id in base 62), the same on the assistant call and on its result, so pairs stay matched. This rule is not in the OpenAPI document (the schema types `id` as a plain string); it is the rule the API is reported to enforce, and the mapping is harmless for ids that already match. Re-check it in the real run.
- The reply is server-sent events: `data:` lines (CR, LF and CRLF line ends, a leading byte order mark ignored, a line or event over 8 MiB is `Protocol`), ended by `data: [DONE]`. Only `choices[0]` is read. `delta.content` is a string or an array of chunks; only `{"type":"text"}` items become `TextDelta`, `thinking` and other types are skipped.
- `delta.tool_calls[]` usually brings a whole call in one fragment, with `id`, `index` (default 0) and `function.arguments` as a string **or a JSON object** (an object is sent on as compact JSON text). Parallel calls repeat `index` 0 with distinct ids, so a fragment with a new non-empty `id` opens a new call whatever its `index`; a fragment without an id goes to the call that last used its `index`, else to the last call. Calls are emitted complete when `finish_reason` arrives, in the order they opened; a missing id becomes `call<n>` and a missing name is `Protocol("tool call without a name")`.
- `finish_reason`: `stop` (or none) is `ToolUse` if the response had a tool call, else `EndTurn`; `tool_calls` is `ToolUse`; `length` and `model_length` are `MaxTokens`; `error` is `Provider("the model stopped with an error")`, not a success; anything else is `Other`. A tool call whose arguments are not a complete JSON object when the response ends with `length`, `model_length` or no finish reason is `Protocol("tool call cut off by the token limit")`; with any other finish reason, arguments that are not a JSON object (also in fragments after the first `finish_reason`) are `Protocol("tool call arguments are not a JSON object")`; truncated or invalid arguments are never emitted. A tool call id sent twice on different indexes is two calls, the second with a made-up id. Made-up ids (`call<n>`) are numbered from a counter shared by all streams of the process.
- `usage` (`prompt_tokens`, `completion_tokens`) arrives in the final chunk and becomes one `Usage` event before `Done`. A chunk with `"object":"error"` or an `error` member is a `Provider` error.

Not confirmed by the document: the error body shapes `{"object":"error","message","type","code"}` and `{"message":"Unauthorized"}` (only the 422 validation shape is in the schema), so the error text is read from several places, in this order: `message` as a string; `message` as an object (`detail[0].msg`, else compact JSON); `error.message`; `error` as a string; `detail` as a string or `detail[0].msg`. Also not in the document: the nine-character id rule above. `model_length` is in the non-streaming enum only and is accepted anyway. No other deviation from the spec of this crate was found.

### Capabilities

`capabilities()` is the provider in general: tools, JSON mode and vision on, no context limit. `list_models` keeps a model unless `capabilities.completion_chat` is `false` (embedding and moderation models drop out), uses `name` as the display name when it is non-empty and differs from the id, takes `max_context_length` as the context window (a value above `u32::MAX` gives none) and sorts by id. `model_capabilities(model)` asks `GET /v1/models/{id}`: tools are `function_calling`, vision is `vision`, JSON mode is always true and `max_context_tokens` is `max_context_length`. A card without a `capabilities` object gives tools on and vision off. The model id is added as one URL path segment, so `/`, `?` and `%` cannot change the path; an empty id, `.` and `..` are `Invalid`.

### Errors

| Case | `AiError` |
| ---- | --------- |
| HTTP 401 or 403 | `Auth("HTTP <code>")` (fixed text; the server text is ignored) |
| HTTP 429 | `RateLimited`; `Retry-After` is read as whole seconds only and clamped to 3600 s (a date, a negative or float value or junk gives `None`) |
| HTTP 400 or 422 with "not supported", "does not support" or "not enabled" | `Unsupported(<message>)` |
| other non-2xx | `Provider("HTTP <code>: <message>")`, or `Provider("HTTP <code>")` without a message |
| timeout, connection refused, body cut off, other transport failure | `Network("timed out" \| "cannot connect to the server" \| "response interrupted" \| "request failed")` |
| malformed `data`, oversize line or event, end of stream before `[DONE]` or a finish reason, tool call cut off or without a name | `Protocol` |
| error payload in the stream, `finish_reason` `error` | `Provider` |

The message is cut to 200 characters after the key is replaced by `***`; it never holds the URL, a header or the prompt. Error bodies are read up to 64 KiB and model lists up to 16 MiB. After `Done` or any `Err` the stream returns `None`. Dropping the stream drops the HTTP response and closes the connection.

### Tests

Unit tests sit in each module; the integration tests in `crates/sqler-ai-mistral/tests/` run against a loopback mock server with hand-written transcripts (text with a `thinking` chunk, parallel tool calls with object arguments, a cut-off tool call, an error chunk, a byte-order-marked stream), including chunk sizes 1, 7 and 255, the secret-capture test and the proxy-bypass test. No test uses the network or a real key. Pending (maintainer): one real Mistral call with a key (it costs money, so neither CI nor an agent runs it); it should also confirm the nine-character id rule and the error body shapes.
