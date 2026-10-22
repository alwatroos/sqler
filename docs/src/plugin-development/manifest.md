# Manifest

Every plugin has a `sqler.toml` at the root of its directory. The file is at most 64 KiB, UTF-8,
with a nesting depth of at most 16. Unknown keys are errors. The first error wins; checks run
in document order.

## Example

```toml
[plugin]
id = "row-counter"
name = "Row Counter"
version = "1.2.0"
api = "0.1"
description = "Shows the row count of the last result."
authors = ["Jane Doe"]
license = "MIT"
capabilities = ["db.read", "ui.panel", "fs.read:data/cache"]

[entry]
component = "row_counter.wasm"

[[commands]]
id = "count-rows"
title = "Count rows"

[[panels]]
id = "stats"
title = "Row statistics"

[[status-items]]
id = "rows"
tooltip = "Rows in the last result"
alignment = "left"

[[menus]]
command = "count-rows"
location = "results"

[[settings]]
key = "sample.size"
label = "Sample size"
default = 100
```

A declarative plugin (no `[entry]`) that wraps a host-engine driver:

```toml
[plugin]
id = "duckdb-shell"
name = "DuckDB"
version = "0.1.0"
api = "0.1"
capabilities = ["driver.host:duckdb"]

[[drivers]]
id = "duckdb"
name = "DuckDB"
kind = "host"
engine = "duckdb"
```

## Tables and keys

`[plugin]` (required)

| Key | Type | Notes |
| --- | ---- | ----- |
| `id` | string | Plugin id, 2 to 64 chars, `[a-z][a-z0-9]*(-[a-z0-9]+)*` |
| `name` | string | At most 80 chars |
| `version` | string | Semver |
| `api` | string | `M.m` (patch 0) or `M.m.p`; checked against the host API version before the plugin is instantiated |
| `description` | string, optional | At most 1024 bytes |
| `authors` | list of strings, optional | Each at most 80 chars, same text rules as names |
| `license` | string, optional | |
| `capabilities` | list of strings | May be empty; at most 32; see [Capabilities](capabilities.md) |

`[entry]` (optional): `component`, a relative path that must end in `.wasm`. It follows the scope-segment rules
(no `..`, no leading `/`, no segment starting or ending with `.`).

Contributions (each list has at most 64 entries; ids are unique per kind):

| Table | Keys |
| ----- | ---- |
| `[[commands]]` | `id`, `title` |
| `[[panels]]` | `id`, `title` |
| `[[status-items]]` | `id`, `tooltip` (optional, at most 200 chars), `alignment` (optional, `left` or `right`, default `right`) |
| `[[menus]]` | `command` (a declared command id), `location` (`editor`, `results` or `object-browser`), `object-kinds` (optional, `object-browser` only) |
| `[[exporters]]` | `id`, `label`, `extension` (`[a-z0-9]{1,16}`) |
| `[completion]` | `drivers` (optional list of engine ids; empty means all) |
| `[[ai-tools]]` | `name` (`[a-z][a-z0-9_]{0,47}`), `description`, `input-schema` (an inline table, at most 16 KiB; converted to JSON, datetimes become strings) |
| `[[drivers]]` | `id`, `name`, `kind` (`host` or `guest`), `engine` (host only), `dialect` (optional) |
| `[[settings]]` | `key` (`[a-z][a-z0-9_.-]{0,63}`), `label`, `description` (optional), `default` (bool, integer, float or string; the type is inferred) |

Names, titles and labels are at most 80 chars. Text must not be empty and must not contain control characters
(C0, C1) or bidirectional marks (U+202A to U+202E, U+2066 to U+2069, U+200E, U+200F, U+061C). The command id the
host uses is `plugin:<plugin>:<command>`.

## Cross rules

- `[[panels]]` need `ui.panel`.
- `[[ai-tools]]` need `ai.tool`.
- A host driver needs `driver.host:<its engine>`; a guest driver needs `driver.guest`.
- Anything except host drivers and settings needs `[entry]`.
- `db.write` needs `db.read` declared as well.
- Commands, status items, menus, exporters and completion need no capability; they are
  declared in the manifest, so the grant key covers them and the approval prompt shows them.

## Errors

`ManifestError` variants (the `field` is a dotted path such as `panels[1].title`):

| Variant | Meaning |
| ------- | ------- |
| `TooLarge`, `NotUtf8`, `Syntax`, `TooDeep` | The file is over 64 KiB, not UTF-8, not TOML, or nested too deep |
| `MissingField`, `UnknownField`, `WrongType` | Structure errors |
| `BadId`, `BadVersion`, `BadApi`, `BadText`, `TooLong` | A value is malformed |
| `Capability` | A capability string failed to parse (carries its index and `CapabilityError`) |
| `DuplicateCapability`, `CapabilityRequires` | Repeated capability; `db.write` without `db.read` |
| `DuplicateContribution`, `UnknownCommand` | Repeated id; a menu names an undeclared command |
| `MissingCapability`, `MissingEntry` | A cross rule is broken |
| `BadEntryPath`, `TooMany`, `BadSchema` | Bad `component` path, too many entries, invalid input schema |

The manifest also fixes the API version: a plugin whose `api` the host does not support is
refused before it is instantiated (`CompatError::Incompatible`).
