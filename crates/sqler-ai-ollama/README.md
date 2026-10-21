# sqler-ai-ollama

`Ollama` implementation of the `AiProvider` trait.

Status: implemented (M5)

Layer: L2

```rust
use sqler_ai_ollama::OllamaProvider;

// None selects http://127.0.0.1:11434.
let provider = OllamaProvider::new(None)?;
assert!(provider.is_local());
```

Use it through `sqler_ai_api::AiProvider`: `list_models`, `stream` (NDJSON, tool calls, usage) and `capabilities`.
`model_capabilities(model)` and `is_local()` are inherent methods.

The HTTP client is `reqwest` on rustls, so the futures and streams must be polled inside a Tokio runtime with
I/O and time enabled. Dropping a stream closes the connection. Tests use a loopback mock server; no real Ollama.

See [AGENTS.md](../../AGENTS.md) for the project handbook and `docs/src/ai/providers.md` for the Ollama section.
