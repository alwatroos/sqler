# ADR-0067: Connection import

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

Users arrive with connections already set up in other tools. Milestone 7 adds an import for three of them: pgAdmin
`servers.json` (R6), Azure Data Studio (R7) and Beekeeper Studio (R8); the import dialog is R9. The foreign files are
untrusted input: they can be huge, deeply nested, hostile or from a newer version of the tool. They also carry
secrets in several forms. The import must never overwrite a profile, never import a secret value and never echo file
content in an error.

Open questions: where the code lives, how a new tool is added, how a foreign entry maps to a `ConnectionSpec`, what
happens on a duplicate or a name clash, what happens to secrets, and which limits apply.

## Decision Drivers

- The import is headless and testable without a UI, a driver or a keychain.
- R7 and R8 add one module and one registry line each, without touching the core.
- AGENTS.md rules: no user info in a target, no secret in a `Debug` output, no new third-party dependency.
- A dry run (the preview the wizard shows) comes before any write.

## Considered Options and Decision Outcome

### Placement and layer

A new crate `sqler-import` at layer L2. Its dependencies are `sqler-core`, `sqler-config`, `serde_json` and
`thiserror`. It has no driver crate, no `sqler-ui` and no new third-party dependency, so
[dependencies.md](../dependencies.md) is unchanged. `sqler-config` (L1) stays the store crate. Putting the importers
there was not taken: it would mix foreign formats into the store, and R7 and R8 would share that crate. The crate
saves nothing to disk; the caller saves `connections.toml` with `ConfigDir::save_connection_store`.

### The `Source` trait and the registry

Three pure stages:

1. A `Source` (`id`, `label`, `default_locations`, `parse`) turns a bounded byte buffer into a `SourceReport`: one
   entry per foreign connection, either a mapped `ConnectionSpec` or a skip reason, plus the names of unsupported
   foreign fields, notes and the secrets the user may need to enter.
2. `plan` checks the report against the current `ConnectionStore` and the caller's list of available driver ids and
   returns an `ImportPlan`, which is the dry run.
3. `apply` adds only new connections and groups to an in-memory store and writes only the secrets the user typed.

`sources()` is the registry (shape of the `sqler-export` registry); `source(id)` looks one up. Adding a tool is one
module plus one line in `sources()`. Locations come from `default_locations` with an injected `HostEnv` (same pattern
as `sqler-config`'s path resolution), so tests never read the real environment; `discover` keeps regular files only.
The explicit path chosen by the user is the primary input; default locations are a best guess.

### Mapping to `ConnectionSpec` and groups

The pgAdmin source maps to the existing `postgres` driver id with the target
`postgres://<host>:<port>/<db>[?user=<user>]`; user info never goes into the target. TLS (`sslmode` and the three
file paths, from `ConnectionParameters` with the legacy top-level keys as fallback), a colour and an SSH tunnel
(password or key file) are mapped when valid. A value that cannot be mapped adds a note and the field is ignored; a
missing or invalid required field skips that one entry. Every other foreign key is listed in `unsupported` by name,
never by value. An unknown `sslmode`, or certificate paths without an `sslmode`, give `tls: None` and a note; the
paths are not kept. The pgAdmin group becomes a group path; group paths are capped at 8 levels and deeper levels are
folded into the last one with a note. `PassFile` and `PasswordExecCommand` are never read or run.

### Duplicates, name collisions and groups

Rules of `plan`, in this order:

1. Entries the source already skipped pass through.
2. A driver that is not in `ImportOptions::available_drivers` gives `DriverUnavailable` ("not available in this
   build"). No list of drivers is hard-coded; the caller (R9: the app's driver registry) fills it.
3. Duplicate key = (driver, target, SSH host, port, user). An existing connection with the same key gives
   `Duplicate`; an earlier entry of the same file gives `DuplicateInFile`. Both are skipped.
4. A name that equals (after trim) one in the store or an earlier planned item is renamed `<name> (2)`, `(3)`, ...
   and `renamed_from` records the old name.

`apply` calls `add_connection` only: no update of an existing id, so nothing is ever overwritten. A group path is
resolved by exact name under its parent and created when missing; an existing group is reused, never renamed or
moved. The preview shows renames and skips, so R9 can change these rules without changing the API.

### Secrets policy

No secret value is imported. A foreign file never supplies one: no decryption of stored passwords, no passfile, no
executed command. A needed secret is reported as a `ProfileSecretKind` (database password, SSH password, SSH key
passphrase). The user may enter it at apply time (`ProvidedSecret`, whose `Secret` redacts its `Debug`); it is written
to the `SecretStore` under the reference of the new connection and the keychain flag is set only then. Without an
entered secret (an empty value counts as not entered) the flag stays false and the secret is listed in
`secrets_skipped`, which covers both "not provided" and "the store refused it". When a write fails, the flag is
cleared and the failure text holds no secret and no error detail. The plan, the report and the store hold no secret.

### Caps

| Constant | Value |
| -------- | ----- |
| `MAX_FILE_BYTES` | 4 MiB |
| `MAX_ENTRIES` | 5,000 |
| `MAX_FIELD_CHARS` | 4,096 |
| `MAX_GROUP_DEPTH` | 8 |

The file is read through a length limit, so a file that grows during the read is caught. JSON nesting is limited by
`serde_json` (depth 128) and gives `Malformed`. Error texts never quote file content; `Malformed` carries line,
column and a fixed text per `serde_json` error category, never the parser message (it can quote input).

### Path validation

Paths read from a foreign file (TLS files, SSH keys) go through `foreign_path`. It refuses empty input, control
characters, more than 4,096 bytes, the prefixes `\\?\`, `\\.\`, `//?/` and `//./`, any component whose stem is a
Windows reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`, checked on every OS), and any path that
is not absolute (`~`, `$HOME`, placeholders and relative paths are not expanded). The check is on the text only; the
file is never opened or tested for existence.

### DuckDB, SQL Server and availability

A foreign `DuckDB` entry is skipped with "DuckDB is not in SQLer 1.0" (ADR-0070). Whether SQL Server or any other
engine can be added is decided by the caller's driver list, not by the crate: an engine that is not in the list is
skipped as "not available in this build".

### Out of scope

The import wizard and its dialogs (R9), Azure Data Studio (R7), Beekeeper Studio (R8) and reading the `pgadmin4.db`
database of pgAdmin. The user guide page is R10.

## Consequences

- The importers are testable with byte buffers, a `MemoryStore` and a fake secret store.
- A foreign secret can never reach the repository, a log or `connections.toml` through this crate.
- The default locations of pgAdmin are an assumption (pgAdmin does not write `servers.json` by itself); the explicit
  path is the primary input.
- A repeated key under `Servers` (the same number twice) is collapsed by the JSON reader: only the last object is
  imported and the earlier one is lost without a note. pgAdmin never writes such a file.
- The crate joins the Windows PR test subset of `ci.yml` ([ADR-0068](0068-ci-pipeline-speed.md)) because path
  semantics differ per OS.
- R9 links the crate into the app and measures the binary size effect.

## Amendment 1 (2026-10-23): the Azure Data Studio source (R7)

`crates/sqler-import/src/ads` adds the source `azure-data-studio`: the user `settings.json` of Azure Data Studio
(`datasource.connections` and `datasource.connectionGroups`). The file is JSON with comments, so a small bounded
reader first blanks `//` and `/* */` comments and trailing commas (strings are tracked; line and column numbers are
kept) and then uses `serde_json`. No new dependency. A file with neither key is `UnrecognisedFormat`; an empty list is
a valid empty report. Default locations: `<user data>/User/settings.json` with `azuredatastudio` and
`azuredatastudio-insiders` under `%APPDATA%` (Windows), `~/Library/Application Support` (macOS) or `~/.config`.

| ADS field | SQLer |
| --------- | ----- |
| `providerName` `MSSQL` | driver `sqlserver`, target `sqlserver://<host>:<port>[/<db>]?user=<user>`, default port 1433 (the connect pipeline moves `?user=` into the user info, S5; the engine accepts no query key) |
| `providerName` `PGSQL` | driver `postgres`, target `postgres://<host>:<port>/<db>?user=<user>` (options `server`, `port`, `database`), default port 5432, database `postgres` |
| other providers | skipped: "only SQL Server and PostgreSQL connections are imported" |
| `options.server` | `host`, `host,port`, `tcp:host,port`, `[v6],port`, `.` and `(local)` (localhost); `host\instance` is skipped (named instances need the SQL Browser) |
| `options.authenticationType` | `SqlLogin` (or none, with a user) is imported; `Integrated` and the Azure AD types (`AzureMFA` and others) are skipped with a reason (SQL login only, ADR-0071); the user is required |
| `options.encrypt`, `trustServerCertificate` (SQL Server only) | `true`/`Mandatory`: `verify-full`, or `require` when the certificate is trusted; `Strict`: `verify-full`; `false`/`Optional`: `prefer` (note); not set: no TLS profile (driver default `prefer`); unknown value: ignored with a note. `hostNameInCertificate` is listed as unsupported |
| `options.password` | never read; a non-empty value only adds the note "a stored password was not imported". A `DbPassword` secret is offered for the user to enter |
| `connectionName` | the name (else the `server` text, else `Azure Data Studio connection N`) |
| `groupId` and the group tree | the group path from `parentId` links; the invisible `ROOT` group is not a level; the nearest group `color` (`#rrggbb`) becomes the connection colour (the report has no group colour); an unknown group id imports at the top with a note; a cycle imports at the top with a note; deeper than 8 levels is folded into the 8th |
| every other key | listed by name (`options.<key>` for options), never by value |

Caps are those of the core: 4 MiB file, 5,000 connections and 5,000 groups, 4,096 characters per field. Whether
`sqlserver` is available is decided by the caller's driver list (`DriverUnavailable`), as for every driver.
