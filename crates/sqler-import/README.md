# sqler-import

Import of connection profiles from other tools. Sources: pgAdmin `servers.json` and the Azure Data Studio user `settings.json`.

Status: M7 (R6, R7): headless core, the pgAdmin source and the Azure Data Studio source; the import dialog is R9

| Source id | Tool | File | Maps to |
| --------- | ---- | ---- | ------- |
| `pgadmin` | pgAdmin 4 | exported `servers.json` | `postgres`: host, port, database, user, TLS, colour, group, SSH tunnel |
| `azure-data-studio` | Azure Data Studio | `User/settings.json` (comments and trailing commas accepted) | `sqlserver` (`MSSQL`) and `postgres` (`PGSQL`), SQL login only: host, port, database, user, `encrypt` to TLS mode, group tree and colour; Windows and Azure AD authentication, other providers and named instances are skipped with a reason |

Layer: L2 (depends on `sqler-core` and `sqler-config`; no driver crate, no `sqler-ui`)

API (pure, no async; `apply` blocks on the keychain, so async callers use `spawn_blocking`):

- `Source` (`id`, `label`, `default_locations`, `parse`), `sources()` (the registry) and `source(id)`. A new tool is one module and one line in `sources()`.
- `discover(source, env)` keeps the default locations that are regular files; `HostEnv` and `HostOs` are injected, so tests never read the real environment. `read_source(path)` is a bounded read.
- `SourceReport` / `SourceEntry`: one entry per foreign connection, either a mapped `ConnectionSpec` or a `SkipReason`, with the names (never values) of unsupported fields, notes and the secrets the user may need to enter.
- `plan(report, store, opts)` gives an `ImportPlan`, the dry run: `DriverUnavailable` (from `ImportOptions::available_drivers`), `Duplicate` and `DuplicateInFile` skips, and `<name> (2)` renames.
- `apply(plan, store, secrets, provided)` adds only new connections and groups to the in-memory store and writes only the secrets in `provided`. Saving `connections.toml` stays with the caller.
- `foreign_path` validates a path read from a foreign file (absolute only, no device names or `\\?\` prefixes) without opening it.
- Caps: `MAX_FILE_BYTES` (4 MiB), `MAX_ENTRIES` (5,000), `MAX_FIELD_CHARS` (4,096), `MAX_GROUP_DEPTH` (8). `ImportError` texts never quote file content.

Foreign files never supply secret values. No stored password is decrypted, no passfile is read and no command is run.

See [ADR-0067](../../docs/src/adr/0067-connection-import.md).
