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

## OpenAI and compatible endpoints

The `sqler-ai-openai` crate implements `AiProvider` as `OpenAiProvider` over the **Chat Completions** API, for OpenAI and for any server that speaks it (LM Studio, vLLM, the `llama.cpp` server, Ollama's `/v1`, a gateway). It reuses the `reqwest` 0.12 stack of the Ollama provider (no new crate), so the futures and streams must be polled inside a Tokio runtime with I/O and time enabled.

Chat Completions is primary, not the Responses API. Every OpenAI-compatible server implements it, so one wire format covers both targets; OpenAI still supports it and the API reference does not mark it deprecated (it recommends Responses for new projects). Using Responses for OpenAI and Chat Completions for the rest would need two wire formats and two test suites, and compatible servers support Responses only partly.

### Base URL, flavours and locality

`OpenAiProvider::new(base_url, api_key)`. `None` for the base URL selects `DEFAULT_BASE_URL`, `https://api.openai.com/v1`. The base URL includes the version path: the provider appends `/models` and `/chat/completions`, so a local server is `http://localhost:1234/v1`. It must be an `http` or `https` URL with a host and without user info, query or fragment (anything else is `AiError::Invalid`; the messages never quote the URL or the key). Classic Azure OpenAI (`api-key` header, `?api-version=`, `/deployments/{id}`) is not supported; a gateway that takes the plain OpenAI path with a `Bearer` key works.

The host `api.openai.com` selects `Flavor::OpenAi`; every other host is `Flavor::Compatible`. The flavours differ in two things: the output cap is `max_completion_tokens` for `OpenAi` and `max_tokens` for `Compatible`, and the default capabilities (below). `with_capabilities(caps)` overrides the defaults for a server that differs.

`is_local()` is true when the host is `localhost` (a trailing `.` and case are ignored) or a loopback address (`127.0.0.0/8`, `::1`); every other host, including a LAN address, is remote. A local host is reached without an environment proxy. Redirects are not followed (the key never goes to a second host), the connect timeout is 10 s, `/models` times out after 30 s and a chat has no overall timeout.

### API key

The key is optional: a local server may need none, and an empty key counts as none. With a key every request carries `Authorization: Bearer <key>`, held as a sensitive header value; without one no `Authorization` header is sent. A key over `http://` to a host that is not local is refused with the config layer's text, "an API key needs https:// unless the host is local". A key with characters not allowed in a header (anything but visible ASCII) is `Invalid`. The key is not in `Debug` (`api_key: "***"`), not in any error (server error text has every occurrence replaced by `***`), and the crate has no logging dependency. It is not zeroized in memory; it also lives in the keychain layer.

### Wire format

Checked on 2026-10-21 against the official OpenAPI description (`openai/openai-openapi`, version 2.3.0) and the OpenAI error-codes guide, and against the `llama.cpp` server README, the vLLM source and the LM Studio docs.

| Call | Use |
| ---- | --- |
| `GET {base}/models` | `{data:[{id, ...}]}`, sorted by `id`, no filtering (embedding and speech models are listed) |
| `POST {base}/chat/completions` | `{model, messages, tools?, stream: true, stream_options: {include_usage: true}, temperature?, response_format?: {type: "json_object"}, max_completion_tokens? \| max_tokens?}` |

- `context_window` is the first present of `context_length`, `max_model_len` (vLLM) and `meta.n_ctx_train` (`llama.cpp`); OpenAI sends none, so it stays `None`. Values above `u32::MAX` are ignored.
- The system prompt is the first message with role `system`. A user message is a string, or an array of `text` and `image_url` parts (`data:<mime>;base64,...`) when it has an image. An assistant message has `content` (null when only tool calls) and `tool_calls` with `arguments` as the raw text (empty becomes `{}`; the provider does not validate it). Each tool result is a `tool` message with `tool_call_id`, prefixed with `error: ` when the tool failed. `tool_choice`, `n`, `parallel_tool_calls` and `user` are not sent.
- The reply is server-sent events: lines split on `\n`, `data:` lines joined with `\n`, a blank line ends an event, `:` comments (keep-alives) and `id:`/`retry:` are ignored, a line or event over 8 MiB is `Protocol`. Only `choices[0]` is read. `delta.content` and `delta.refusal` are `TextDelta`s as they arrive. Tool-call fragments are collected per `delta.tool_calls[].index` (a fragment without `index` goes to the slot with the same `id`, or the last slot) and each call is emitted complete when `finish_reason` arrives, in index order; a missing `id` becomes `call_<index>`, a missing name is `Protocol`. The final chunk with empty `choices` and `usage` becomes one `Usage` before `Done`; without it no `Usage` is sent. `Done` follows `data: [DONE]`; a clean end of stream without `[DONE]` is accepted only after a `finish_reason` (some servers omit it), otherwise it is `Protocol`.
- `finish_reason` `stop` (or none) is `ToolUse` if the response had a tool call, else `EndTurn`; `tool_calls` and `function_call` are `ToolUse`; `length` is `MaxTokens`; anything else (for example `content_filter`) is `Other`.
- A `data:` payload with an `error` member, or an `event: error`, is `Provider`.

What the live check found. Confirmed: `stream_options.include_usage` and a last chunk with empty `choices` before `[DONE]`; `tool_calls[].index` (required) with `id` and `name` on the first fragment only; `max_completion_tokens` (and `max_tokens` marked deprecated and unusable with o-series models); the `finish_reason` values; the error object `{code, message, param, type}`; `Retry-After` and `Retry-After-Ms` headers; a mid-stream `error` data frame. `llama.cpp` serves `/v1/models` with `meta.n_ctx_train`; vLLM accepts both `max_tokens` (deprecated) and `max_completion_tokens`. Not confirmable from public documents in this check: the exact 401 text that quotes a masked key (the provider never uses the server text for 401 and 403) and the `insufficient_quota` code text in the error-codes guide (the mapping follows the field the API returns). Fixtures are hand-written from these documents, not captured.

### Capabilities

`capabilities()` returns the `with_capabilities` override if set. Otherwise `OpenAi` reports tools, JSON mode and vision on; `Compatible` reports tools and JSON mode on and vision off (unknown). Images are still sent if the caller adds them. No context limit is reported.

### Errors

| Case | `AiError` |
| ---- | --------- |
| HTTP 401 or 403 | `Auth("HTTP <code>")` (fixed text; OpenAI's 401 message quotes a masked key) |
| HTTP 429 with `error.code` `insufficient_quota` | `Provider("HTTP 429: <message>")` |
| other HTTP 429 | `RateLimited`, wait from `retry-after-ms` if present, else `Retry-After` seconds |
| HTTP 400 with code `unsupported_parameter` or `unsupported_value`, or text "does not support" or "not supported" | `Unsupported` |
| other non-2xx | `Provider("HTTP <code>: <message>")`, or `Provider("HTTP <code>")` without a message |
| timeout, connection refused, body cut off, other transport failure | `Network("timed out" \| "cannot connect to the server" \| "response interrupted" \| "request failed")` |
| malformed `data`, oversize line or event, end of stream before `[DONE]` or a `finish_reason` | `Protocol` |
| `error` payload in the stream | `Provider` |

The message is the first of `error.message`, `error` as a string, `message` or `detail`, with the key replaced by `***` and cut to 200 characters. A server that rejects `stream_options` or `max_completion_tokens` answers 400, which is `Provider` or `Unsupported`; there is no retry. Reasoning models (o-series, gpt-5) reject a `temperature` other than the default, which comes back as `Unsupported`; the caller decides whether to leave the temperature out. After `Done` or any `Err` the stream returns `None`. Dropping the stream drops the HTTP response and closes the connection.

Tests run against a loopback mock server with hand-written SSE fixtures. Pending (maintainer): one real OpenAI call, and one run against a local compatible server (LM Studio or the `llama.cpp` server).
