# ADR-0002: Plugin ABI: WASM Component Model

- Status: accepted
- Date: 2026-10-20
- Deciders: SQLer maintainers

## Context and Problem Statement

SQLer is extensible through plugins, including drivers for non-core databases. Plugins must be
sandboxed, language-agnostic where possible, and have a stable interface.

## Decision Drivers

- Sandboxing with no ambient authority.
- A stable, versioned interface definition.
- No `unsafe` in the host for plugin loading beyond what the runtime provides.
- Capabilities approved by the user.

## Considered Options

- WebAssembly components on wasmtime, with WIT interfaces.
- Native dynamic libraries.
- Core WebAssembly without the Component Model.
- Embedded scripting such as Lua or JS.

## Decision Outcome

Plugins are WebAssembly components on wasmtime (49.0.4 in the 2026-10-20 survey), with WIT
interfaces, targeting WASI 0.2 and tracking 0.3.

- Manifest `sqler.toml`: id, version, capabilities, entry points.
- Capabilities: `db.read`, `db.write`, `ui.panel`, `net.http:<host>`, `fs.read:<scope>`, `ai.tool`.
- Host WIT interfaces in `crates/sqler-plugin-api/wit/`: `catalog`, `query` (guarded), `ui` (a
  declarative widget tree), `ai-tools`, `export`, `completion`, `settings`, `log`.
- Sandbox: no ambient authority; memory and CPU limits; capabilities are approved at install and
  shown in the plugin list.

### Consequences

- Plugins can be written in any language that targets the Component Model.
- The host owns the guard on `query`; plugins cannot bypass it.
- The WIT files become public API and are versioned.

### Confirmation

The plugin host (M6) is tested with example plugins that request, and are denied, capabilities they
did not declare.

## Pros and Cons of the Options

### WebAssembly components on wasmtime

- Good: strong sandbox, typed interfaces via WIT, capability model fits WASI.
- Bad: the Component Model tooling is still evolving.

### Native dynamic libraries

- Bad: unstable Rust ABI, requires `unsafe`, no sandbox.

### Core WebAssembly without the Component Model

- Good: simple runtime.
- Bad: hand-rolled ABI for strings and structures.

### Embedded scripting such as Lua or JS

- Bad: not Rust everywhere; weaker sandbox.

## More Information

- `cargo-component` 0.21.1 has had no release since 2025-03 (2026-10-20 survey in
  [Dependencies](../dependencies.md)). Re-evaluate it against `wit-bindgen` plus the `wasm32-wasip2`
  target in M6.
