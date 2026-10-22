# MCP server

`sqler-cli mcp` serves SQLer's AI tools to any Model Context Protocol client, such as Claude Code or
Claude Desktop. The design is in [ADR-0040](../adr/0040-mcp-server.md).

```text
sqler-cli mcp [--stdio | --http[=PORT]] [--token-file PATH] ([--driver ID] TARGET)...
```

Stdio is the default. `--driver` applies to the targets after it (default `sqlite`). `--http` without a
port binds `127.0.0.1` on a free port. Connections are named `db1`, `db2`, ... in the order given, and
the target text never appears in a name, error or log line.

## What is exposed

The same tools as the in-app chat ([tool contract](tool-contract.md)): catalog tools, `run_query`,
`sample_rows`, `explain_query` and the others. The two UI-only tools (`open_in_editor`,
`open_results_tab`) are not listed and calling them is an error.

Every call has the same guards as in the app ([safety](safety.md)):

- Only `Read` statements run. `sqler-cli mcp` has no approver, so a write, DDL or statement that
  cannot be classified is refused with `isError` and the code `no_approver`.
- Row and time caps apply, and values are redacted per `ai.toml`.
- Results are wrapped in the untrusted-data envelope. The client must treat the text as data, not as
  instructions.
- Each call is written to `ai-audit.jsonl` in the config directory with `"surface":"mcp"`. A malformed
  `ai.toml` stops the server instead of falling back to defaults.
- Up to 4 calls run at once; the others wait.

A call the client cancels before it finishes (`notifications/cancelled`, a dropped HTTP connection or
shutdown) is audited with the outcome `cancelled` and gets no response (see ADR-0040).

## Protocol

Revisions 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05 are supported. The 2026-07-28 revision
(no `initialize`, stateless) is not. Only tools are offered: no resources, prompts, logging or progress.

## Stdio

The client starts the process and talks over its stdin and stdout. Nothing but protocol messages is
written to stdout. A line over 1 MiB ends the session.

Claude Code:

```sh
claude mcp add sqler -- sqler-cli mcp /home/me/data.db
```

Use absolute database paths in client configurations: a relative path resolves against the client's
working directory, and the SQLite driver creates a missing file, so a wrong path silently serves an
empty database.

Claude Desktop and other clients that read an `mcpServers` object:

```json
{
  "mcpServers": {
    "sqler": {
      "command": "sqler-cli",
      "args": ["mcp", "/home/me/data.db"]
    }
  }
}
```

Several databases, with a driver per group of targets:

```json
{
  "mcpServers": {
    "sqler": {
      "command": "sqler-cli",
      "args": ["mcp", "/home/me/data.db", "--driver", "postgres", "postgres://user@localhost/app"]
    }
  }
}
```

A target with a password is visible in the client's configuration file. Prefer a driver URL without
a password where the driver allows it.

## HTTP

```sh
sqler-cli mcp --http 8765 ./data.db
```

The server binds `127.0.0.1` only; there is no way to bind another address. Requests must carry
`Authorization: Bearer <token>`, a `Host` of `127.0.0.1`, `localhost` or `[::1]` with the bound port, and
no non-loopback `Origin`. The endpoint is `http://127.0.0.1:PORT/mcp`. At most 64 sessions are open at once.

### Token

- `SQLER_MCP_TOKEN` sets the token (32 to 512 visible ASCII characters).
- `--token-file PATH` reads the token from a file. If the file does not exist, it is created with
  mode 0600 and a generated token. An existing file readable by group or others is refused
  (`chmod 600` it). This holds on Unix only: on Windows the file inherits the ACL of its directory and
  the permission check is skipped, so keep it in a directory only you can read. It needs `--http`; with `--stdio` it is a usage error. Giving both the variable
  and the flag is a usage error (exit 2).
- With neither, a token is generated and printed once on stdout in the start block, together with the URL.
  It is never written to stderr or logs.

Generic client configuration:

```json
{
  "mcpServers": {
    "sqler": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

Claude Code:

```sh
claude mcp add --transport http sqler http://127.0.0.1:8765/mcp \
  --header "Authorization: Bearer <token>"
```

Keep the token file readable by you only, and do not paste the token into shared notes or scripts.

## Troubleshooting

| Symptom | Cause |
| ------- | ----- |
| `401` | Missing or wrong token, or a scheme other than `Bearer`. |
| `403` | The `Host` or `Origin` is not loopback, or the port does not match. Use `127.0.0.1`. |
| `404` | The path is not `/mcp`. |
| `405` | `GET` is not offered; the server answers `POST` and `DELETE` only. |
| `413` | The request is over 1 MiB. |
| `503` | 64 sessions are open. Close one (`DELETE`) or wait for idle ones to expire. |
| `isError` with `no_approver` | The statement is not a read. Writes are not available over `sqler-cli mcp`. |
| Client reports an unsupported protocol version | It speaks only 2026-07-28. Use a client that also offers an earlier revision. |
| Server exits with code 1 at start | A target could not be connected, or `ai.toml` is malformed. |
