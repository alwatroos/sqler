# sqler-completion

Intellisense engine: scope analysis, alias resolution and ranking. Pure functions, heavily tested, no async, no I/O, no UI.

`complete(&CompletionRequest, &dyn SchemaSource) -> Completions` returns ranked candidates (schemas, tables, views, columns in scope, aliases, CTEs, functions, keywords, snippets) for SQL text, a cursor offset and a dialect (`PostgreSQL`, `MySQL`, `SQLite`). It works on incomplete SQL through a hand-written lexer and a clause tracker, and reads names through the `SchemaSource` trait (`MemorySchema` for tests); data that is not loaded yet comes back as `SchemaRequest` hints. `analyze` returns the statement, context, qualifier and scope without a schema. Method and rule of the benchmark: `docs/src/benchmarks.md`.

Bench: `cargo bench -p sqler-completion --bench completion -- [--tables N --columns N --samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-p95-ns B]`.

Status: implemented (M3)

Layer: L2

See [AGENTS.md](../../AGENTS.md) for the project handbook.
