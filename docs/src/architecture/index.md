# Architecture

SQLer is a Cargo workspace. It is layered, and dependencies flow downward only: a crate may depend
only on crates in lower layers.

```text
L0 core
L1 driver-api, ai-api, sql, plugin-api, config
L2 driver-*, ai-ollama/anthropic/openai/mistral, catalog, completion, export
L3 ai-tools, plugin-host
L4 mcp
L5 acp
L6 ui, cli
```

Planned topics:

- Overview: planned (M1)
- Data flow: planned (M2)
- Threading model: planned (M1)
- Per-crate pages: added with each crate's implementation

The full crate table is in `AGENTS.md` at the repository root. Decisions behind the architecture
are recorded in the [architecture decisions](../adr/index.md).
