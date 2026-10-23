# ADR-0072: Plugin export formats

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

A plugin can declare `[[exporters]]` in its manifest, and the WIT `exporter` interface ([ADR-0055](0055-plugin-abi-wit.md))
already has `start`, `writer.push` and `writer.finish`, which return bytes for the host to write. Still open: how a
format that appears after the first frame gets into the export dialog and the CLI, how the guest's bytes meet the
partial-file protocol of [ADR-0032](0032-grid-export.md), what the user sees when a plugin fails, and how the strings
of a plugin fit the `&'static str` ids of `sqler_export::Format`.

## Decision Drivers

- The built-in formats, the six writers and every registry caller stay unchanged.
- Plugins are discovered after the first frame, so a registry built at startup would always be empty.
- The guest never gets a file, a path or a handle: it returns bytes, the host writes them.
- A failed or cancelled export leaves no target and no partial file, as for a built-in format.
- No new crates.io dependency; `sqler-ui` stays free of wasmtime.

## Considered Options

- A run-time overlay: a `FormatSource` trait asked after the built-ins, like `DriverSource` ([ADR-0061](0061-plugin-drivers.md)).
- Change `Format::id`, `label` and `extension` to return `&str`. Rejected: it touches the six built-in impls,
  `FormatEntry`, the CLI and both benches for no gain.
- A mutable registry with `register`/`unregister`. Rejected for the reason given in ADR-0061.
- A guest that writes through WASI files. Rejected: it would need a `fs` grant and bypass the partial-file protocol.

## Decision Outcome

### FormatSource

`sqler-export` gets `FormatSource` (`fn formats(&self) -> Vec<Box<dyn Format>>`, called on the UI thread, no I/O) and
`Registry::with_sources(&[Arc<dyn FormatSource>])`: the built-ins first, then each source's formats in order, and an id
that is already taken is skipped silently. `ExportError` gets `Plugin(String)`; its text is already safe for the user.
`sqler-plugin-host` provides `PluginExporters`, one process-wide source that the app attaches to the host (first attach
wins), the same way `PluginDrivers` is attached. Until a host is attached, and for a host without exporters, it offers
nothing. The export dialog builds its registry when it opens, and that snapshot travels in `WriteSpec`, so the format the
user checked is the format that writes, even if a plugin is disabled meanwhile.

### Id and label

A format of a plugin has the id `plugin-<plugin id>:<exporter id>`, the label `<plugin name> (plugin): <label>` (each
part passed through the one-line cleaner of the ADR-0060 addendum, capped at 64 bytes, so no control characters or
bidirectional marks reach the dialog) and the extension from the manifest. Built-in ids never start with `plugin-`. A
plugin exporter needs no capability (the `exporter` interface is ungated); it is offered while its plugin is loaded and
not disabled. Plugin formats take no options.

### Writing

`PluginFormat::create` returns a `PluginWriter`. `begin` calls `start`, each chunk the `Exporter` passes in (at most
1,024 rows) is one `push`, and `finish` calls `finish`, writes the bytes into the sink the `Exporter` gave, flushes it
and releases the resource. A writer dropped without `finish` releases the resource through a call whose result is
ignored. Every call carries a ticket (generation, instance id) so a restarted, disabled or re-approved plugin is
noticed. The bytes go through the sink, so the partial-file protocol, the progress counters and the cancel checks of
`Exporter` apply unchanged.

### Blocking wait

`RowWriter` is synchronous and runs on the export's blocking thread. `PluginHost::wait_blocking` spawns the call on the
host's runtime and blocks on a `std::sync::mpsc` channel. It must never be called from a thread of the host's runtime;
the app host has its own runtime, the UI writer runs on the blocking pool, and the CLI (whose export runs on a
current-thread runtime) starts a dedicated runtime with one worker for the plugin host. Cancel is checked between calls,
so it can lag by up to one guest call, bounded by the export batch limit of 5 s.

### Interning

`Format::id`, `label` and `extension` return `&'static str`. Each distinct plugin string is leaked once, kept in a set
in `PluginExporters`, and reused when the same plugin is listed again. The amount is bounded by the distinct exporter
ids, labels and extensions loaded in one process; it is not freed. This is cheaper and safer than changing the trait.

### Texts

Failures map to fixed texts, with `{f}` the format label and `{p}` the plugin id: "the export format `{f}` of plugin
`{p}` crashed" (trap), "... did not answer in time", "... ran out of memory", "... stopped (the plugin was restarted,
disabled or its permissions changed)", "... is not available", and for a guest `plugin-error` "... failed: <message>"
with the message cleaned to one line and capped at 256 bytes. A cancel is `ExportError::Cancelled`. No text includes
engine output, a path or SQL; the guest only sees column names, types and the rows being exported.

### CLI

`sqler-cli export --format plugin-<id>:<fmt>` (build with the `plugins` feature) reads `<config>/plugins` and the stored
grants and prefs, never prompts, and only scans the plugin folder when the format id starts with `plugin-`. A plugin
format is treated as binary: `--output -` is refused.

## Consequences

- The dialog and the CLI show plugin formats without a restart of the registry; built-ins are untouched.
- A plugin can crash or hang an export, never write a file; the user gets a fixed text and no partial file.
- The CLI may run two plugin hosts (drivers and exporters) in one process, each instantiating its plugins. Acceptable
  for a one-shot command.
- Interned strings are never freed.
- Throughput of a plugin format is below a built-in one because every chunk crosses the sandbox; the bench
  `plugin_export` measures it.
