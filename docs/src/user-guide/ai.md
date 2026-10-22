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

## In the app

AI is off until you turn it on for a connection. Nothing is sent to a provider by setting one up.
Setting up, checking and turning AI on all happen in four places.

### Settings > AI

Open Settings (`settings:open` or `mod+,`) and scroll to **AI**; the command `ai:open-settings` ("AI:
Open Settings") or the **AI settings** button of the AI view jumps there.

- **Providers.** One row per provider with Edit and Remove. Under it, "Add provider" has one button per
  kind compiled into your build (Ollama runs on this computer and is the first). The editor has Id
  (read-only when editing), Kind, Endpoint (empty uses the default, `http://127.0.0.1:11434` for
  Ollama), Models (comma-separated) and, for Anthropic, OpenAI and Mistral, the API key in a masked
  field. Save writes `ai.toml` and puts the key into the keychain; afterwards the field is empty and
  reads "Stored in the keychain" with **Remove key**. **Test connection** asks the provider for its
  models (15 s limit) and shows "Testing...", "Connected: N models" with **Use listed models**, or
  "Failed: ..." (never with your key or endpoint text). Removing a provider clears the model choices
  that name it and deletes its key.
- **Models.** Default, NL to SQL, Explain, Fix and Exploration, a `provider/model` of the
  configured providers. The Default entry can be "None"; every other entry can be "Default", which
  means the Default entry serves it.
- **Redaction.** The built-in column names are listed read-only; extra names go in one field,
  separated by commas or new lines.
- **Limits.** Max rows and Timeout (s) within the ranges above, and "Lift limits (never on prod)".

If `ai.toml` cannot be read, the section says so and refuses edits until you fix or remove the file;
SQLer never overwrites it. Without a configuration directory, changes last for the session and the
section says so (keys still go to the keychain).

![The AI section of the Settings page](../screenshots/after/ai-settings-dark.png)

The provider editor, with the masked API key and **Test connection**:

![The provider editor](../screenshots/after/ai-provider-editor-dark.png)

### The AI view

The **AI** item of the activity bar (above Settings) or `workbench:show-ai` (`Cmd/Ctrl+Shift+A`) shows
the side view: the AI state of the active tab's connection (or the selected one) with the provider,
model, "local"/"remote" and whether result rows may go along, and the button **Turn on for this
connection** / **Turn off** (`ai:toggle-connection`); the list of providers with the text "key stored" when a key
is stored; and the buttons **AI settings** and **Test provider** (`ai:test-provider`). Without a
provider it explains how to set one up.

![The AI view](../screenshots/after/ai-side-view-dark.png)

![The AI view before a provider is set up](../screenshots/after/ai-side-view-empty-dark.png)

### The status bar

The status bar shows the state for the active connection: "AI off", "AI not set up" (on, but no
provider), "{provider} · local", "{provider} · remote" or "{provider} · local +remote" (a model that
uses a local provider and another that uses a remote one). The tooltip names the provider, its kind and
model, and says whether the schema and result rows go to this computer or to a remote service. A click
shows the AI view. A provider counts as local when its endpoint host is `localhost` or a loopback
address, or when it is Ollama without an endpoint.

### The connection dialog

The connection form has an **AI** block: Off/On and, when on, **Send result rows** (Environment
default, Yes, No), the same as `[connection.ai]` in [Connections](connections.md). Editing a connection
keeps these settings.

![The AI block of the connection dialog](../screenshots/after/connection-dialog-ai-dark.png)

### Commands

| Command | Palette name |
| ------- | ------------ |
| `workbench:show-ai` | View: Show AI |
| `ai:open-settings` | AI: Open Settings |
| `ai:add-provider` | AI: Add Provider |
| `ai:test-provider` | AI: Test Provider |
| `ai:save-provider` | AI: Save Provider |
| `ai:cancel-provider` | AI: Close Provider Editor |
| `ai:edit-settings` | AI: Edit Settings |
| `ai:toggle-connection` | AI: Turn On or Off for This Connection |
| `ai:open-audit-log` | AI: Open Audit Log |
| `ai:load-more-audit` | AI: Load More Audit Entries |
| `ai:copy-audit-sql` | AI: Copy SQL of Audit Entry |
| `workbench:show-audit` | View: Show Audit Log |

Only `workbench:show-ai` has a default shortcut. See the [placement decisions](../adr/0050-ai-surfaces-in-the-workbench.md).

## Audit log and redaction

Every AI tool call is recorded in `ai-audit.jsonl` in the config directory (cell values are never stored), and listed columns are hidden from the model. See [Safety](../ai/safety.md).

### The audit log

The Audit panel lists the tool calls recorded in `ai-audit.jsonl`, newest first. It is a tab of the bottom panel, next to Results, Messages and History. Open it with the **Audit** tab, the **Audit log** button of the AI view, or the commands `ai:open-audit-log` ("AI: Open Audit Log") and `workbench:show-audit` ("View: Show Audit Log"). The log is read when you open the panel, never at startup; if SQLer restores the panel from the last session, it shows a **Load audit log** button first.

![The Audit panel](../screenshots/after/ai-audit-dark.png)

Each row shows:

- **When**: the age of the call (`just now`, `N min ago`, `N h ago`, `N d ago`).
- **Surface**: `In-app assistant`, `In-app session`, `MCP` or `ACP agent`.
- **Connection**: the connection name, or `—` when the call had none.
- **Tool**: the tool that was called.
- **SQL**: the first line of the statement, cut at 200 characters, with `+N` when the call had more statements. SQL is shown as stored: values that were masked stay masked (`update acct set ssn = ?`).
- **Decision**: `Not needed`, `Approved`, `Denied`, `Refused` or `Not asked`. Denied and Refused are shown in the danger colour, Approved in the success colour.
- **Result**: `N rows · size` for a successful call, `Error: <code>` for a failed one, and `Cancelled` for a call that was dropped before it finished, and the outcome name otherwise.

The toolbar has four filters: connection (**All connections** or a saved connection), surface, tool and decision. Opening the panel sets the connection filter to the connection of the active tab, or of the selected sidebar connection; choose **All connections** to see every call. Changing a filter reads the log again from the newest entry. The status line shows the number of entries, `Loading…` while a read runs, an error if the read failed, and a note when lines of the file were skipped (damaged lines) or are newer than this version understands.

A page holds 200 entries. When there are more, **Load more** (`ai:load-more-audit`) appends the next page. Click a row to select it, then **Copy SQL** (`ai:copy-audit-sql`) copies its statements, joined with `;` and a newline. The command is available only when the selected entry has SQL.

The panel is read-only: it has no way to edit or delete entries. It never shows the tool arguments (except the `sql` text), the text of error messages, cell values or API keys.

Redaction works on names. It does not catch a flagged column that is renamed before the model sees it: through a view (`create view v as select password as p ...`), a `union` behind a dummy first select, a CTE column list, or a Postgres whole-row reference such as `row_to_json(c)`. It also does not look inside cells, so JSON text with a `password` key is sent as it is. Use `send_rows = false` on connections where this matters. On Unix the audit file is readable by you only (0600); on Windows it relies on your profile's permissions.
