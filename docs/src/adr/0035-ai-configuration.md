# 0035 AI configuration: `ai.toml`, provider key references, per-connection opt-in

- Status: accepted
- Date: 2026-10-21
- Amends: [ADR-0011](0011-connection-store-format.md) (format v2) and [ADR-0012](0012-keychain-secrets.md) (provider-scoped reference)

## Context and problem statement

M5 adds AI features ([ADR-0003](0003-ai-tool-contract.md)). They need settings that no earlier file holds: the providers (Ollama, Anthropic, OpenAI, Mistral), which model serves which use case, which column names are redacted before anything leaves the machine, and the row and time caps for queries the model runs. Provider API keys are secrets and must stay in the keychain. A connection must opt in to AI one by one, and production connections need a safer default. `sqler-config` is layer L1, so the types are plain data and carry no provider client. A new file, a new secret reference and a new field on the connection store are public API and file format, so an ADR comes first.

## Decision drivers

- No secret in any file, by construction (as ADR-0011).
- AI off unless the user turns it on, per connection.
- Production is safer by default and cannot lift its caps.
- Typo-safe, versioned, hand-editable TOML (as ADR-0005).
- Older builds keep reading `connections.toml` until AI is actually used.
- `SecretStore`, `KeychainStore` and `MemoryStore` stay unchanged.

## Considered options

Where the AI settings live:

1. A new versioned file `ai.toml`.
2. An `[ai]` section of `settings.toml`.

Provider key reference:

1. A derived reference from the provider id (a new secret kind).
2. An explicit `api_key_ref = "..."` string per provider.

Version of `connections.toml`:

1. Save the lowest version that can hold the data.
2. Always write `version = 2`.

## Decision outcome

Chosen: `ai.toml`, derived references, lowest-version save.

### `ai.toml` (format v1)

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

- `settings.toml` is a flat struct owned by the settings tab, and providers are an array of tables with their own validation and version, so they get their own file (`ai.toml` in the config directory). A missing file is the default configuration.
- A provider `id` follows the connection id grammar: 1 to 64 characters of `[a-z0-9-]`, starting with a letter or digit. `kind` is `ollama`, `anthropic`, `openai` or `mistral`. `endpoint` is optional: `http://` or `https://`, a host, no `@`, `?`, `#` or whitespace, at most 2048 bytes. `models` entries are not empty.
- `[models]` assigns a provider and model to `default`, `nl-to-sql`, `explain`, `fix` and `exploration`. A use case without its own entry uses `default`. A choice must name a known provider and one of its `models`.
- Unknown keys are errors. A key such as `api_key` under `[[provider]]` is a parse error and the error text does not repeat the value. `version` missing, 0 or greater than 1 is rejected; greater than 1 reads "written by a newer SQLer".
- `AiConfig::validate` returns a typed `AiConfigProblem`, so a settings UI can check before save. Load and save both validate. The texts never contain an endpoint or key value.

### Provider keys

The file holds only `api_key_in_keychain = true`. The reference is derived from the provider id: a new secret kind `ai-provider-key`, so the account is `<provider-id>:ai-provider-key` under the same keychain service ([ADR-0012](0012-keychain-secrets.md)). A typed `ProviderKeyRef` wraps a `SecretRef` of that kind and is the value passed to `SecretStore`. Existing kinds, the `SecretRef` grammar and the store implementations do not change. Renaming is not possible (the id is the key), the same trade-off as ADR-0011.

### Per-connection opt-in (`connections.toml` format v2)

A connection may have an optional `[connection.ai]` table with `enabled` (default `false`) and `send_rows` (optional). Rows are sent to the model when `enabled && send_rows.unwrap_or(environment != prod)`: on by default outside production, off by default in production, and an explicit value wins. Without the table, AI is off.

`CONNECTIONS_FORMAT_VERSION` becomes 2. Version 1 and 2 files both load. A version 1 file that contains `ai` is rejected ("the ai table needs version = 2"). Save writes `version = 2` only when some connection has non-default AI settings and `version = 1` otherwise, so builds that only know version 1 keep working until AI is used. Once a file is saved as version 2, an older build refuses it with the "newer SQLer" message. Version 3 and above is refused by this build.

### Redaction

A fixed default list of column name fragments (`password`, `passwd`, `secret`, `token`, `api_key`, `ssn`) is always applied. `[redaction] extra` adds more. Entries are trimmed, lower-cased and case-insensitive substrings of column names. There is no way to remove a default.

### Limits

`max_rows` (default 1000, 1 to 1,000,000) and `timeout_secs` (default 30, 1 to 3600) cap the queries the model runs. `unlimited = true` lifts both caps, except on a production connection, where it is ignored and the caps always apply. This is not a load error.

## Consequences

- Good: no field can carry a key; the reference needs no validation and cannot drift.
- Good: AI settings are validated before save and independent of the settings tab.
- Good: users who never use AI keep a `version = 1` connection file.
- Bad: a file saved with `version = 2` locks out older builds.
- Bad: `http://` endpoints to remote hosts send prompts in clear text. They are allowed, because Ollama on localhost needs `http://`; the user guide says so.
- Bad: editing a connection in the form resets its AI settings to off until the settings tab carries them through. This fails safe.
- Neutral: `ai.toml` is rewritten whole on save; comments and key order are not kept.

## Pros and cons of the options

- `ai.toml`: own version and validation, no clash with the settings tab. Chosen. An `[ai]` section of `settings.toml` would put an array of tables in a flat strict struct. Rejected.
- Derived reference: no extra key, nothing to validate. Chosen. An explicit `api_key_ref` can be duplicated or drift, and invites storing a value. Rejected.
- Lowest-version save: older builds stay compatible. Chosen. Always `version = 2` locks them out for users who never use AI. Rejected.
