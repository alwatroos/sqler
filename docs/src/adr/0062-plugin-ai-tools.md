# ADR-0062: Plugin AI tools through the tool contract

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

A plugin with the `ai.tool` capability declares tools in its manifest ([ADR-0055](0055-plugin-abi-wit.md)) and exports
`ai-tools.invoke`. Until now nothing offered those tools to a model. The AI tool contract ([ADR-0036](0036-ai-tool-schemas-guards-and-host-traits.md),
[ADR-0037](0037-ai-redaction-untrusted-data-and-audit-log.md)) fixes how a model's tool call is guarded, redacted and
audited, and the plugin `query` interface ([ADR-0058](0058-plugin-query-and-catalog.md)) fixes how a plugin runs SQL.
This ADR joins them: a plugin tool is a tool like any other, and a model call to it must not be a way around either set
of rules.

## Decision Drivers

- `sqler-ai-tools` and `sqler-plugin-host` are L3 peers. Neither may depend on the other.
- One exit for every tool call, so redaction, the untrusted-data envelope, the audit row and the cancel guard cannot be
  skipped for plugin tools.
- Any SQL a plugin runs for a model goes through the plugin gate of ADR-0058 **and** the approver of the surface that
  made the call. No approver means nothing but plain reads runs.
- The audit guarantee holds: a surface with no audit sink offers no plugin tools.
- The plugin engine stays out of default builds ([ADR-0054](0054-plugin-runtime-and-driver-feasibility.md) size rule).

## Considered Options

- A trait seam `ExternalTools` in `sqler-ai-tools`, an adapter in `sqler-app` (feature `plugins`), and a typed
  `ai-tools.invoke` call in the host that takes the data service and approver of one call.
- A separate field on the workspace and on each AI job. Same result; touches three workspace files and three job
  structs. Rejected.
- Make `sqler-ai-tools` depend on the host. Rejected: it breaks the layering and would pull wasmtime into every AI crate.

## Decision Outcome

Chosen: the first option.

### The seam

`sqler-ai-tools` gets `ExternalTools` (`tools()` and `call(name, arguments, ctx)`), `ExternalTool`, `ExternalContext`
and `ExternalApproval`. `ToolSet::with_external_tools` attaches an implementation. The tool set lists external tools
after its built-ins and `search_data`, with or without a UI host, and dispatches an unknown name to them. The call runs
through the same `call_detailed` to `finish` path as a built-in, so redaction, the envelope, the audit row and the
cancel guard need no extra code.

### Naming

The model sees `<plugin-id>__<tool-name>`. A tool is listed only if its name matches `[A-Za-z0-9_-]{1,64}`, contains
`__`, is not a built-in name and is not a duplicate (the first wins), and its schema is an object with
`"type":"object"` (MCP requires it). A longer name is left out by the host. The description is prefixed with the plugin
name and id (`Plugin tool from <name> (<id>): ...`) and clipped to 1024 bytes. It reaches the model, so it is treated as
untrusted text and the prefix tells the model where it came from.

### Arguments

The tool set checks the arguments against a small subset of JSON Schema before the call: an object; `required`;
a top-level `properties.<k>.type` and `enum`; `additionalProperties: false`. Other keywords are ignored. A failure is
`invalid_arguments` and the plugin is not called. The guest still validates its own input.

### The approval path

`ExternalContext::approve` is the only way a plugin statement is confirmed during an AI call. In order: an earlier
denial denies without asking; an unknown connection denies; bound parameters deny (the AI approval card cannot show
parameters, and ADR-0058 requires the exact SQL **and** parameters, so a statement with parameters that needs approval
is refused unasked); no approver marks the call `no_approver`; otherwise the surface's `Approver` is asked, with
`tool = "external_tool"`, the exact SQL and the connection. `prod` asks twice. One denial turns the call into `denied`
whatever the plugin returns, and later asks are denied unasked. A leaked context cannot approve after its call ended.

The app adapter gives each call its own `PluginData`: a `SurfaceData` that shows only the connections the surface lends
and applies the surface's `read_only` flag at the data layer (the plugin gate then declines every non-read statement on
it), and a `SurfaceApprover` that forwards `WriteRequest` to `ExternalContext::approve`. The host swaps this value into
the instance's services for that one call and restores the previous services afterwards. A dropped or timed-out call
drops the instance, so the swap cannot leak. The plugin manager's own write dialog is never used for an AI call, and a
plugin cannot reach a connection that the AI opt-in left out.

### Audit scope

The audit row of a plugin tool call is always written, with the tool name and the surface. Statements that went through
approval are listed in it (they are marked as approved). Read statements that a plugin ran without approval are not
listed: the host does not report them to the tool set. Plugins see no catalog on AI calls, because the app has none to
lend outside a window. A result is redacted by the existing rules; a `columns` array of plain names is understood as well
as one of objects with a `name`, so a plugin result that lists a `password` column is masked.

### Which surfaces list plugin tools

The in-app session, the in-app agent endpoint (ACP over the in-app MCP server) and the in-app MCP server. The tools ride
on `AiAudit`, which every AI job already receives, so a surface with no audit sink cannot have plugin tools. Editor quick
actions (assist) build their tool set the same way but keep their fixed allow-list (`search_schema`, `describe_table`)
and never offer them. The agent and MCP endpoints snapshot the definitions when they start: a plugin granted later
appears with the next agent or server.

Standalone `sqler-cli mcp` lists no plugin tools in M6. It has no plugin host and no grant store: grants live in the
app's `GrantBook`, and a headless server with no window could not show the grant dialog or the write approval. Adding a
host there would mean a second place that decides what a plugin may do. The decision can be revisited when the CLI
gets its own grant store.

### Errors

A failed plugin call is mapped to the tool errors of the contract; no engine text reaches the model. Guest messages are
clipped to 512 characters. A plugin result that is not JSON is a `host` error. The wait for an approval counts against
the `ai_tool` deadline (30 s), as ADR-0058 already says for commands.

### Size

Everything new in `sqler-app` sits under the feature `plugins`, which stays off by default.
`cargo tree -p sqler-app -e normal --prefix none | grep '^wasmtime '` and `cargo tree -p sqler-ui -i wasmtime` print
nothing by default.

## Consequences

- Plugin tools get the full guard of the contract with one seam and no duplicate rules.
- A tool that runs a write shows the user the exact SQL on the surface's own approval card, labelled `external_tool`.
- Plugin read statements are not listed in the audit row, and plugins have no catalog on AI calls.
- `sqler-cli mcp` offers no plugin tools until it has a grant store.
