# ADR-0055: Plugin ABI: the `sqler:plugin` WIT package

- Status: accepted
- Date: 2026-10-22
- Deciders: SQLer maintainers

## Context and Problem Statement

[ADR-0002](0002-plugin-abi-wasm-component-model.md) chose WebAssembly components with WIT interfaces and said the WIT
files become public, versioned API. [ADR-0054](0054-plugin-runtime-and-driver-feasibility.md) fixed the runtime
(wasmtime, epoch interruption, guests built with `wit-bindgen` for `wasm32-wasip2`). Still open: the exact package,
how its version evolves, which worlds and interfaces a plugin may use, which WASI interfaces the host links, and how
a host-engine driver (the ADR-0054 fallback) differs from a guest driver.

## Decision Drivers

- The ABI is public: a plugin built today must keep loading, or be refused with a clear message.
- A plugin that only draws a panel must not stub exporter or driver functions.
- A denied plugin still loads (ADR-0002 Confirmation): the host always links every import and denies at call time.
- Later items (P8 widgets, P9 drivers, P10 export, P11 completion, P12 AI tools) add interfaces without breaking P2.
- The crate that owns the WIT stays free of wasmtime; the host and the guests generate bindings from the same files.

## Considered Options

- One WIT package `sqler:plugin` with one file per interface and feature worlds that `include` a base world.
- One monolithic world every plugin must fully implement.
- Referencing `wasi:*` worlds from our WIT (it would vendor `wit/deps/`).

## Decision Outcome

One package, `sqler:plugin@0.1.0`, in `crates/sqler-plugin-api/wit/`. The package line appears only in `world.wit`;
every other file holds one interface. The Rust constants `WIT_PACKAGE` (`"sqler:plugin"`) and `API_VERSION` (0.1.0)
equal that line; a drift test checks it. The host runs `wasmtime::component::bindgen!` on the directory; guests run
`wit_bindgen::generate!` on the same directory.

### Version rule

- While the major is 0: an additive change bumps the patch, a breaking change bumps the minor.
- From 1.0 on: additive is a minor bump, breaking is a major bump.
- Additive means a new interface in a new `.wit` file, plus `import`/`export` lines in worlds and the package line
  bump. An existing interface or type is never edited (P8 and P9 follow this).
- A manifest names the version it was built against (`[plugin] api`, `M.m` or `M.m.p`). The host supports it when
  `ApiVersion::host_supports` holds: for major 0, the same minor and a host patch >= the required patch; otherwise the
  same major and (minor, patch) <= the host's.
- The refusal path: the host checks the manifest before instantiation and refuses with `CompatError::Incompatible
  { required, host }`. Link failure of a missing import is the second line of defence (P4 reports it as the same kind
  of error).

### Worlds

| world | content |
| ----- | ------- |
| `imports` | the five imports: `log`, `settings`, `catalog`, `query`, `ui` |
| `base` | `include imports`; export `lifecycle` |
| `ui-plugin` | `base` + export `ui-handler` |
| `ai-tool-plugin` | `base` + export `ai-tools` |
| `exporter-plugin` | `base` + export `exporter` |
| `completion-plugin` | `base` + export `completion` |
| `driver-plugin` | `base` + export `driver` |
| `full` | `base` + all exports (reference only; the host binds per feature world and looks up each export separately, because instantiating through a world fails when the component lacks one of its exports) |

### Interfaces

| interface | direction | content |
| --------- | --------- | ------- |
| `types` | shared | `value`, `data-type`, `object-kind`, `environment`, `column`, `object-path`, `db-object`, `column-info`, `connection-info`, `error-code`, `plugin-error` |
| `log` | import | `write(level, message)` |
| `settings` | import | `get(key) -> option<setting-value>` |
| `catalog` | import | `connections`, `children`, `columns` |
| `query` | import | `run(query-request) -> result<query-result, plugin-error>` |
| `ui` | import | `refresh-panel`, `set-status`, `notify` |
| `widgets` | shared | the flattened widget tree (node 0 is the root, children are indices), `widget-event`, `command-context` |
| `lifecycle` | export | `init` |
| `ui-handler` | export | `run-command`, `render-panel`, `panel-event`, `status` |
| `ai-tools` | export | `invoke(tool, arguments-json)` |
| `exporter` | export | `start(format, columns) -> writer`; the host writes the bytes, the plugin never writes files |
| `completion` | export | `complete(request) -> list<completion-item>`; the cursor is a UTF-8 byte offset |
| `driver` | export | `connect`, resources `session` and `row-stream` |

`widget` carries only semantic styles and gap tokens: no colours, sizes or pixels. Temporal values travel as ISO-8601
text. Every fallible call returns `plugin-error { code, message }`; `error-code::capability-denied` is the runtime
denial of [ADR-0056](0056-plugin-sandbox-and-capabilities.md).

### WASI

The WASI 0.2 interfaces the host may link:

| WASI | host behaviour |
| ---- | -------------- |
| io, clocks, random | linked |
| cli | empty environment and arguments; stdout and stderr go to `log`; no stdin |
| filesystem | read-only preopens, from `fs.read` scopes only |
| http outgoing-handler | only with `net.http` hosts |
| sockets | linked with everything denied |

Tracking 0.3: a WASI 0.3 world is an additive later version of the package. Our WIT does not reference `wasi:*`
worlds (no vendored `wit/deps/`); the guest toolchain adds the WASI imports itself.

### Host-engine and guest drivers

Per the ADR-0054 fallback, a driver whose engine is native (DuckDB, SQL Server) is a declarative manifest entry
(`kind = "host"`, `engine = "<id>"`) plus the capability `driver.host:<engine-id>`; the host runs the engine through the
driver seam (P9). Such a plugin may have no `[entry]` and no wasmtime. A guest driver (`kind = "guest"`) exports the WIT
`driver` interface and needs `driver.guest`. P9 may add a host-driver interface as an additive file.

### Consequences

- A panel plugin implements only `lifecycle` and `ui-handler`.
- A change to an existing interface is a breaking change: a minor bump while 0.x, and an ADR.
- A denied plugin loads; it gets `capability-denied` at call time.
- `wit-parser` (dev only) checks the package parses in tests. The crate has no wasmtime in `cargo tree`.

### Confirmation

`crates/sqler-plugin-api/tests/wit_parse.rs` parses the package and checks the version, the eight worlds and the 13
interfaces by name. `tests/wit_drift.rs` (W2) checks the version against `API_VERSION` and the enum cases against
`sqler-core`.

## Pros and Cons of the Options

### One package with feature worlds (chosen)

- Good: small plugins, additive growth, one source for host and guests.
- Bad: the host must link every import to keep denied plugins loadable.

### One monolithic world

- Bad: every plugin stubs functions it does not use; every addition breaks every plugin.

### Referencing `wasi:*` worlds

- Bad: vendors `wit/deps/`, pins WASI versions inside our package, no gain for guests.

## More Information

- [ADR-0002](0002-plugin-abi-wasm-component-model.md), [ADR-0054](0054-plugin-runtime-and-driver-feasibility.md),
  [ADR-0056](0056-plugin-sandbox-and-capabilities.md)
- WIT files: `crates/sqler-plugin-api/wit/`
