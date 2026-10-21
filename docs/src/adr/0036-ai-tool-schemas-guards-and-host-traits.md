# 0036 AI tool schemas, guards and host traits

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

A model that works on a database needs tools: read the schema, run SQL, explain a plan. ADR-0003 fixed the tool names and the safety rules in principle. M5 now needs the concrete contract: JSON schemas, result shapes, the guard that stands between a model and a driver, and the interfaces a host (the desktop app, the CLI, the MCP server) implements to lend connections. The code lives in `sqler-ai-tools` (L3). AGENTS.md §10.2 requires an ADR before a new public API.

## Decision drivers

- One entry point that owns every guard, so no host can skip one.
- A model must never run a write, DDL or unclassifiable statement without a human decision, and `prod` needs a second one.
- Catalog tools answer from the cache and do not touch the database.
- Bounded cost: row cap and deadline per call, enforced in the tool layer, cancelling the driver on timeout.
- Dyn-compatible traits in the style of `sqler-driver-api` and `sqler-ai-api` (boxed futures, no `async-trait`), no new external dependency.
- Testable with SQLite and a scripted approver.

## Considered options

1. Free functions per tool, each host adds its own guards.
2. One `ToolSet` that owns the guards, with host traits (`SessionSource`, `Approver`, `UiHost`) for everything it needs from outside.
3. Expose the driver `Session` to the model layer and let the agent loop guard.

## Decision outcome

Chosen: option 2.

### Entry point

`ToolSet::new(Arc<dyn SessionSource>, Limits)`, optionally `with_approver` and `with_ui_host`. `definitions()` returns the `ToolDefinition`s (hand-written JSON Schemas); `call(&ToolCall) -> ToolResult` is the only way to run a tool; `call_detailed` returns a `CallReport` (result, connection, executed statements with class, approval and row count) for audit and the UI. The futures are `Send`. Arguments are parsed with `ToolCall::parse_arguments` into structs with `deny_unknown_fields`; every schema is `{"type":"object","properties":..,"required":..,"additionalProperties":false}`.

### Tools

| Tool | Arguments | Result |
| ---- | --------- | ------ |
| `list_connections` | `{}` | `{connections:[{id,name,driver,environment,read_only}]}` |
| `list_schemas` | `connection, database?` | `{connection, supported, schemas}` |
| `list_tables` | `connection, addr, kinds?` | `{connection, tables:[{name,kind,database,schema}]}` |
| `describe_table` | `connection, addr, name` | `{connection, table:{name,kind,database,schema,columns,primary_key,foreign_keys}}` |
| `describe_function` | `connection, addr, name` | `{connection, functions:[{name,kind,database,schema,definition}]}` |
| `search_schema` | `connection, query, kinds?, limit?` (1 to 200, default 50) | `{connection, hits:[{name,kind,database,schema,table,quality}]}` |
| `sample_rows` | `connection, addr, table, limit?` (default 50) | query result |
| `run_query` | `connection, sql, max_rows?` | query result |
| `explain_query` | `connection, sql, analyze?` (default false) | query result |
| `open_in_editor` | `sql, connection?` | `{opened:true}` |
| `open_results_tab` | `connection, sql, title?` | `{opened:true}` |
| `search_data` | `connection, question, max_rows?` | `{sql, ...query result}` (schema only here; T9 implements it) |

`addr` is `database?: string, schema?: string`. `list_tables` kinds are `table`, `view`, `materialized_view`. `describe_function` reads the definition text through a session and the driver's introspection (`null` without introspection); it is cached in the `ToolSet` per connection and object (at most 256 entries, cleared when full). All other catalog tools read the `Catalog` (cache first) and never acquire a session. `search_schema` matches names only.

Query result:

```json
{"connection":"c1","limits":{"max_rows":1000,"timeout_ms":30000},"not_run":0,
 "statements":[{"index":0,"sql":"...","class":"read","approved":true,
   "columns":[{"name":"id","type":"integer"}],"rows":[[1]],"row_count":1,
   "rows_affected":null,"truncated":null,"notice":null,"error":null}]}
```

Cells: `Null` is `null`, `Bool` and `Int` are JSON values, a finite `Float` is a number else the string `"NaN"`, `"Infinity"` or `"-Infinity"`, `Bytes` is `"0x"` plus hex, every other value is its `Display` string. Column `type` is the lowercase `Debug` of `DataType`. Large integers (above 2^53) may lose precision in some JSON readers.

### Errors

A failed call returns `ToolResult::error` with `{"error":{"code":..,"message":..}}`. `ToolError` variants and codes: `UnknownTool` (`unknown_tool`), `InvalidArguments` (`invalid_arguments`), `UnknownConnection` (`unknown_connection`), `NotAvailable` (`not_available`), `Unsupported` (`unsupported`), `NotFound` (`not_found`), `NoApprover` (`no_approver`), `Denied { index }` (`denied`), `ReadOnly` (`read_only`), `Catalog` (`catalog`), `Driver` (`driver`), `Host` (`host`). A driver error inside a statement is not a tool error: it is the statement's `error` field and the rest count in `not_run`.

### Guard and approval flow

1. The SQL text is classified per statement with `sqler_sql::classify_each` (fail-safe, ADR-0020). A driver without a dialect (SQL Server, `DuckDB`) has no splitter: the whole trimmed text is one `Unknown` statement. Empty text is `invalid_arguments`.
2. Every statement whose class is not `Read` needs an approval; so does any `EXPLAIN` with `ANALYZE` or `ANALYSE` among its options, found by tokenizing the statement (bare or inside the parenthesised list, any case, with comments and whitespace in between), whichever tool sent it (reason `ExplainAnalyze`). `explain_query` also refuses `sql` that itself starts with `EXPLAIN`, `ANALYZE`, `ANALYSE` or `(`; the `analyze` flag is the only way to ask for it. `Read` means "no write syntax found", not "provably harmless".
3. Approvals are collected for all statements, in order, before the first one runs. The `Approver` gets an `ApprovalRequest` (tool, connection, SQL, class, reason, stage, index, count). On a `prod` connection each statement is asked twice: `Confirm`, then `ProdSecond`.
4. A `Deny` returns `denied` with the zero-based statement index (`ApprovalRequest.index` is zero-based too) and nothing runs. A `read_only` connection refuses any statement that needs approval (`read_only`).
5. Without an `Approver`, any statement that needs approval is refused (`no_approver`). There is no default-allow.
6. `open_results_tab` accepts only statements classified `Read`; the UI tools are absent from `definitions()` and answer `not_available` when no `UiHost` is set.

### Limits

`Limits { max_rows, timeout, unlimited }` is built from `sqler-config`'s `AiLimits` (defaults 1000 rows and 30 seconds). `Limits::effective(environment)` gives `Caps`; `unlimited` removes the caps except on `prod`, where they always apply. The effective row cap is the smaller of the argument `max_rows` and the cap, per statement; one more row than allowed marks the statement `row_limit` and the stream is dropped. One deadline covers a call (approvals come before it starts). On expiry the tool asks the driver to stop through `CancelHandle::cancel`, drops the stream, marks `time_limit` and counts the remaining statements in `not_run`. The session is released and stays usable, unless the call's future is dropped: then the session is dropped without `release`. After the deadline, `cancel` and `release` each get at most two seconds. SQLite cannot interrupt a step before its first row: the tool returns at the deadline, the driver stops later.

### Known limits

- Static classification cannot see the side effects of a function called inside a `SELECT` (`select pg_terminate_backend(1)`, `select nextval('s')`). Approval and, once it lands, the read-only connection mode are the controls; the read-only mode arrives with a later work item.
- `INSERT ... RETURNING` and similar statements stream rows: the row cap cuts the stream after the cap, but the write itself is not undone, and the driver may have applied more rows than the tool returned.

### Host interfaces

- `SessionSource`: `connections()` lends `ConnectionInfo` (id, name, driver, environment, `read_only`), `catalog(id)` gives the `Catalog`, `acquire(id)` lends a boxed `Session` and `release` returns it (default: close). The host decides which connections are lent; AI opt-in filtering (ADR-0035) is the host's job.
- `Approver`: `approve(&ApprovalRequest) -> Decision` (`Approve` or `Deny`). The UI shows a dialog; the CLI may prompt or deny.
- `UiHost`: `open_in_editor` and `open_results_tab`, implemented only by hosts with a window.

All traits use boxed futures, as `sqler-driver-api` does.

## Consequences

- Good: one place for every guard; hosts cannot forget one, and the property test checks that every executed statement is `Read` or approved (and confirmed twice on `prod`).
- Good: catalog tools are cheap, offline-capable and need no approval.
- Good: no new external dependency; `sqler-config` is used for limits only.
- Bad: `sqler-config` pulls `keyring` into hosts such as `sqler-cli`.
- Bad: with no read-only transaction wrapper in `sqler-driver-api`, the guard rests on classification; a misclassified write needs the user to read the SQL in the dialog.
- Bad: dialect-less drivers get whole-text `Unknown` approval, and `sample_rows` and `explain_query` are `unsupported` there.
- Neutral: `read_only` is `false` until a connection mode exists.

## Pros and cons of the options

- Free functions per tool: each host re-implements approval and limits; one gap is a safety hole. Rejected.
- `ToolSet` with host traits: chosen, see above.
- Exposing `Session` to the agent loop: the loop would own guards and cancellation, spread across T8, T9 and the MCP server. Rejected.
