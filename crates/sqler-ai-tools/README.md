# sqler-ai-tools

The tool set exposed to models: catalog tools (`list_connections`, `list_schemas`, `list_tables`, `describe_table`, `describe_function`, `search_schema`), guarded SQL tools (`run_query`, `sample_rows`, `explain_query`) and UI tools (`open_in_editor`, `open_results_tab`).

Status: implemented (M5, T7); `search_data` is added by T9

Layer: L3

## API overview

- `ToolSet`: `new(Arc<dyn SessionSource>, Limits)`, `with_approver`, `with_ui_host`, `definitions()`, `call()` and `call_detailed()`.
- `SessionSource`: lends connections, their `Catalog` and `Session`s.
- `Approver`: decides on every statement that is not classified `Read` (twice on `prod`); without one such statements are refused.
- `UiHost`: opens SQL in an editor or a results tab.
- `Limits`: row cap and timeout from `sqler-config`'s `AiLimits`; never lifted on `prod`.
- `ToolError`: error codes returned as `{"error":{"code","message"}}`.

Decision record: [ADR-0036](../../docs/src/adr/0036-ai-tool-schemas-guards-and-host-traits.md). Integrator page: [Tool contract](../../docs/src/ai/tool-contract.md).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
