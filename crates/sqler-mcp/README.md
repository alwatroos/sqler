# sqler-mcp

`MCP` server exposing the `sqler-ai-tools` tool set to external agents, over stdio and streamable HTTP.

Layer: L4

- `McpServer::new(tools, audit, options)` wraps one `ToolSet` (audit surface `mcp`).
  `tools/list` is the tool set's definitions minus the UI-only tools (`open_in_editor`,
  `open_results_tab`); `tools/call` goes through `ToolSet::call_detailed`, so guards, redaction,
  the untrusted-data envelope and the audit log apply. Approval is whatever the tool set carries:
  without an approver every statement that is not a plain read is refused (`no_approver`).
- `serve_io` / `serve_stdio`: newline-delimited JSON-RPC, one session, a line over
  `max_request_bytes` ends it.
- `bind_http` / `HttpServer::run`: `POST`/`DELETE` on `/mcp` (stateful, `Mcp-Session-Id`), loopback
  bind only, `Host`/`Origin` check, `Authorization: Bearer <token>` (`AuthToken`, constant-time
  compare, never logged), body cap. Answers are `application/json`; there is no server-to-client
  stream (`GET` is `405`).
- Protocol: JSON-RPC 2.0 with the `initialize` handshake (revisions 2025-11-25, 2025-06-18,
  2025-03-26, 2024-11-05), `ping`, `tools/list`, `tools/call` and `notifications/cancelled`.
  The protocol layer is in-crate because the `rmcp` SDK added more than the size budget allows
  (ADR-0040).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
