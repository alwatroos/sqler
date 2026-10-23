# WIT reference

The plugin ABI is the WIT package `sqler:plugin@0.1.1` ([ADR-0055](../adr/0055-plugin-abi-wit.md)). The source of truth is
`crates/sqler-plugin-api/wit/`. This page lists every interface, function and type of that package. A test
(`cargo test -p xtask --test wit_reference`) fails when this page and the WIT disagree, or when a capability cell
disagrees with the gate in `sqler-plugin-api`.

How to read the tables:

- **Host import**: the plugin calls it, SQLer implements it.
- **Plugin export**: SQLer calls it, the plugin implements it.
- The capability column is what the host call needs: `none`, one capability from [Capabilities](capabilities.md), or for
  `query.run` the query rule. A `none` call still needs the contribution to be declared in `sqler.toml`.
- A type row has `-` in the capability column. Record fields are written `name: type`.
- `result<T, plugin-error>` is the error type of every fallible call. `option<T>` is "may be absent".
- A name written with a leading `%` in the WIT (`%list`, `%type`) is a keyword; the `%` is not part of the name.

## Worlds

A plugin component is built for one world. Every world includes `imports` through `base`, so the host interfaces `log`,
`settings`, `catalog`, `query` and `ui` are always linked; an ungranted call fails with `capability-denied`.

| World | Adds |
| ----- | ---- |
| `imports` | The host side: imports `log`, `settings`, `catalog`, `query`, `ui`. |
| `base` | `imports` plus the export `lifecycle`. The minimum of every plugin. |
| `ui-plugin` | `base` plus the export `ui-handler`: commands, panels and status items. |
| `ai-tool-plugin` | `base` plus the export `ai-tools`. |
| `exporter-plugin` | `base` plus the export `exporter`. |
| `completion-plugin` | `base` plus the export `completion`. |
| `driver-plugin` | `base` plus the export `driver`. |
| `driver-catalog-plugin` | `driver-plugin` plus the export `driver-catalog` (0.1.1). |
| `full` | Every export. The host does not instantiate a component through this world; it looks up each export separately. |

## `log`

Host import. Logging into the host log.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `log.level` | `enum`: `trace`, `debug`, `info`, `warn`, `error` | - |
| `log.write` | `func(level: level, message: string)` | none |

## `settings`

Host import. Plugin settings declared in the manifest.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `settings.setting-value` | `variant`: `boolean(bool)`, `integer(s64)`, `float(f64)`, `text(string)` | - |
| `settings.get` | `func(key: string) -> option<setting-value>`; `none` when the key is unknown | none |

## `catalog`

Host import. Read access to the catalog.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `catalog.connections` | `func() -> result<list<connection-info>, plugin-error>` | db.read |
| `catalog.children` | `func(connection: string, parent: object-path) -> result<list<db-object>, plugin-error>` | db.read |
| `catalog.columns` | `func(connection: string, table: object-path, name: string) -> result<list<column-info>, plugin-error>` | db.read |

## `query`

Host import. Guarded query execution. Reads need `db.read`, other statements `db.write` (ADR-0056).

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `query.query-request` | `record`: `connection: string`, `sql: string`, `params: list<value>`, `max-rows: option<u32>`, `timeout-ms: option<u32>` | - |
| `query.query-result` | `record`: `columns: list<column>`, `rows: list<list<value>>`, `truncated: bool`, `rows-affected: option<u64>` | - |
| `query.run` | `func(request: query-request) -> result<query-result, plugin-error>` | db.read or db.write (query rule) |

## `ui`

Host import. Requests to the host UI.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `ui.refresh-panel` | `func(panel: string)`; asks the host to render a panel again | ui.panel |
| `ui.set-status` | `func(item: string, text: string)`; sets the text of a status item | none |
| `ui.notify` | `func(message: string)`; shows a short notification | none |

## `lifecycle`

Plugin export. Start-up.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `lifecycle.init` | `func() -> result<_, plugin-error>`; called once after instantiation | none |

## `ui-handler`

Plugin export. Commands, panels and status items.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `ui-handler.run-command` | `func(command: string, context: command-context) -> result<_, plugin-error>` | none |
| `ui-handler.render-panel` | `func(panel: string) -> widget-tree` | ui.panel |
| `ui-handler.panel-event` | `func(panel: string, event: widget-event) -> option<widget-tree>`; a returned tree replaces the panel | ui.panel |
| `ui-handler.status` | `func(item: string) -> option<string>` | none |

## `ai-tools`

Plugin export. AI tools declared in the manifest.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `ai-tools.invoke` | `func(tool: string, arguments-json: string) -> result<string, plugin-error>`; JSON in, JSON out | ai.tool |

## `exporter`

Plugin export. Export formats. The host writes the bytes; the plugin never writes files ([Exporters](exporters.md)).

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `exporter.writer` | `resource`: an open export | - |
| `exporter.writer.push` | `func(rows: list<list<value>>) -> result<list<u8>, plugin-error>`; encodes a batch | none |
| `exporter.writer.finish` | `func() -> result<list<u8>, plugin-error>`; returns the trailing bytes | none |
| `exporter.start` | `func(format: string, columns: list<column>) -> result<writer, plugin-error>` | none |

## `completion`

Plugin export. SQL completion items ([Completion providers](completion.md)).

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `completion.completion-kind` | `enum`: `keyword`, `table`, `column`, `function`, `snippet`, `other` | - |
| `completion.completion-request` | `record`: `connection: option<string>`, `driver: option<string>`, `text: string`, `cursor: u32` (a UTF-8 byte offset into `text`) | - |
| `completion.completion-item` | `record`: `label: string`, `insert-text: string`, `kind: completion-kind`, `detail: option<string>` | - |
| `completion.complete` | `func(request: completion-request) -> list<completion-item>` | none |

## `driver`

Plugin export. A database driver implemented as a guest ([Drivers](drivers.md)).

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `driver.connect-params` | `record`: `host: string`, `database: string`, `user: string`, `password: option<string>`, `port: option<u16>`, `options: list<tuple<string, string>>` | - |
| `driver.execution` | `variant`: `rows(row-stream)`, `affected(u64)`; a statement outcome | - |
| `driver.row-stream` | `resource`: a stream of rows | - |
| `driver.row-stream.columns` | `func() -> list<column>`; the result columns | driver.guest |
| `driver.row-stream.next-batch` | `func(max-rows: u32) -> result<option<list<list<value>>>, plugin-error>`; `none` at the end | driver.guest |
| `driver.session` | `resource`: an open connection | - |
| `driver.session.execute` | `func(sql: string, params: list<value>) -> result<execution, plugin-error>` | driver.guest |
| `driver.session.close` | `func()`; closes the connection | driver.guest |
| `driver.connect` | `func(driver: string, params: connect-params) -> result<session, plugin-error>` | driver.guest |

## `driver-catalog`

Plugin export, added in 0.1.1. Optional introspection of a guest driver; it has the same gate as `driver`.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `driver-catalog.table-column` | `record`: `name: string`, `data-type: data-type`, `type-name: string`, `nullable: bool`, `default: option<string>` | - |
| `driver-catalog.supported-kinds` | `func(s: borrow<session>) -> list<object-kind>`; the object kinds the driver can list | driver.guest |
| `driver-catalog.list` | `func(s: borrow<session>, parent: object-path, kind: object-kind) -> result<list<db-object>, plugin-error>`; the children of `parent` of one kind | driver.guest |
| `driver-catalog.columns` | `func(s: borrow<session>, table: db-object) -> result<option<list<table-column>>, plugin-error>`; `none` when the driver has no column data | driver.guest |
| `driver-catalog.definition` | `func(s: borrow<session>, object: db-object) -> result<option<string>, plugin-error>`; the DDL of an object, `none` when there is none | driver.guest |

## `types`

Shared data types, used by the other interfaces.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `types.value` | `variant`: `null`, `boolean(bool)`, `integer(s64)`, `float(f64)`, `text(string)`, `bytes(list<u8>)`, `decimal(string)`, `date(string)`, `time(string)`, `timestamp(string)`, `timestamp-tz(string)`, `json(string)`; temporal values are ISO-8601 text | - |
| `types.data-type` | `enum`: `boolean`, `integer`, `float`, `decimal`, `text`, `binary`, `date`, `time`, `timestamp`, `timestamp-tz`, `json`, `other`; the portable column type | - |
| `types.object-kind` | `enum`: `database`, `schema`, `table`, `view`, `materialized-view`, `function`, `procedure`, `trigger`, `sequence`, `type`, `index`, `extension`, `role` | - |
| `types.environment` | `enum`: `dev`, `staging`, `prod`; the environment tag of a connection | - |
| `types.column` | `record`: `name: string`, `data-type: data-type`, `type-name: string`; a result column | - |
| `types.object-path` | `record`: `database: string`, `schema: string`, `table: option<string>` | - |
| `types.db-object` | `record`: `kind: object-kind`, `parent: object-path`, `name: string` | - |
| `types.column-info` | `record`: `name: string`, `type-name: string`, `nullable: bool`; a column of a table | - |
| `types.connection-info` | `record`: `id: string`, `name: string`, `driver: string`, `environment: environment`; never carries a secret | - |
| `types.error-code` | `enum`: `capability-denied`, `declined`, `invalid-argument`, `not-found`, `limit-exceeded`, `timeout`, `failed` | - |
| `types.plugin-error` | `record`: `code: error-code`, `message: string`; the error of every fallible call | - |

## `widgets`

The declarative widget tree, flattened: node 0 is the root and children are indices. Limits are in
[Capabilities](capabilities.md) and the `ui.panel` pages.

| Item | Signature | Capability |
| ---- | --------- | ---------- |
| `widgets.direction` | `enum`: `vertical`, `horizontal` | - |
| `widgets.gap` | `enum`: `none`, `small`, `medium`, `large` | - |
| `widgets.text-style` | `enum`: `body`, `muted`, `heading`, `code` | - |
| `widgets.button-style` | `enum`: `primary`, `secondary` | - |
| `widgets.stack` | `record`: `direction: direction`, `gap: gap`; a container | - |
| `widgets.text` | `record`: `content: string`, `style: text-style`; a text label | - |
| `widgets.button` | `record`: `id: string`, `label: string`, `style: button-style` | - |
| `widgets.text-input` | `record`: `id: string`, `value: string`, `placeholder: option<string>`; a single-line input | - |
| `widgets.checkbox` | `record`: `id: string`, `label: string`, `checked: bool` | - |
| `widgets.select` | `record`: `id: string`, `options: list<string>`, `selected: option<u32>`; a drop-down | - |
| `widgets.table` | `record`: `columns: list<string>`, `rows: list<list<string>>`; read-only | - |
| `widgets.widget` | `variant`: `stack(stack)`, `text(text)`, `button(button)`, `text-input(text-input)`, `checkbox(checkbox)`, `select(select)`, `table(table)`, `divider` | - |
| `widgets.widget-node` | `record`: `widget: widget`, `children: list<u32>`; only a stack has children | - |
| `widgets.widget-tree` | `record`: `nodes: list<widget-node>`; the root is node 0 | - |
| `widgets.field-change` | `record`: `id: string`, `value: string`; a text input changed | - |
| `widgets.toggle` | `record`: `id: string`, `checked: bool`; a checkbox toggled | - |
| `widgets.selection` | `record`: `id: string`, `index: u32`; a drop-down selection | - |
| `widgets.widget-event` | `variant`: `clicked(string)`, `changed(field-change)`, `toggled(toggle)`, `selected(selection)`; a user interaction with a panel | - |
| `widgets.editor-context` | `record`: `connection: option<string>`, `selection: string` | - |
| `widgets.object-context` | `record`: `connection: string`, `object: db-object` | - |
| `widgets.results-context` | `record`: `connection: string`, `columns: list<column>` | - |
| `widgets.command-context` | `variant`: `none`, `editor(editor-context)`, `object(object-context)`, `results(results-context)`; where a command was invoked | - |
