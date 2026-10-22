# sqler-plugin-api

The plugin contract of SQLer: the WIT package `sqler:plugin`, the `sqler.toml` manifest validator, the capability model, the manifest hash and grant key, widget-tree and contribution types, and the sandbox limits.

Status: API 0.1.0 (M6). While the major version is 0, an additive change bumps the patch and a breaking one bumps the minor (ADR-0055).

Layer: L1

## wasmtime-free

This crate has no `wasmtime` and no `wit-bindgen` dependency. It is plain data and parsing code. The host bindings (`wasmtime::component::bindgen!` on the `wit/` directory) live in `sqler-plugin-host`; plugin guests run `wit_bindgen::generate!` on the same directory. `sqler-plugin-host` also owns loading, sandboxing and the grant store.

## Module map

| Path | Content |
| ---- | ------- |
| `wit/*.wit` | Package `sqler:plugin@0.1.0`: `world.wit` (worlds) and one file per interface |
| `src/api.rs` | `API_VERSION`, `ApiVersion`, `host_supports`, `CompatError` |
| `src/id.rs` | Id newtypes: `PluginId`, `EngineId`, `ContributionId`, `ToolName`, `SettingKey` |
| `src/capability.rs` | `Capability`, `CapabilityKind`, `CapabilitySet`, `Gate`, `HostInterface` |
| `src/host_pattern.rs`, `src/fs_scope.rs` | `HostPattern` and `FsScope` grammar and matching |
| `src/manifest/` | `Manifest::parse`, `ManifestError`, contributions |
| `src/hash.rs`, `src/grant.rs` | `ManifestHash`, `ComponentDigest`, `GrantKey` |
| `src/widget.rs` | `WidgetTree` and its validation |
| `src/limits.rs` | `Limits::DEFAULT` |

## Documentation

Author documentation is in the book: `docs/src/plugin-development/` (manifest and capabilities pages). Decisions: ADR-0055 (ABI) and ADR-0056 (sandbox and capabilities).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
