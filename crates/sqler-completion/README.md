# sqler-completion

Intellisense engine: scope analysis, alias resolution and ranking. Pure functions, heavily tested, no async, no I/O, no UI.

`complete(&CompletionRequest, &dyn SchemaSource) -> Completions` returns ranked candidates (schemas, tables, views, columns in scope, aliases, CTEs, functions, keywords, snippets) for SQL text, a cursor offset and a dialect (`PostgreSQL`, `MySQL`, `SQLite`). It works on incomplete SQL through a hand-written lexer and a clause tracker, and reads names through the `SchemaSource` trait (`MemorySchema` for tests); data that is not loaded yet comes back as `SchemaRequest` hints. `analyze` returns the statement, context, qualifier and scope without a schema. `hover(&HoverRequest, &dyn SchemaSource) -> Option<Hover>` describes the identifier under the cursor (a column with type, nullability and foreign key, a table or view with kind and schema, a CTE or subquery), and `diagnose(text, dialect, &dyn SchemaSource) -> Vec<Diagnostic>` reports unknown schemas, tables, columns and qualifiers and ambiguous columns per statement; both are conservative and never flag names whose schema data is not loaded. Method and rule of the benchmark: `docs/src/benchmarks.md`.

Bench: `cargo bench -p sqler-completion --bench completion -- [--tables N --columns N --samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-p95-ns B]`.

Status: implemented (M3)

Layer: L2

See [AGENTS.md](../../AGENTS.md) for the project handbook.
