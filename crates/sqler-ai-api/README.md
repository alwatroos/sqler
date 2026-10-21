# sqler-ai-api

The `AiProvider` trait, message and tool types, streaming and token accounting.

Status: M5 (provider contract, ADR-0034)

Layer: L1

## Usage

A provider implements `AiProvider`. `stream` returns a `ChatStream` of
normalised events (`TextDelta`, `ToolCall`, `Usage`, then one `Done`). Dropping
the stream cancels the request. `collect` reads a stream into a `ChatResponse`:

```rust,ignore
let mut stream = provider.stream(ChatRequest::new("model", vec![Message::user("hi")]));
let response = sqler_ai_api::collect(stream.as_mut()).await?;
```

## Feature `test-support`

Compiles `mock::ScriptedProvider`, which replays scripted events. Downstream
crates enable it in `[dev-dependencies]`:

```toml
sqler-ai-api = { workspace = true, features = ["test-support"] }
```

See [AGENTS.md](../../AGENTS.md) for the project handbook.
