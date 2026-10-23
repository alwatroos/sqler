# Publishing

For 1.0 a plugin is installed locally. There is no registry, no download and no update channel.

## Install

A plugin is a folder with `sqler.toml` and, unless it is declarative only, the `.wasm` component named in `[entry]`.
Copy the folder to `plugins/<id>` in the [configuration directory](../user-guide/settings.md). The folder name is the
plugin id. [Tutorial](tutorial.md) step 3 has the commands for each OS.

To share a plugin, hand over that folder (for example as an archive). The person who receives it copies it the same way.

## Development folders

`--plugin-dir <PATH>` (repeatable) adds a folder that holds `sqler.toml`, or whose children are plugins. SQLer looks
nowhere else for development plugins. A development plugin with the id of an installed one replaces it
([Plugins](../user-guide/plugins.md)).

## Approvals and updates

The approval of a plugin is tied to its id, version, manifest hash and component hash. When you install a new build with
any of these changed, SQLer asks again before the plugin runs ([Capabilities](capabilities.md)). Raise `version` in
`sqler.toml` for every release you hand out.

## Not in 1.0

A plugin registry, search and automatic updates are planned after 1.0, with an intake process for plugins. Nothing else is
promised here.
