# Tool contract

SQLer gives a model a fixed set of tools to read a database schema and run SQL. They live in the crate `sqler-ai-tools`; the decision is recorded in [ADR-0036](../adr/0036-ai-tool-schemas-guards-and-host-traits.md), building on [ADR-0003](../adr/0003-ai-tool-contract.md). This page is for people who embed the tools in a host.

## Tools

| Tool | What it does | Approval |
| ---- | ------------ | -------- |
| `list_connections` | Lends connections: id, name, driver, environment, `read_only` | no |
| `list_schemas` | Schemas of a connection (optionally one database) | no |
| `list_tables` | Tables, views and materialized views of a schema | no |
| `describe_table` | Columns, primary key and foreign keys | no |
| `describe_function` | Functions and procedures with their definition text | no |
| `search_schema` | Fuzzy name search over the cached schema | no |
| `sample_rows` | First rows of a table | like `run_query` |
| `run_query` | Runs SQL | per statement unless `Read` |
| `explain_query` | `EXPLAIN` of one statement | `analyze` needs approval |
| `open_in_editor` | Opens SQL in an editor tab (UI hosts only) | no |
| `open_results_tab` | Opens and runs a read statement in a results tab (UI hosts only) | refused unless every statement is `Read` with no approval reason (so never `EXPLAIN ANALYZE`) |
| `search_data` | Natural-language question to SQL and rows. Listed only when the host attaches a `DataSearch` (`ToolSet::with_data_search`); the generated SQL must be a plain read, is shown through `DataSearch::shown` before it runs, and the result is the `run_query` body plus `sql` and `rationale` (ADR-0038) | like `run_query` (anything but a plain read is refused) |

Catalog tools answer from the schema cache and do not open a session. Only the definition text of `describe_function` reads the database; it is cached.

## Using the tool set

```rust,ignore
let tools = ToolSet::new(Arc::new(source), Limits::default())
    .with_approver(Arc::new(approver))
    .with_ui_host(Arc::new(ui));
let definitions = tools.definitions();      // give these to the provider
let result = tools.call(&tool_call).await;  // run one tool call
let report = tools.call_detailed(&tool_call).await; // result plus executed statements
```

A host implements:

- `SessionSource`: `connections()`, `catalog(id)`, `acquire(id)` and `release(id, session)`. Lend only the connections the user opted in for AI.
- `Approver`: answers an `ApprovalRequest` (SQL, class, reason, stage, zero-based statement index) with `Approve` or `Deny`.
- `UiHost` (optional): `open_in_editor` and `open_results_tab`. Without it the two tools are not offered and answer `not_available`.

## External tools (plugins)

A host can add tools of its own with `ToolSet::with_external_tools(Arc<dyn ExternalTools>)` (ADR-0062). The desktop app does this for plugins that have the `ai.tool` capability; `sqler-cli mcp` lists none.

- `ExternalTools::tools()` returns `ExternalTool { name, description, input_schema }`. A tool is listed, after the built-ins, only if its name matches `[A-Za-z0-9_-]{1,64}`, contains `__`, is not a built-in name and is not a duplicate, and its schema is an object with `"type":"object"`. The description is clipped to 1024 bytes. The app names plugin tools `<plugin-id>__<tool-name>`.
- Before the call the arguments are checked against a subset of JSON Schema: an object; `required`; a top-level `properties.<k>.type` and `enum`; `additionalProperties: false`. A failure is `invalid_arguments` and the tool is not called.
- `ExternalTools::call(name, arguments, ctx)` gets an `ExternalContext` that is valid for that call only. `ctx.connections()` lists what the surface lends, `ctx.use_connection(id)` names a connection by id (it feeds the audit row and the redaction), and `ctx.approve(ExternalApproval)` asks the surface's `Approver`. The approval card shows `tool = "external_tool"`, the exact SQL and the connection. Parameters cannot be shown, so a statement with parameters that needs approval is refused unasked.
- One denial turns the call into `denied`, and a missing approver into `no_approver`, whatever the tool returns. The wait counts against the call deadline.
- The result goes through the same exit as a built-in tool: redaction (a `columns` array of plain names or of objects with a `name`), the untrusted-data envelope, the audit row and the cancel guard. A `read_only` connection is enforced by the host's data service, not by the tool set.

## Safety rules

- A statement that is not classified `Read` (write, DDL, other such as `SET` or transaction control, or unknown) needs an approval. `Read` means no write syntax was found (ADR-0020).
- All approvals of a call are collected before the first statement runs. A denial stops the whole call.
- On a `prod` connection every such statement is confirmed twice.
- Without an `Approver` the call is refused (`no_approver`). A `read_only` connection refuses (`read_only`).
- `EXPLAIN ANALYZE` executes the statement, so it needs approval; SQLite has no `ANALYZE` form and answers `unsupported`.
- SQL Server has the `tsql` dialect ([ADR-0073](../adr/0073-tsql-dialect-and-manifest-dialect.md)): its statements are classified, `sample_rows` uses `select top (n)`, and `explain_query` answers `unsupported`.
- A driver without a dialect: `run_query` asks approval for the whole text as one unknown statement; `sample_rows` and `explain_query` answer `unsupported`.

## Limits

Defaults come from `ai.toml` (1000 rows, 30 seconds; see [AI configuration](../user-guide/ai.md)). The row cap applies per statement, the deadline per call. The deadline starts once the approvals are in and covers acquiring the session and running the statements; time spent waiting for a person to approve does not count. On a timeout the driver is asked to cancel and the statement reports `time_limit`; at the row cap it reports `row_limit` with a notice. The `unlimited` setting lifts both caps, but never on `prod`.

## Results and errors

Query tools return `{connection, limits, not_run, statements:[...]}`; each statement has `index`, `sql`, `class`, `approved`, `columns`, `rows`, `row_count`, `rows_affected`, `truncated`, `notice` and `error`. Binary values are `"0x"` plus hex; non-finite floats are the strings `"NaN"`, `"Infinity"`, `"-Infinity"`.

A failed call returns an error result `{"error":{"code":"...","message":"..."}}` with one of: `unknown_tool`, `invalid_arguments`, `unknown_connection`, `not_available`, `unsupported`, `not_found`, `no_approver`, `denied`, `read_only`, `catalog`, `driver`, `host`.

Results are wrapped in an untrusted-data envelope and redacted before they reach the model (ADR-0037, [Safety](safety.md)). The result text is `NOTICE`, `<untrusted-data>`, the JSON with `<` and `>` escaped, `</untrusted-data>`, each on its own line; `sqler_ai_tools::unwrap_untrusted(content)` returns the inner JSON text, which parses to the same value. Statements that are touched by redaction carry a `redaction` object.
