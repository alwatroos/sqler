# Completion providers

A plugin can add rows to the SQL editor's completion popup. The built-in popup is not
changed and never waits for a plugin: it opens as it always did, and the plugin rows are added
when they arrive. The design note is in
[ADR-0026](../adr/0026-editor-intellisense.md#note-p11).

Completion providers need a build with the `plugins` feature (off by default in the shipped
binary). A completion provider needs no capability.

## Manifest

```toml
[completion]
drivers = ["sqlite", "postgres"]
```

`drivers` is optional. A non-empty list limits the plugin to those engine ids (the driver of
the tab's connection). An empty or missing list matches every driver. A tab with no driver
only reaches plugins whose list is empty.

## The world

The component exports the WIT `completion` interface (world `completion-plugin`, package
`sqler:plugin@0.1.1`). The host calls `complete` with:

| Field | Content |
| - | - |
| `connection` | always `none` |
| `driver` | the engine id of the tab's connection, if it has one |
| `text` | the statement the cursor is in |
| `cursor` | byte offset of the cursor in `text` |

The prefix is `text[..cursor]`. `text` is the statement around the cursor, not the whole
buffer, and at most 64 KiB; with a longer statement nobody is asked.

## Answer

A list of items: `label`, `kind` (keyword, table, column, function, snippet or other), an
optional `insert-text` and an optional `detail`. The host cleans every text to one line
(control and bidirectional characters are removed) and then applies these caps:

| Item | Cap |
| - | - |
| Items per answer | 50 (the rest is dropped) |
| `label` | 128 bytes |
| `detail` | 256 bytes |
| `insert-text` | 1024 bytes |

An item whose label is empty after cleaning is dropped. An empty `insert-text` becomes the
label. The editor has no snippet engine, so the insert text is inserted as plain text.

## Deadline and faults

A call has 250 ms. A call that runs longer is stopped, and its answer is dropped. A timeout
or a trap counts as a fault; after three faults the host disables the plugin (the same rule
as every other call). At most one completion call per plugin is in flight: while a plugin is
busy, it is skipped for that popup. A plugin without an approved grant, switched off, or with
a data directory error is not asked.

## Ranking and source marking

The plugin rows come after all built-in rows. Plugins are ordered by id, and each plugin keeps
its own order. A row whose label and insert text equal a built-in row or an earlier plugin
row is dropped. At most 100 plugin rows are shown. Every plugin row has the plugin's name in
front of its detail (`Name · detail`), so the user can tell where it came from.

The answer is kept for the place it was asked for (the same text and cursor). When it
arrives and the cursor has not moved, the popup is asked for again and now includes the rows.
Once the cursor or the text moves, the answer is dropped.
