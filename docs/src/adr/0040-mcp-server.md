# 0040 MCP server: in-crate JSON-RPC layer, stdio and loopback HTTP

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

ADR-0003 promised that external agents (Claude Code, Claude Desktop, any MCP client) can use the same
tool set as the in-app chat, through the `sqler-mcp` crate. ADR-0036 and ADR-0037 built that tool set
(`ToolSet`) with its guards, redaction, untrusted-data envelope and audit log. This ADR fixes how the
tool set is served over the Model Context Protocol: which protocol layer, which transports, how a
client authenticates, and what the host (the `sqler-cli mcp` command now, `sqler-app` in T17) calls.

ADR-0003 and AGENTS.md section 2 named `rmcp` as the MCP SDK. That was a plan, not a measurement. This
ADR records the measurement.

## Decision drivers

- Every call goes through `ToolSet::call_detailed`, so no MCP path can skip a guard (ADR-0036).
- Surface and audit apply by construction (`with_audit(sink, Surface::Mcp)`).
- A local database tool must not become a network service: loopback only, authenticated.
- Binary size (the desktop budget is tight) and supply chain (`cargo deny` clean, no `rsa`,
  `openssl-sys`, `native-tls`, a second `rustls`).
- Follow the protocol revisions that real clients speak today.

## Considered options

1. Adopt `rmcp` 3.6.0 (server, async-rw transport, streamable-HTTP server) with a manual `ServerHandler`.
2. A minimal JSON-RPC 2.0 / MCP layer inside `sqler-mcp`, behind the same public API.

The step-1 gate (T15 spec) was four criteria, evaluated live on 2026-10-21 (details in the T15
verification notes):

| Criterion | rmcp 3.6.0 | Result |
| --------- | ---------- | ------ |
| C1 `cargo deny check` clean, no new ignore, exception or licence; no `rsa`, `openssl-sys`, `native-tls`, extra `aws-lc-rs`, second `rustls` | clean | pass |
| C2 no `axum`, no second major of `hyper`, `http`, `tower`, `schemars`, `reqwest` | none added (it does add `chrono`, `uuid`, `rand` 0.10, `base64` 0.23, `sse-stream`) | pass |
| C3 release `sqler-cli` grows by at most 1.5 MiB for the SDK plus the server (macOS arm64) | +2,510,192 bytes = **2.39 MiB** | **fail** |
| C4 manual handler, cancellation in the request context, stdio over any reader/writer, mountable HTTP service | yes | pass |

Size measurements (release `sqler-cli`, macOS arm64):

| Build | Bytes |
| ----- | ----- |
| main, untouched | 7,510,560 |
| tool-set stack, multi-thread tokio, no MCP | 7,969,808 |
| the same plus rmcp and the server | 10,480,000 (rmcp and server: +2.39 MiB) |
| the same plus the in-crate server | 8,462,784 (server: +492,976 = 0.47 MiB) |
| final `sqler-cli` with `mcp` (as shipped) | 8,886,384 (+1,375,824 = 1.31 MiB vs main 7,510,560) |

A second finding: rmcp's streamable-HTTP client features depend on `reqwest` 0.13 and the `aws-lc`
provider, a second major next to the workspace `reqwest` 0.12, so an rmcp client could not drive the
HTTP integration tests.

## Decision Outcome

**Option 2. `rmcp` was measured and is not adopted.** C3 failed by 0.89 MiB, and the cost is mostly the
full MCP model with `serde` and `schemars` derives, `chrono`, `uuid`, `rand`, `tracing` and SSE, none
of which a tools-only local server needs. The rmcp-based implementation was built first and passed its
integration tests, then was replaced; the public API did not change. The in-crate layer is small
(`initialize`, `ping`, `tools/list`, `tools/call`, two notifications) and has its own tests on both
transports. The cost is spec drift: the layer must be kept up to date by hand (see "Protocol revisions").
Revisit `rmcp` if its size drops or the size budget grows. AGENTS.md section 2 and ADR-0003 are updated
additively to point here.

### Protocol

- Methods: `initialize`, `ping`, `tools/list`, `tools/call`. Notifications: `notifications/initialized`,
  `notifications/cancelled`. Anything else is `-32601`. No batches (removed in 2025-06-18), no
  progress (`ToolSet` has no progress channel), logging, resources or prompts.
- Capabilities: `{"tools": {"listChanged": false}}`. `serverInfo` is the name (`sqler`) and the crate
  version.
- **Protocol revisions spoken:** 2025-11-25 (newest), 2025-06-18, 2025-03-26 and 2024-11-05. A client asking
  for another revision is answered with 2025-11-25 and decides whether to continue. **2026-07-28 is not
  implemented.** That revision drops `initialize` and sessions for stateless requests, so a client that
  speaks only that flow cannot connect. Revisit when Claude Code and Claude Desktop require it.
- `tools/list` is `ToolSet::definitions()` minus the UI-only tools (`open_in_editor`, `open_results_tab`).
  Each definition maps to `name`, `description`, `inputSchema` (must be an object schema, asserted at
  construction).
- `tools/call` builds `ToolCall::new("mcp-<jsonrpc id>", name, arguments)`, runs `call_detailed`, and returns the
  enveloped `ToolResult.content` unchanged as one text block, with `isError` from the result. Clients
  must treat the text as untrusted data (ADR-0037).
- A name that is not listed (unknown or UI-only) is `-32602` and is never dispatched. Malformed params are
  `-32602`. A panic or internal failure is `-32603` with a fixed text.

### Transports

- **stdio** (default): newline-delimited JSON over any reader/writer pair (`serve_io`; `serve_stdio` uses
  the process streams). A line over `max_request_bytes` (1 MiB) without a newline ends the session with
  `RequestTooLarge`. At most 64 `tools/call` requests may be pending per session. End of input lets
  running calls finish; shutdown cancels them. Only protocol bytes go to stdout.
- **HTTP**: `POST /mcp` answers `application/json` (no SSE). `GET` is 405, `DELETE` ends the session
  (204). A session is created by `initialize` and named by `Mcp-Session-Id`. `MCP-Protocol-Version`, when
  present, must be a spoken revision (else 400).
  - **Limits:** at most **64 sessions** at once (503 beyond); sessions idle for 30 minutes are dropped when a new one
    is created. The request body is capped at `max_request_bytes` (413).
  - **Bind policy:** loopback only. `bind_http` refuses any other address with `NonLoopbackBind`
    before binding. There is no option to bind elsewhere.
  - **Checks, in order:** path (404 unless `/mcp`), `Host` is `127.0.0.1`, `localhost` or `[::1]` with the
    bound port, and an `Origin`, if present, is loopback (403, which stops DNS rebinding), bearer token
    (401 with `WWW-Authenticate: Bearer`, one fixed body), body size (413).

### Authentication

HTTP requires `Authorization: Bearer <token>` (scheme case-insensitive). `AuthToken` is 32 random bytes
from `ring`'s `SystemRandom`, base64url without padding (43 characters). A supplied token must be
32 to 512 visible ASCII characters. Comparison is constant time (`subtle`). The type is `Zeroizing` and
its `Debug` prints `AuthToken(***)`; no error variant holds it. The token never appears in a log or error.

The CLI takes the token from `SQLER_MCP_TOKEN` or `--token-file` (read if present; created with mode 0600
on Unix and filled with a generated token if missing (on Windows the file inherits the directory ACL and
the permission check is skipped); both together is a usage error). With neither, a token is
generated and printed once on stdout in the start block. That is the hand-off: stderr and logs never carry
it, and a token file's content is never printed.

### Approval, audit, concurrency

- `McpServer::new` wraps the tool set with `with_audit(sink, Surface::Mcp)`, so every dispatched call is
  audited with `surface = mcp` and is subject to the same classification, row and time caps and redaction.
- Approval is whatever the host's `ToolSet` carries. `sqler-cli mcp` attaches no approver, so anything
  above `Read` is refused with `no_approver` (ADR-0036). Over MCP, writes work only when a host provides an
  approver (T17).
- `max_concurrent_calls` (default 4) is a semaphore: extra calls wait. `notifications/cancelled`, a dropped
  HTTP connection or shutdown drops the `call_detailed` future, which cancels the driver. Over HTTP the open
  POST of a cancelled call gets JSON-RPC `-32800`.

### Host API (T17, T16)

`McpServer::new(tools, audit, options)`, `serve_io`, `serve_stdio`, `bind_http(addr, token) -> HttpServer`
with `local_addr()`, `url()` (`http://127.0.0.1:PORT/mcp`) and `run(shutdown)`. Constants `MCP_PATH`,
`AUTH_HEADER` (equal to `sqler_acp::MCP_AUTH_HEADER`), `TOKEN_ENV`, `UI_ONLY_TOOLS`. T16's HTTP
server descriptor points at `url()` with the `Authorization` header.

## Consequences

- Good: +0.47 MiB for the server; no new supply-chain surface beyond `httpdate`; one code path
  (`call_detailed`) for guards; the token and loopback rules are tested.
- Bad: the protocol layer is ours to maintain, and 2026-07-28 is unsupported.
- Cancelled calls are audited: cancelling drops the `call_detailed` future, and the drop guard in
  `ToolSet` (T19, PR #73) writes an entry with outcome `cancelled`. This covers `notifications/cancelled`
  on stdio and HTTP, a dropped HTTP connection and shutdown.
- A client with the token can hold up to 64 sessions; the cap and idle expiry bound that.
- The token printed on stdout is visible to whoever reads the terminal or the parent process's pipe.
