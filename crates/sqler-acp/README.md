# sqler-acp

`ACP` client that spawns and hosts agents such as Claude Code, Codex CLI and Gemini CLI inside SQLer.

Status: implemented (M5, T16): launch, sessions, streamed updates, permission requests, cancel, `MCP` descriptor. Tested against a mock agent only; real Claude Code and Codex runs are pending (maintainer).

Layer: L5

## API

- `AgentProfile`, `builtin_profiles()` (`claude-code`, `codex`), `agent_env`, `resolve_program`: how to start an agent, and the scrubbed environment it gets.
- `AgentHandle::launch(profile, host, LaunchOptions)` starts the process, runs `initialize` and returns `(AgentHandle, AgentEvents)`.
  - `new_session(cwd, Vec<McpServerDescriptor>)`, `prompt(session, text)`, `cancel(session)`, `close()`, `exit_watch()`, `info()`, `pid()`.
  - `AgentEvents::recv()` yields `AgentEvent::{Update, Stderr, Exited}`.
- `AcpHost` answers permission requests.
- `McpServerDescriptor::{Stdio, Http}` with `McpToken` (redacted `Debug`, zeroized).
- `AcpError` has user-safe texts, for example a missing agent names the profile and an install hint.

The process is killed and reaped on every exit path. File system and terminal capabilities are not offered. See [ADR-0041](../../docs/src/adr/0041-acp-client.md) and the [user page](../../docs/src/ai/acp.md).

## Mock agent

`src/bin/sqler-acp-mock-agent.rs` is a small `ACP` agent for the tests (`tests/mock_agent.rs`). The prompt text picks the behaviour: `echo`, `permission`, `crash`, `hang`, `hang-deaf`, `env`, `descriptor`. Flags: `--no-mcp-http`, `--crash-on-init`.

```sh
cargo test -p sqler-acp
```

See [AGENTS.md](../../AGENTS.md) for the project handbook.
