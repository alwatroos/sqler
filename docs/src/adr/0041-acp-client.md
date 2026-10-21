# 0041 ACP client: agent profiles, one supervisor task per agent, scrubbed environment, MCP descriptor and no client capabilities

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

M5 makes `SQLer` an `ACP` (Agent Client Protocol) client: it starts an agent process such as Claude Code (through its `ACP` adapter) or Codex (`codex-acp`), runs sessions with it, shows its streamed updates, relays its permission requests to the user and gives it `SQLer`'s `MCP` tools (T15, ADR-0040). The Agent tab (T17) is the user of this crate, and it runs on the shared tokio runtime behind the `SessionBridge` (ADR-0007). `sqler-acp` (layer L5) is the headless part: it launches, talks, and cleans up. Six things need a decision: the protocol crate and its version, how agents are described, the process lifecycle, how the `MCP` server is handed over, which client capabilities `SQLer` offers, and what the agent may inherit from `SQLer`'s environment.

## Decision drivers

- A crash, a hang or a cancel must never leave an orphan process; the process is killed and reaped on every exit path.
- No secret of `SQLer` reaches the agent except the `MCP` token for the injected server.
- A missing agent is a clear, actionable message, not a spawn error.
- The public API is `Send + Sync` and works from the multi-thread runtime; the UI never blocks.
- Tests need no network and no real agent.
- A protocol crate bump must stay local to one module.

## Considered options

1. Protocol crate: `agent-client-protocol` (the official Rust SDK of the `ACP` project) or a hand-written JSON-RPC client.
2. Connection ownership: one supervisor task per agent on the caller's runtime, or a dedicated OS thread with a current-thread runtime and a `LocalSet`.
3. Environment: pass the parent environment through and remove known secrets, or start from an empty environment and add an allowlist.
4. Client capabilities: offer file system and terminal, or none.

## Decision outcome

**Protocol crate.** `agent-client-protocol` **3.4.0** (crates.io `max_stable_version` and `newest_version`, verified 2026-10-21; license Apache-2.0; Rust 1.88, edition 2024; features: none, the default is empty). Option 1a: a hand-written client would copy the schema and the ordering rules of a moving protocol. Its normal dependency tree adds `agent-client-protocol-schema` 1.12.0, `agent-client-protocol-derive`, `serde_with`, `darling`, `derive_more`, `strum`, `futures` (and its parts), `futures-concurrency`, `uuid`, `tracing`, `pin-project`, `indexmap` and a few small crates, all MIT, Apache-2.0 or `MIT OR Apache-2.0` (read from `cargo metadata`); `cargo deny` was not installed where this was written, so the CI `cargo deny check` run is the final check (no new advisory ignore is added, and no `rsa`). `sqler-acp` also adds `tokio-util` 0.7.20 (feature `compat`, to adapt tokio pipes to the `futures` I/O traits of the SDK) and uses `tokio` 1.54.0 (`rt`, `process`, `sync`, `io-util`, `io-std`, `macros`, `time`), `serde` 1.0.229, `serde_json` 1.0.154, `thiserror` 2.0.21 and `zeroize` 1.9.1, all already in the workspace. The unstable features of the SDK are off, so only stable protocol v1 is spoken.

The SDK API is closure based (`Client.builder().on_receive_notification(..).on_receive_request(..).connect_with(transport, main)`) and all its futures are `Send`. `SQLer` still wraps every protocol type (profile, descriptor, updates, errors, exit info), and only `connection.rs` and the descriptor mapping import the SDK, so a later bump changes only those.

**Connection ownership.** Option 2a. `AgentHandle::launch` spawns one supervisor task (`tokio::spawn`) per agent. It owns the child process, the stderr reader and the connection. The public handle sends commands (new session, prompt, cancel, close) over an unbounded `mpsc` channel with a `oneshot` reply; streamed updates, stderr lines and the final `Exited` event go to a bounded (256) `AgentEvents` channel (the supervisor waits when it is full, which is the back pressure). Permission requests are answered in a task spawned on the connection, so the dispatch loop is never held while the user decides. The dedicated-thread design (a `LocalSet` thread per agent) was needed only if the SDK futures were not `Send`; they are, so it was rejected as extra code and an extra thread per agent. The crate must be used inside a tokio runtime that has the `time` and `process` drivers.

**Agent profiles.** `AgentProfile { id, name, command, args, env, inherit_env, install_hint }`, serde friendly so a user-defined profile can live in a TOML file (storing them is the job of the settings layer, T17; user-defined profiles are an assumption of the task). `validate` requires an id of `[a-z0-9-]+` and a command. Built-in profiles, with the package and binary names verified on npm on 2026-10-21:

| Id | Program | Install hint | `inherit_env` |
| -- | ------- | ------------ | ------------- |
| `claude-code` | `claude-agent-acp` | `npm install -g @agentclientprotocol/claude-agent-acp` | `ANTHROPIC_API_KEY`, `CLAUDE_CONFIG_DIR` |
| `codex` | `codex-acp` | `npm install -g @agentclientprotocol/codex-acp` | `OPENAI_API_KEY`, `CODEX_HOME` |

The Claude Code adapter was published as `@zed-industries/claude-code-acp` (binary `claude-code-acp`); npm now marks that name, and `@zed-industries/claude-agent-acp`, as renamed to `@agentclientprotocol/claude-agent-acp` (binary `claude-agent-acp`, 0.94.0), and `@zed-industries/codex-acp` as replaced by `@agentclientprotocol/codex-acp` (binary `codex-acp`, 2.2.2). The profile id stays `claude-code`. Running these against the real agents is pending (maintainer).

**Process lifecycle.** The program is looked up on `PATH` first (`resolve_program`; a command with a path separator is used as is; on Windows `PATHEXT` is tried, so npm `.cmd` shims are found). If it is missing, `launch` fails with `AcpError::AgentNotFound`, whose text names the profile, the program and the install hint, and nothing is spawned. The child has piped stdin, stdout and stderr and `kill_on_drop`. A reader keeps the last 64 stderr lines (each cut to 4096 characters) and forwards each as `AgentEvent::Stderr`. `initialize` must finish within `init_timeout` (30 s). Shutdown (`close`, dropping the handle, the end of the connection): stdin is closed, the process has `shutdown_grace` (2 s) to exit, then it is killed, and it is always `wait`ed for (reaped). `cancel` sends `session/cancel` and answers a pending permission request of that session with `Cancelled` at once; if the prompt has not returned after `cancel_grace` (5 s) the process is killed and the prompt resolves `StopReason::Cancelled`. A crash (stdout closed, or the child exits) makes every pending call fail with `AcpError::AgentExited { exit }` (exit code or signal, and the stderr tail), sends `AgentEvent::Exited`, resolves every `ExitWatch`, and later calls fail the same way. A launch that fails after the spawn (timeout, unsupported protocol version, an error from the agent, a crash) kills and reaps before it returns. One prompt runs per session at a time (`PromptInProgress`).

**The MCP server descriptor.** `McpServerDescriptor` is `Stdio { name, command, args, env }` (the agent starts the command and talks `MCP` over its stdio) or `Http { name, url, token }` (a loopback endpoint). These are the two transports of T15's server. The `Http` URL host must be `127.0.0.1`, `[::1]` or `localhost`, with no user info in the URL; anything else is refused with `InvalidDescriptor`, because the token is a bearer secret and must not leave the machine. The token goes only into the header `Authorization: Bearer <token>` of the `session/new` message (`MCP_AUTH_HEADER`); `McpToken` is zeroized on drop and its `Debug` prints `McpToken(***)`. An `Http` descriptor is refused with `McpTransportUnsupported` when the agent did not advertise HTTP `MCP` in `initialize`. The `args` and `env` of a `Stdio` descriptor are passed to the agent verbatim, so callers must not put a credential there; the token path is the HTTP one. SSE is not offered. *Check against ADR-0040 when T15 has merged: the header name is the one constant `MCP_AUTH_HEADER`.*

**Client capabilities.** None: `fs.readTextFile = false`, `fs.writeTextFile = false`, `terminal = false` (assumed off in M5). The agent works on the files through its own tools and uses `SQLer`'s tools for the database. A file system or terminal request from the agent is answered with JSON-RPC "method not found" by the SDK.

**Environment.** Option 3b. The child starts from `env_clear()`. Then `agent_env` adds, in this order: a fixed base allowlist from the parent (`PATH HOME USER LOGNAME SHELL LANG LC_* TERM TMPDIR TEMP TMP XDG_* HTTP_PROXY HTTPS_PROXY NO_PROXY` and the lowercase proxy names, `SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`; on Windows also `SystemRoot windir USERPROFILE APPDATA LOCALAPPDATA ProgramData ProgramFiles ComSpec PATHEXT`), then the names in the profile's `inherit_env` (the user's own login variables, not `SQLer` secrets), then the profile's literal `env`. `SQLER_*`, cloud credentials, `CARGO_*` and everything else are not passed. `agent_env` is a pure function over an injected parent environment, so it is tested without `set_var`. No `SQLer` secret goes into an argument either: the arguments are the profile's own.

The mock agent (`sqler-acp-mock-agent`, a binary of this crate built on the same SDK) drives the integration tests: handshake, streaming, permission approved and denied, cancel (also with a pending permission and with an agent that ignores cancel), crash, crash on init, the injected descriptor, the environment, reaping.

## Consequences

- Positive: one launch path with a single place for cleanup; no orphan on cancel, close, drop, timeout or crash (tests check the exit and, on Unix, that the pid is gone).
- Positive: the agent cannot read `SQLer`'s secrets from its environment; the only secret it receives is the `MCP` bearer token, in a header, on loopback.
- Positive: the public types do not expose the SDK; a bump touches `connection.rs` and `descriptor.rs`.
- Negative: grandchild processes (an npm agent spawning the CLI) are not killed; only the direct child is closed and reaped. A process group or a job object would need `unsafe` or a new crate.
- Negative: `agent-client-protocol` brings `serde_with`, `darling`, `futures`, `uuid` and `tracing` into the build of whatever depends on `sqler-acp` (T17: the `sqler-app` binary size is checked there).
- Negative: no file system or terminal capability means an agent cannot show unsaved editor buffers to itself; that is future work.
- Negative: only text content of updates is carried (images and resources become `Other`).
- Known limit: the built-in package names are from npm on 2026-10-21 and may be renamed again; a user can override a profile. Real Claude Code and Codex runs are pending (maintainer).
