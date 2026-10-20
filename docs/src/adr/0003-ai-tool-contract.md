# ADR-0003: AI tool contract

- Status: accepted
- Date: 2026-10-20
- Deciders: SQLer maintainers

## Context and Problem Statement

Models reach SQLer's data through tools. Three surfaces need them: the in-app chat, an MCP server
for external agents, and agents hosted inside SQLer over ACP. The tools and their safety guards
must be identical on every surface.

## Decision Drivers

- One place to enforce the safety guardrails.
- No drift between surfaces.
- Every call is auditable.

## Considered Options

- One shared tool set in `sqler-ai-tools`.
- A tool set per surface.
- A raw SQL tool without classification.

## Decision Outcome

One tool set in `sqler-ai-tools`, shared by the in-app chat, the MCP server (`sqler-mcp`, rmcp) and
ACP-hosted agents (`sqler-acp`).

| Tool | Purpose | Guard |
|---|---|---|
| `list_connections` | names, environment, read-only flag | none |
| `list_schemas`, `list_tables`, `describe_table`, `describe_function` | catalog from `sqler-catalog`, never hits the DB if cached | none |
| `search_schema` | fuzzy search over object names and comments | none |
| `sample_rows` | first N rows of a table | row cap |
| `run_query` | execute SQL | read-only by default; statements classified by `sqler-sql`; any write/DDL requires user approval in the UI; hard row/time limits; prod connections require a second confirmation |
| `explain_query` | EXPLAIN (ANALYZE only with approval) | |
| `search_data` | natural-language search → generated SQL → results, with the SQL shown to the user before and after | same as `run_query` |
| `open_in_editor`, `open_results_tab` | UI-side tools, only available to the in-app assistant | |

Binding guardrails:

1. Default read-only. Writes, DDL, and anything the classifier cannot categorise require an explicit, per-statement user confirmation showing the exact SQL.
2. Row and time limits on every AI-initiated query; configurable, never disableable for prod-labelled connections.
3. No data leaves the machine unless the user picked a remote provider; a clear indicator shows which provider receives schema/data. Ollama (local) is the documented default.
4. Column-level redaction list (e.g. `password`, `ssn`, `token`) applied to anything sent to a provider; user-extensible.
5. Audit log for every tool call.
6. Prompt-injection hardening: content coming from the database (cell values, comments) is wrapped as untrusted data in prompts and never interpreted as instructions.

Every tool call writes an audit log entry with the tool, arguments, SQL executed and result size,
plus a timestamp and the connection. The user can open the log.

### Consequences

- Argument schemas and the default row and time limit values are set in M5.
- Guards live in one crate and cannot be skipped by choosing a different surface.

### Confirmation

M5 tests run the same guard scenarios through the in-app, MCP and ACP surfaces.

## Pros and Cons of the Options

### One shared tool set

- Good: one guard implementation, one audit path.
- Bad: UI-side tools need an in-app-only exception.

### A tool set per surface

- Bad: tool sets drift, and guards get duplicated.

### A raw SQL tool without classification

- Bad: breaks guardrail 1 (default read-only).

## More Information

- [AI](../ai/index.md)
