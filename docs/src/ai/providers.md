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

## Anthropic

The `sqler-ai-anthropic` crate implements `AiProvider` as `AnthropicProvider` over the Messages API (`POST /v1/messages` with `stream: true`, `GET /v1/models`). It reuses the `reqwest` 0.12 workspace dependency of the Ollama provider (one TLS stack, see `docs/src/dependencies.md`), so the futures and streams must be polled inside a Tokio runtime with I/O and time enabled.

### Endpoint, key and remote

`AnthropicProvider::new(api_key, None)` uses `DEFAULT_ENDPOINT`, `https://api.anthropic.com`; `new(api_key, Some(url))` takes an `https` URL with a host, or an `http` URL whose host is `localhost` or a loopback address (a test server or a local proxy), so the key never travels in clear text. A URL with a user name, a password, a query or a fragment is `AiError::Invalid`, as is an empty key or one with characters that cannot be in a header (the key is never echoed). Redirects are not followed, the connect timeout is 10 s, a loopback endpoint is reached without an environment proxy, and a chat has no overall timeout. `GET /v1/models` pages time out after 30 s.

The caller passes the key (T11 reads it from the keychain). It is kept only as a sensitive header value and sent as `x-api-key`; it is never in a URL. `Debug` of the provider prints `api_key: "<redacted>"`. Every error text has the key replaced by `***` before it is cut to 200 characters, and `crates/sqler-ai-anthropic/tests/secrets.rs` checks that the key is in no error text, `Debug` print, `log` record or `tracing` event or span.

Anthropic is a remote provider: `is_local()` is always false, even with a loopback endpoint. `is_local` and `model_capabilities` are inherent methods, so a caller that needs them builds the concrete `AnthropicProvider`.

### Wire format

Checked on 2026-10-21 against the Anthropic documentation (Messages, streaming, Models list, errors, versions, models overview).

| Call | Use |
| ---- | --- |
| `GET /v1/models?limit=1000` | `id`, `display_name`, `max_input_tokens`; `&after_id=<last_id>` while `has_more`, at most 10 pages |
| `POST /v1/messages` | `{model, max_tokens, system?, messages, tools?, temperature?, stream: true}` |

Every request sends `x-api-key`, `anthropic-version: 2023-06-01` (the only version the versioning page lists) and `content-type: application/json`; `/v1/messages` also sends `accept: text/event-stream`. No `anthropic-beta` header is sent.

- `max_tokens` is required by the API: it is the request's `max_output_tokens`, else `DEFAULT_MAX_TOKENS` (4096), which is below the output limit of every listed model (64K for the smallest). `system` is the top-level string. Tools are `{name, description, input_schema}`. The API combines consecutive turns of one role; the provider also merges them, so several tool results are one user turn.
- User text and images keep their order (`image` blocks are base64 with the media type). An assistant message is its text followed by `tool_use` blocks with `input` as a JSON object (bad JSON is `AiError::Invalid` as the first stream item); an assistant message without blocks is dropped. A tool result is a `tool_result` block (`tool_use_id`, `content`, `is_error` when true) in a user turn. Results are not checked against earlier calls; the API does that.
- The reply is server-sent events (lines end at `\n`, `\r\n` or `\r`; events up to 8 MiB). Events are dispatched on the `type` of the JSON data; the `event:` name is ignored, and so is every unknown event, content block or delta type (`thinking`, `signature_delta`, `server_tool_use`, ...).
- Text from `text_delta` (and from a non-empty text block start) becomes `TextDelta`. The `input_json_delta` fragments of a `tool_use` block are buffered per block index and one complete `ToolCall` is emitted at `content_block_stop` (the start `input`, or `{}`, if no fragment came; malformed JSON is passed through raw). Interleaved blocks are kept apart.
- Usage: the counts of `message_start` and `message_delta` are cumulative. Input is `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`; a count that is missing keeps its last value (the documented `message_delta` carries only `output_tokens`). The provider emits the increment as `Usage` events, so the sum is the total. `message_stop` gives `Done`.

| `stop_reason` | `StopReason` |
| ------------- | ------------ |
| `end_turn`, `stop_sequence` | `EndTurn` |
| `tool_use` | `ToolUse` |
| `max_tokens`, `model_context_window_exceeded` | `MaxTokens` |
| `pause_turn`, `refusal`, anything else | `Other(reason)` |
| none | `ToolUse` if the response had a tool call, else `EndTurn` |

Differences from what the plan expected, found by the live check: the Models API now returns `max_input_tokens` (and `max_tokens` and a `capabilities` object), so `list_models` uses it for `context_window`; and no listed model lacks image input, so the vision table has no exception (see Capabilities).

### Capabilities

`capabilities()` is the provider in general: tools and vision on, JSON mode off, context 200,000 tokens. `model_capabilities(model)` is a static table with no I/O: ids starting with `claude-` have tools and vision and `max_context_tokens` of 200,000; other ids have tools only and no window. 200,000 is the smallest context of any listed model (Haiku 4.5 and Sonnet 4.5 have 200K; the 4.8, 5 and 5.5 generations have 1M), so a prompt budgeted on it always fits; `list_models` reports the exact `max_input_tokens`, and falls back to the table when it is null. `claude-3-5-haiku`, the one model the plan expected without image input, is retired.

JSON mode is off, and a request with `ResponseFormat::Json` fails with `AiError::Unsupported` as the first stream item: the Messages API has no schema-less JSON flag, and structured outputs need a schema and a beta header.

### Errors

| Case | `AiError` |
| ---- | --------- |
| HTTP 401 or 403 | `Auth("HTTP <code>")` |
| HTTP 429 | `RateLimited` with integer `Retry-After` seconds |
| HTTP 529 | `Provider("overloaded (HTTP 529): <message>")` |
| other non-2xx | `Provider("HTTP <code>: <message>")`, or `"HTTP <code>"` without a message |
| SSE `error` event `authentication_error`, `permission_error` | `Auth(<type>)` |
| SSE `error` event `rate_limit_error` | `RateLimited` without a delay |
| SSE `error` event `overloaded_error` | `Provider("overloaded: <message>")` |
| other SSE `error` event | `Provider("<type>: <message>")` |
| timeout, connection refused, body cut off, other transport failure | `Network("timed out" \| "cannot connect to the Anthropic API" \| "response interrupted" \| "request failed")` |
| malformed event, event over 8 MiB, end of stream before `message_stop` | `Protocol` |

An overload is a `Provider` error, not `RateLimited`: it is the service, not the caller's quota, and the UI should not say "rate limited". A dedicated `AiError::Overloaded` is a follow-up change to `sqler-ai-api`. The message is the API's `error.message`; it never holds the URL, a header or the prompt. After `Done` or any `Err` the stream returns `None`. Dropping the stream drops the HTTP response and closes the connection.

Tests run against a loopback mock server with fixtures hand-written from the documentation's example transcripts. One real streamed call with tools on a real key is pending (maintainer).

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
