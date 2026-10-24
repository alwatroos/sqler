# Exporters

A plugin can add a file format to **Export Results...** and to `sqler-cli export`. The decision is recorded in
[ADR-0072](../adr/0072-plugin-export-formats.md). The plugin never gets a file, a path or a handle: it turns rows into
bytes and SQLer writes them, so the partial-file rule (a cancelled or failed export leaves no file) and the progress
line work as for a built-in format.

Plugin formats need a build with the `plugins` feature. Release builds have it; a local `cargo build` needs
`--features plugins`.

## Declare it

```toml
[plugin]
id = "my-formats"
name = "My formats"
version = "1.0.0"
api = "0.1"

[entry]
component = "my_formats.wasm"

[[exporters]]
id = "tsv"
label = "Tab-separated"
extension = "tsv"
```

An exporter needs no capability, so the grant key covers it and the approval prompt shows it. Build the component for
the world `exporter-plugin` (or `full`).

## The interface

The WIT `exporter` interface has one function and one resource:

- `start(format, columns)` opens an export in the declared exporter `id` and gets the column names and types. It returns a
  `writer`.
- `writer.push(rows)` encodes one batch (at most 1,024 rows) and returns the bytes to write.
- `writer.finish()` ends the export and returns the trailing bytes.

Any call may return a `plugin-error`; the export then fails, and no target file is left. Values arrive as the WIT
`value` variant; `null` is a value, not an absence of a call.

## What the user sees

| | |
| - | - |
| Id | `plugin-<plugin id>:<exporter id>` (what `sqler-cli export --format` takes) |
| Label | `<plugin name> (plugin): <label>`, cleaned to one line of at most 64 bytes per part |
| Extension | as declared (`[a-z0-9]{1,16}`) |
| Options | none; the dialog shows no option fields |
| Listed | while the plugin is loaded and not switched off |

When a call fails the export ends with a fixed text, for example "the export format `Tab-separated` of plugin
`my-formats` crashed" (trap), "did not answer in time" (more than 5 s for one call), "ran out of memory", "stopped (the
plugin was restarted, disabled or its permissions changed)" or "failed: " followed by your message. Your message is cleaned to one
line and cut at 256 bytes, so keep it short and free of paths and data.

## Limits

- One call is limited to 5 s, so a **Cancel** can take up to that long to take effect.
- Encode in `push` and keep `finish` small; every chunk crosses the sandbox, so throughput is below a built-in format.
  The bench `plugin_export` measures it.
- The guest sees only column names, types and the rows being exported, never the SQL, a path or a secret.

The fixture `plugins/fixtures/exporter` is a complete example.
