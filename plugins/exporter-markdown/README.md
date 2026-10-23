# exporter-markdown

An example SQLer plugin (world `exporter-plugin`) that exports the rows of a result as a Markdown table:

```
| id | name | note |
| ---: | --- | --- |
| 1 | a\|b |  |
```

Integer, float and decimal columns are right-aligned (`---:`). Cells are written as text, null as an empty cell,
bytes as `0x` and lowercase hex; `|` and `\` are escaped and line breaks become `<br>`.

## Capability

None: the manifest declares `capabilities = []`. The host opens the file and writes the bytes the plugin returns, so
the plugin has no file access and needs no grant.

## Difference from the built-in Markdown format

SQLer has a built-in `markdown` format (label `Markdown table`). This plugin is a separate format: its id is
`plugin-exporter-markdown:markdown` and its label is `Exporter Markdown (plugin): Markdown table`, so both show up
in the export dialog. The built-in one does not right-align number columns. Both write the extension `md`.

The plugin API gives `start` no way to return bytes, so the header lines are returned by the first `push` (or by
`finish` when there were no rows).

## Build and install

```
cargo run -p xtask -- plugins build exporter-markdown
```

The installable directory is `target/plugins/debug/exporter-markdown/` (`target/plugins/release/exporter-markdown/`
with `--release`): `sqler.toml` and `exporter-markdown.wasm`. In SQLer open the plugin manager and add that directory
as a development plugin.

## Tests

The behaviour is tested from the host in `crates/sqler-plugin-host/tests/guests/exporter_markdown.rs`.
