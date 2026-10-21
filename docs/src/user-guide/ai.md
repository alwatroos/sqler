# AI

This page describes how to configure the AI features of `SQLer`. The settings live in `ai.toml` in the
configuration directory, next to `connections.toml`. If the file does not exist, the defaults below
apply and AI is off for every connection. SQLer never writes an API key to this file or to any other
file. See [ADR-0035](../adr/0035-ai-configuration.md) for the design.

## Configuration

```toml
version = 1

[[provider]]
id = "local"
kind = "ollama"
endpoint = "http://127.0.0.1:11434"
models = ["qwen2.5-coder:7b"]
api_key_in_keychain = false

[models]
default = { provider = "local", model = "qwen2.5-coder:7b" }

[redaction]
extra = ["iban"]

[limits]
max_rows = 1000
timeout_secs = 30
unlimited = false
```

Unknown keys are errors. The error names the file and the problem but never repeats an endpoint or a
key.

### Top level

| Key | Required | Rule |
| --- | -------- | ---- |
| `version` | yes | `1`. Missing, 0 or any other number is rejected; a larger number reads "written by a newer SQLer". |
| `[[provider]]` | no | a model provider, below |
| `[models]` | no | which model serves which use case |
| `[redaction]` | no | extra column names to redact |
| `[limits]` | no | caps for queries the model runs |

### `[[provider]]`

| Key | Required | Rule |
| --- | -------- | ---- |
| `id` | yes | 1 to 64 lowercase letters, digits and `-`, starting with a letter or digit; unique among providers |
| `kind` | yes | `ollama`, `anthropic`, `openai` or `mistral` |
| `endpoint` | no | `http://` or `https://` with a host; no `@`, `?`, `#` or spaces; at most 2048 bytes; `http://` is rejected for a provider with an API key unless the host is local |
| `models` | no | the model names this provider offers (default empty; needed for any `[models]` choice); none may be empty |
| `api_key_in_keychain` | no | `true` when the API key is in the system keychain; default `false` |

A key such as `api_key` in this table is rejected. Store the key in the keychain, see
[Secrets](secrets.md#provider-api-keys).

An `http://` endpoint to a host other than your own machine sends prompts in clear text. SQLer allows
it for a provider without an API key, because an Ollama server may need `http://`. For a provider with
an API key the key itself would travel in clear text, so SQLer rejects `http://` unless the host is
`localhost`, `127.0.0.1` (any `127.x.x.x`) or `[::1]`. Use `https://` for anything remote.

### `[models]`

| Key | Required | Rule |
| --- | -------- | ---- |
| `default` | no | `{ provider = "<id>", model = "<name>" }`, used by any use case without its own entry |
| `nl-to-sql` | no | the model that turns a question into SQL |
| `explain` | no | the model that explains a statement |
| `fix` | no | the model that fixes a failing statement |
| `exploration` | no | the model that explores a database with tools |

`provider` must be the `id` of a `[[provider]]`, and `model` must be one of its `models`.

### `[redaction]`

| Key | Required | Rule |
| --- | -------- | ---- |
| `extra` | no | more column names to redact; each 1 to 64 characters, no control characters |

Columns whose name contains `password`, `passwd`, `secret`, `token`, `api_key` or `ssn` are always
redacted. The match is a substring of the column name that ignores case and separators (`_`, `-`, spaces), so `user_password`, `apiKey` and `api-key` match.
Your `extra` entries are trimmed, lower-cased and matched the same way; duplicates are dropped. The
built-in list cannot be shortened: a `defaults` key is rejected.

### `[limits]`

| Key | Required | Default | Rule |
| --- | -------- | ------- | ---- |
| `max_rows` | no | `1000` | 1 to 1,000,000; the most rows a model-run query returns |
| `timeout_secs` | no | `30` | 1 to 3600; the longest such a query may run |
| `unlimited` | no | `false` | `true` lifts both caps |

On a connection marked `prod`, `unlimited` is ignored and the caps always apply.

### Per connection

AI is off until you turn it on for a connection with a `[connection.ai]` table in
`connections.toml`, see [Connections](connections.md). With `send_rows` unset, row data may be sent to
the model on every connection not marked `prod` (including unlabelled ones) and is not sent on `prod`. An explicit `send_rows` wins.

## Audit log and redaction

Every AI tool call is recorded in `ai-audit.jsonl` in the config directory (cell values are never stored), and listed columns are hidden from the model. See [Safety](../ai/safety.md).

Redaction works on names. It does not catch a flagged column that is renamed before the model sees it: through a view (`create view v as select password as p ...`), a `union` behind a dummy first select, a CTE column list, or a Postgres whole-row reference such as `row_to_json(c)`. It also does not look inside cells, so JSON text with a `password` key is sent as it is. Use `send_rows = false` on connections where this matters. On Unix the audit file is readable by you only (0600); on Windows it relies on your profile's permissions.
