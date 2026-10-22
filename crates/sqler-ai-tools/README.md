# sqler-ai-tools

The tool set exposed to models: catalog tools (`list_connections`, `list_schemas`, `list_tables`, `describe_table`, `describe_function`, `search_schema`), guarded SQL tools (`run_query`, `sample_rows`, `explain_query`) and UI tools (`open_in_editor`, `open_results_tab`).

Status: implemented (M5, T7 tools, T9 assist engine and `search_data`)

Layer: L3

## API overview

- `ToolSet`: `new(Arc<dyn SessionSource>, Limits)`, `with_approver`, `with_ui_host`, `definitions()`, `call()` and `call_detailed()`.
- `SessionSource`: lends connections, their `Catalog` and `Session`s.
- `Approver`: decides on every statement that is not classified `Read` (twice on `prod`); without one such statements are refused.
- `UiHost`: opens SQL in an editor or a results tab.
- `Limits`: row cap and timeout from `sqler-config`'s `AiLimits`; never lifted on `prod`.
- `Assist`, `AssistOptions`, `Conversation`: natural language to SQL, explain and fix; returns text only, never runs SQL. `AssistSearch` implements `DataSearch`, which turns on `search_data` (`ToolSet::with_data_search`).
- `ToolError`: error codes returned as `{"error":{"code","message"}}`.

Decision records: [ADR-0036](../../docs/src/adr/0036-ai-tool-schemas-guards-and-host-traits.md), [ADR-0038](../../docs/src/adr/0038-ai-assist-engine-prompts-and-grounding.md). Prompts: [Prompts and assist](../../docs/src/ai/prompts.md). Integrator page: [Tool contract](../../docs/src/ai/tool-contract.md).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
