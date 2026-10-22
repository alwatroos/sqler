# sqler-ai-mistral

`Mistral` implementation of the `AiProvider` trait.

Status: implemented (M5)

Layer: L2

```rust
use sqler_ai_mistral::MistralProvider;

// The caller passes the API key (T11 reads it from the keychain). None selects
// https://api.mistral.ai.
let provider = MistralProvider::new(&api_key, None)?;
assert!(!provider.is_local());
```

Use it through `sqler_ai_api::AiProvider`: `list_models` (`GET /v1/models`, chat-capable models only, sorted by id, with
the context window), `stream` (`POST /v1/chat/completions` over server-sent events: text, complete tool calls, usage,
stop reasons) and `capabilities`. `model_capabilities(model)` (`GET /v1/models/{id}`), `with_read_timeout` and
`is_local()` (always false: Mistral is remote) are inherent methods.

`new(api_key, endpoint)` trims the key and refuses an empty key or one with a byte that is not visible ASCII. The
endpoint must be an `http` or `https` URL with a host, at most 2048 bytes, without whitespace, control characters,
user info, query or fragment; `http` is accepted only for a loopback host, so the key never travels in clear text. The
messages never quote the key or the URL. The key is kept only as a sensitive header value; `Debug` prints
`api_key: "***"` and every error text has the key replaced by `***`.

Behaviour worth knowing: the output cap is sent as `max_tokens`, there is no `stream_options`, tool-call ids that do not
match Mistral's nine-character alphanumeric rule are mapped to a stable id that does (call and result stay paired),
`thinking` chunks are dropped, a tool call cut off by the token limit is a `Protocol` error (truncated arguments are
never emitted), and a `finish_reason` of `error` is a `Provider` error. The read timeout is 120 s of silence, also
while streaming.

The HTTP client is `reqwest` on rustls, so the futures and streams must be polled inside a Tokio runtime with I/O and
time enabled. Dropping a stream closes the connection. Tests use a loopback mock server with hand-written transcripts;
no real API call and no key. A real run against Mistral is pending (maintainer; it needs a key and costs money, so
neither CI nor an agent runs it).

See [AGENTS.md](../../AGENTS.md) for the project handbook and `docs/src/ai/providers.md` for the Mistral section.
