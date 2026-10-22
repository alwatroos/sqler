# sqler-ai-anthropic

`Anthropic` implementation of the `AiProvider` trait.

Status: implemented (M5)

Layer: L2

```rust
use sqler_ai_anthropic::AnthropicProvider;

// The caller passes the API key (T11 reads it from the keychain). None selects
// https://api.anthropic.com.
let provider = AnthropicProvider::new(&api_key, None)?;
assert!(!provider.is_local());
```

Use it through `sqler_ai_api::AiProvider`: `list_models` (paged `GET /v1/models`), `stream` (Messages API over
server-sent events: text, complete tool calls, usage, stop reasons) and `capabilities`.
`model_capabilities(model)` (a static table) and `is_local()` (always false: Anthropic is remote) are inherent methods.

The key is kept only as a sensitive header value; `Debug` prints `"<redacted>"` and every error text has the key
replaced by `***`. `http` endpoints are accepted only for loopback hosts. JSON mode is not available
(`AiError::Unsupported`); a request without `max_output_tokens` sends `max_tokens: 4096`.

The HTTP client is `reqwest` on rustls, so the futures and streams must be polled inside a Tokio runtime with
I/O and time enabled. Dropping a stream closes the connection. Tests use a loopback mock server; no real API call
and no key. A real run is pending (maintainer).

See [AGENTS.md](../../AGENTS.md) for the project handbook and `docs/src/ai/providers.md` for the Anthropic section.
