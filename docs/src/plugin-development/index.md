# Plugin development

Plugins are WebAssembly components with WIT interfaces. The decision is recorded in
[ADR-0002](../adr/0002-plugin-abi-wasm-component-model.md). The ABI is the WIT package
`sqler:plugin@0.1.0` ([ADR-0055](../adr/0055-plugin-abi-wit.md)); the sandbox and the
capability model are in [ADR-0056](../adr/0056-plugin-sandbox-and-capabilities.md).

A plugin is a directory with a `sqler.toml` manifest and, unless it is declarative only, one
`.wasm` component. The manifest declares what the plugin contributes (commands, panels,
status items, menus, exporters, completion, AI tools, drivers, settings) and which
capabilities it asks for. The user approves the capabilities; the default is deny.

## Pages

- [Tutorial](tutorial.md): generate a plugin from the template, build it, install it and run its command.
- [Manifest](manifest.md): every key of `sqler.toml`, an example and the error list.
- [Capabilities](capabilities.md): the grammar, what each capability gates, grant keys,
  re-prompts and limits.
- [Drivers](drivers.md): guest and host drivers, what a driver receives, streaming, cancel and the error texts.
- [Exporters](exporters.md): file formats for the export dialog and `sqler-cli export`, ids, labels, limits and error texts.
- [Completion providers](completion.md): rows from a plugin in the SQL editor popup.
- [WIT reference](wit-reference.md): every interface, function and type of the WIT package.
- [Publishing](publishing.md): local install, development folders and approvals; a registry is planned after 1.0.

## Choosing a world

A plugin picks the feature worlds it needs (they use `include`), so a panel plugin does not
stub exporter functions. Worlds: `base` (lifecycle only), `ui-plugin`, `ai-tool-plugin`,
`exporter-plugin`, `completion-plugin`, `driver-plugin`, `driver-catalog-plugin` (a driver
that also lets the app browse its schema, package 0.1.1) and `full`. Every world imports the
five host interfaces `log`, `settings`, `catalog`, `query` and `ui`. The host always links
them; an ungranted call returns `error-code::capability-denied` at runtime, so a denied
plugin still loads.

A manifest without `[entry]` is declarative only. Drivers of kind `host` and settings need no
component and load without the WebAssembly runtime.
