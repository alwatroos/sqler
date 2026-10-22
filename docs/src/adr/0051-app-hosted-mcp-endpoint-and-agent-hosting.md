# 0051 App-hosted MCP endpoint and agent hosting

- Status: accepted
- Date: 2026-10-22

## Context and problem statement

[ADR-0040](0040-mcp-server.md) gives SQLer an MCP server (`sqler-mcp`) and [ADR-0041](0041-acp-client.md) an ACP client (`sqler-acp`) that launches an external coding agent and hands it MCP server descriptors. Both leave the host open: who serves the tools from inside the desktop app, which connection and approver they use, and who owns the agent process. The app must let an agent use one connection through the same tools, limits, redaction, audit and approvals as the in-app AI session ([ADR-0053](0053-ai-session-tab-and-approvals.md)), and must never leave an agent or an endpoint behind.

## Decision drivers

- No write without an approval in the UI that shows the exact SQL; production asks twice through the real gate ([ADR-0024](0024-query-execution.md)).
- The token is never shown, logged or audited.
- No orphan processes and no endpoint after its tab is gone, on every exit path.
- Tokio work only through `SessionBridge`; state lives in the `Workspace` and is testable headless (the pattern of ADR-0053).
- Lazy startup: nothing is read, bound or launched before the user opens an agent.

## Considered options

1. One shared endpoint for the whole app, or one endpoint per Agent tab.
2. The endpoint and lifecycle in `sqler-ui`, or a `Connector`-like trait implemented in `sqler-app`.
3. State in the `Workspace` plus one runner task per tab, or an actor that owns the state.
4. ACP permission requests in T13's `ApprovalQueue`, or a sibling queue.

## Decision

Option 1b, 2a, 3a and 4b.

| Topic | Decision |
| ----- | -------- |
| Tab kind | Pinned `TabKind::Agent`, titled `Agent: {connection}`, with its state in `Workspace` (`ai::agent`). It needs a selected connection, AI on for it and a readable `ai.toml`; no provider is needed. |
| Endpoint and token | One loopback endpoint per tab (`127.0.0.1`, port 0) with its own random `AuthToken` and its own approver, so an approval goes to the tab whose agent asked. The token exists only in `McpToken`, `AuthToken` and the session descriptor; the URL holds none. The status item `MCP · n` counts live endpoints. |
| Approvals | SQL writes go through T13's `ApprovalQueue`, `answer_id` and `open_prod_gate` via the generalised `approvals_mut` seam. Each ask has an id; only the front ask can be answered. A palette command (`ai:approve-statement`) needs the card to be shown, because it carries no id; a click on a card carries the id of the ask it shows and needs nothing else. Production shows the second confirmation through the real gate. |
| Permissions | ACP permission requests use a sibling `PermissionQueue` with the same contract: ask id, front-only answers, only an offered option, shown-only palette commands, a dropped reply means `Cancelled`. Options show in the agent's order; reject holds the focus. |
| Audit | The endpoint writes surface `agent`: an `AgentSurface` sink clones the entry and sets `surface`, because `McpServer::new` forces `mcp`. |
| Runner and teardown | One runner task per tab owns the agent handle, the endpoint and the source. `AgentControl` sends commands synchronously, so the GPUI thread needs no runtime. Every exit path (close, quit, crash, AI off, a dropped control) runs one teardown: drop the prompt future, stop the endpoint, shut the source and close the session, then `AgentHandle::close` (kill and reap), then report the exit. |
| Bounds | Cancel sends `session/cancel` and ends the turn; the process is killed only if the agent ignores it for 5 s. Endpoint stop waits at most 250 ms; the agent gets a 500 ms grace (`AGENT_SHUTDOWN_GRACE`) to exit after stdin closes. `app:quit` waits `CLOSE_LIMIT` (2 s); the OS-menu quit hook only 150 ms. Close, quit, crash and AI-off always stop the process. A tab that is still starting holds an abort signal: closing, AI off and quit fire it, `start_agent` gives up and kills the half-started process (a dropped `AgentHandle::launch` kills it, also during `initialize`), and a started runner that was never adopted tears itself down. The idle session close in the teardown is bounded to 500 ms. |
| Crash and missing agent | The tab shows the failure card: the message, an `Install: {hint}` line, the hint to set an absolute `command` in `agents.toml`, and the last 64 stderr lines. A restart starts a new run. |
| Profiles | Built-ins plus `<config>/agents.toml` (`[[agent]]` tables; a user id replaces a built-in id). Read lazily on the blocking pool at the first open, never written. An invalid file gives a notice and the built-ins. |
| Working directory | `<config>/agent-workspace`, else `temp_dir()/sqler-agent`. |
| AI off | Turning AI off for a connection stops its agents and denies their asks. |

## Consequences

- An agent sees what the in-app session sees and is bound by the same limits, redaction and approvals; the guard suite checks this on the three surfaces.
- Every Agent tab binds a port; ports are per tab and ephemeral.
- Known limits: grandchildren of the agent are not killed ([ADR-0041](0041-acp-client.md)); a Windows npm `.cmd` shim can leave `node` running; within the 150 ms OS-menu quit hook an agent that ignores stdin EOF can outlive SQLer; on macOS a GUI app has no npm global bins on `PATH`, so use an absolute `command` in `agents.toml`; an agent may echo its own descriptor, token included, into its messages, which SQLer shows but never logs or audits.
- Known limit, `SQLite`: the driver cancels a statement only between rows. After a time-limited statement that yields no rows (for example a long cross join) the tab's session stays busy, and the next call on the same tab hits the time limit too. The behaviour is the same on all three surfaces (in-app session, MCP, agent); teardown stays bounded because the session is dropped.
- The `AgentSurface` wrapper rewrites the surface after the fact instead of the server taking a surface.

## Follow-ups

- Interrupt `SQLite` in the middle of a statement in the driver (`sqlite3_interrupt` or a progress handler), so that a cancel or a time limit frees the session at once.
- `PermissionQueue` duplicates the contract of `ApprovalQueue` because `ApprovalRequest` is `#[non_exhaustive]` with no constructor. Make `ApprovalQueue` generic and fold `PermissionQueue` in; the unit tests mirror each other one to one until then.
- The guard suite drives the endpoint with a stand-in MCP client, because the mock agent cannot call MCP. Add an `mcp-call` prompt to the mock agent and drop the stand-in.
- The engine transcript in `sqler-ai-tools` is unbounded.
