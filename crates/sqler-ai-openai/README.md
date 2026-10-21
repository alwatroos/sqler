# sqler-ai-openai

`OpenAI` and any OpenAI-compatible endpoint implementation of the `AiProvider` trait.

Status: implemented (M5)

Layer: L2

```rust
use sqler_ai_openai::OpenAiProvider;

// The official API: https://api.openai.com/v1, with a key.
let openai = OpenAiProvider::new(None, Some("sk-..."))?;

// A local compatible server (LM Studio, vLLM, llama.cpp): no key needed.
let local = OpenAiProvider::new(Some("http://localhost:1234/v1"), None)?;
assert!(local.is_local());
```

Use it through `sqler_ai_api::AiProvider`: `list_models`, `stream` (server-sent events, parallel tool calls, usage)
and `capabilities`. `with_capabilities`, `base_url`, `flavor` and `is_local` are inherent methods.

## API choice

**Chat Completions** (`GET {base}/models`, `POST {base}/chat/completions`) is the primary API, not the Responses API.
Every OpenAI-compatible server implements Chat Completions, so one wire format covers OpenAI and the compatible
servers, and OpenAI still supports it. Using Responses for OpenAI would need a second wire format and a second test
suite, and compatible servers support Responses only partly.

## Base URL, key and scope

- The base URL includes the version path (`https://api.openai.com/v1`, `http://localhost:1234/v1`). The host
  `api.openai.com` is the `OpenAi` flavour (`max_completion_tokens`, vision on); any other host is `Compatible`
  (`max_tokens`, vision off unless `with_capabilities` says otherwise).
- The key is optional. It is sent as a `Bearer` token, never printed or put in an error, and refused over `http://`
  to a host that is not local.
- Classic Azure OpenAI (`api-key` header, `?api-version=` query, `/deployments/{id}`) is out of scope; gateways that
  take the plain OpenAI path and a `Bearer` key work.

The HTTP client is `reqwest` on rustls, so the futures and streams must be polled inside a Tokio runtime with
I/O and time enabled. Dropping a stream closes the connection. Tests use a loopback mock server with hand-written
fixtures; no real API and no key. Real runs are pending (maintainer).

See [AGENTS.md](../../AGENTS.md) for the project handbook and `docs/src/ai/providers.md` for the OpenAI section.
