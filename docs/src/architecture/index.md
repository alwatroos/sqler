# Architecture

SQLer is a Cargo workspace. It is layered, and dependencies flow downward only: a crate may depend
only on crates in lower layers.

```text
L0 core
L1 driver-api, ai-api, sql, plugin-api, config, tunnel
L2 driver-*, ai-ollama/anthropic/openai/mistral, catalog, completion, export
L3 ai-tools, plugin-host
L4 mcp
L5 acp
L6 ui, cli
L7 app
```

Pages:

- [Core and driver API](core-and-driver-api.md): domain types, the `Driver` and `Introspect` traits
- [Catalog](catalog.md): the lazy schema cache and its background refresh
- [SQL text services](sql.md): splitting, cursor lookup, placeholders, classification and formatting on one lexer
- [Completion](completion.md): the pure intellisense engine and its `SchemaSource`
- [UI](ui.md): workspace state, the tokio bridge, the connect pipeline and the object browser

Planned topics:

- Overview: planned
- Per-crate pages: added with each crate's implementation

The full crate table is in `AGENTS.md` at the repository root. Decisions behind the architecture
are recorded in the [architecture decisions](../adr/index.md).
