# Plugin host

`sqler-plugin-host` (L3) loads plugins, checks them against their manifest, and runs the WebAssembly components of
code plugins in a sandbox. The decisions are in [ADR-0057](../adr/0057-plugin-host-runtime.md); the ABI and the
capability model are [ADR-0055](../adr/0055-plugin-abi-wit.md) and [ADR-0056](../adr/0056-plugin-sandbox-and-capabilities.md).

## Layers

```text
default features   engine-free core      discover, digest and grant key, fs scopes, traits, errors
cl | pulley-rt     runtime layer         wasmtime engine, one Store per plugin, WASI, wasi-http, log, settings
net                NetEgress             reqwest + rustls (ring), vetted resolver
```

The shipped `sqler-app` enables none of them. Declarative plugins (host-provided drivers, P9) need only the core.

## Module map

| Module | Role |
| ------ | ---- |
| `dirs`, `discovery` | `PluginDirs`, `discover`: dev paths first, then the config directory; manifest, API check, canonical entry, digest |
| `scope` | `resolve_preopens`: `fs.read` scopes to canonical host directories, symlink escapes refused |
| `grant`, `settings`, `log`, `egress`, `status`, `error` | the traits and types the runtime consumes: `GrantSource`, `SettingsStore`, `Egress`, `PluginStatus`, error taxonomy |
| `runtime::engine` | the `Engine` (created on first compile), the digest-keyed component cache, the epoch thread |
| `runtime::host`, `call`, `instance` | `PluginHost`, `PendingCall`, instantiation, deadlines, fault accounting |
| `runtime::limiter` | memory and table caps with the "limit hit" flag |
| `runtime::bindings`, `link` | one `bindgen!` over `crates/sqler-plugin-api/wit`; `add_imports` links WASI, wasi-http and each host interface |
| `runtime::wasi`, `http` | the WASI context (preopens, stdio to the log) and the wasi-http send hook |
| `runtime::log_host`, `settings_host` | the `log` and `settings` interfaces |
| `runtime::unbound::{catalog,query,ui}` | gate-checked stubs until P5 and P8 replace them |
| `net::{addr,host,resolver,egress}` | `is_forbidden`, `check_host`, `VettedResolver`, `NetEgress` |

## Life of a call

1. The caller asks the host (`init`, and later the typed calls of P8 to P12). Nothing was compiled before this point.
2. A task on the injected tokio runtime takes the plugin's lock. If there is no instance, it reads the effective
   capabilities (`declared ∩ GrantSource::approved(key)`), resolves the preopens, compiles the component (cached by
   digest, re-hashed on read), links, instantiates and runs `lifecycle.init`.
3. The call runs against `select!` of the guest future, the deadline of its `CallKind` and the cancel token. The epoch
   thread makes a spinning or blocked guest yield every tick.
4. A trap, panic, memory cap, deadline or host panic drops the instance; the third consecutive fault disables the plugin.

## Guest paths of preopens

A granted `fs.read` scope appears at `/` + the scope text, read-only:

| scope | guest path | host directory |
| ----- | ---------- | -------------- |
| `fs.read:plugin` | `/plugin` | the plugin's install directory |
| `fs.read:data` | `/data` | `<config>/plugins/<id>/data` (`<plugin dir>/data` without a config directory) |
| `fs.read:data/cache` | `/data/cache` | that subdirectory |
| `fs.read:home/projects` | `/home/projects` | `<home>/projects` |

A scope that is missing, escapes its root through a symlink or has no home directory gets no preopen, a warning in the log, and an entry in `PluginStatus::scope_issues` with the reason. The `plugin` and `data` roots must themselves be real directories directly inside their parent (the plugin's own directory, the host-owned data directory of that plugin id); a root that is a symlink or a junction is refused, so a package cannot ship `data -> ~/.ssh`. Only `home` may be a link.

Grants are enforced per call, not by the linker: one shared `Linker` links every interface for every plugin, so an ungranted import is present and answers `capability-denied` (or has an empty preopen set, or a denying `Egress`). Later items must not assume an import is absent when ungranted. A grant change takes effect through `refresh_grants`, which cancels the calls in flight and drops the instance.

## How later items plug in

Each of P5, P8 to P12 owns one module `src/<name>/**`, adds one `mod` line to `lib.rs` and one line to
`runtime/link.rs::add_imports`, deletes its `runtime/unbound/` stub if it has one, and resolves its export from the
generated `GuestIndices` of `Live::pre`. Host traits go into `Services`.

## Testing

- Engine-free and unit tests run everywhere with default features.
- `--features cl` runs the runtime tests with hand-written WAT components (`tests/runtime_calls.rs`).
- `--features net` runs `NetEgress` against an in-process server on `127.0.0.1` (`tests/net_egress.rs`).
- `tests/guests.rs` runs the built `smoke` and `probe` guests when `SQLER_PLUGIN_FIXTURES=target/plugins/debug` is set
  (after `cargo run -p xtask -- plugins build`); `SQLER_TEST_REQUIRE_PLUGINS=1` makes a missing fixture a failure.
