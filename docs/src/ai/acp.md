# ACP agents

`SQLer` can start an external coding agent that speaks the [Agent Client Protocol](https://agentclientprotocol.com/) (`ACP`), talk to it, show what it does, ask you before it runs a tool, and give it `SQLer`'s own tools. The headless client is the crate `sqler-acp`; the Agent tab that uses it is a separate step (T17). The design is in [ADR-0041](../adr/0041-acp-client.md).

Status: the client is tested against a mock agent only. Runs against the real Claude Code and Codex adapters are pending (maintainer).

## Profiles

A profile says how to start one agent: `command`, `args`, literal `env`, `inherit_env` (names copied from `SQLer`'s environment) and an `install_hint`. Two profiles are built in:

| Id | Program | Install | Variables it inherits |
| -- | ------- | ------- | --------------------- |
| `claude-code` | `claude-agent-acp` | `npm install -g @agentclientprotocol/claude-agent-acp` | `ANTHROPIC_API_KEY`, `CLAUDE_CONFIG_DIR` |
| `codex` | `codex-acp` | `npm install -g @agentclientprotocol/codex-acp` | `OPENAI_API_KEY`, `CODEX_HOME` |

The adapters are Node packages; they need Node.js and the agent's own login (for example `claude` signed in, or an API key in your environment). The package names were checked on npm on 2026-10-21; the older `@zed-industries/*` names are deprecated.

A user-defined profile is plain TOML (where it is stored is decided by the Agent tab):

```toml
id = "my-agent"             # lowercase letters, digits and '-'
name = "My agent"
command = "my-agent-acp"    # a name on PATH, or a path
args = ["--stdio"]
env = [["MY_AGENT_MODE", "readonly"]]   # literal variables, applied last
inherit_env = ["MY_AGENT_TOKEN"]        # copied from SQLer's environment when set
install_hint = "pip install my-agent-acp"
```

## What the agent gets from SQLer

The agent process starts with an empty environment plus:

1. a fixed list copied from `SQLer`'s environment: `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`, `TMPDIR`, `TEMP`, `TMP`, `XDG_*`, the proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, also lowercase), `SSL_CERT_FILE` and `NODE_EXTRA_CA_CERTS` (on Windows also `SystemRoot`, `windir`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `ProgramData`, `ProgramFiles`, `ComSpec` and `PATHEXT`);
2. the names in the profile's `inherit_env`: your own login variables, not `SQLer` secrets;
3. the profile's literal `env`.

Nothing else is passed, so a variable such as `SQLER_*` or a cloud credential in `SQLer`'s environment never reaches the agent. The arguments are the profile's own. The only secret `SQLer` gives an agent is the `MCP` token for the server it injects (next section).

## The MCP server it can use

When a session is created, `SQLer` hands the agent a list of `MCP` servers. Two shapes exist, matching the transports of `SQLer`'s `MCP` server:

- **Stdio:** a command (and arguments and environment) the agent starts itself. Its arguments and environment go to the agent as they are, so put no credential in them.
- **HTTP:** a URL on `127.0.0.1`, `[::1]` or `localhost`, plus a token. The token is sent only in the header `Authorization: Bearer <token>` of the session creation message. A URL on any other host is refused, and so is an HTTP server for an agent that did not say it supports HTTP `MCP`.

## What the agent may do on your machine

`SQLer` offers the agent **no** client capabilities: no file reading or writing through `SQLer`, no terminal. A request for them is answered "method not found". The agent still reads files through its own tools. Before it runs a tool that needs approval it sends a permission request; the host (the Agent tab) shows the choices (allow once, allow always, reject once, reject always). Cancelling a prompt answers a pending request with "cancelled".

## Lifecycle

- If the program is not on `PATH`, you get `agent "<name>" not found: "<program>" is not on PATH; install: <hint>` and nothing is started.
- `initialize` has 30 seconds. Cancel gives the agent 5 seconds to end the turn, then the process is killed. Closing the agent closes its stdin, waits 2 seconds, then kills it. The process is always waited for, so no zombie is left.
- If the agent crashes, every pending call fails with an "exited unexpectedly" error that carries the exit code or signal and the last 64 lines of the agent's stderr.
- Grandchild processes (an agent that starts another program) are not killed; only the agent process itself is.

## Troubleshooting

| Symptom | Cause and fix |
| ------- | ------------- |
| `not found ... is not on PATH` | Install the adapter with the hint shown, or give the profile the full path in `command`. A GUI app may have a shorter `PATH` than your terminal. |
| `exited unexpectedly (exit code 1)` | Read the stderr tail in the error: usually a missing login or API key. Set the variable in your environment and list it in `inherit_env`. |
| `did not answer in time during initialize` | The agent hung at start (for example waiting for a login prompt). Run it in a terminal first. |
| `does not support MCP over http` | The agent only supports stdio `MCP`. Use the stdio descriptor. |
| `speaks ACP version N` | The adapter is newer or older than the protocol version `SQLer` speaks (1). Update `SQLer` or the adapter. |
