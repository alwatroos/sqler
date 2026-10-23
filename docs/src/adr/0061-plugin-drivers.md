# ADR-0061: Plugin drivers

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

[ADR-0054](0054-plugin-runtime-and-driver-feasibility.md) decided that drivers can come from plugins and that DuckDB
and SQL Server use a host-provided engine behind a plugin shell. [ADR-0055](0055-plugin-abi-wit.md) defines the WIT
`driver` interface and [ADR-0056](0056-plugin-sandbox-and-capabilities.md) the capabilities `driver.guest` and
`driver.host:<engine>`. Still open: how a plugin driver enters the `DriverRegistry`, who owns connect, TLS and sockets,
what a guest driver sees, how cancel works, what the user sees when a driver is unavailable, and how the existing guards
(run flow, production gate) keep applying.

## Decision Drivers

- Built-in drivers and every registry caller stay unchanged.
- Drivers come and go at run time (enable, disable, revoke, reload) without a mutable registry behind the app's `Arc`.
- A plugin driver gets no new write path: the run flow and the prod gate of [ADR-0024](0024-query-execution.md) apply.
- A guest driver is untrusted code that holds database credentials; it must see as little as possible.
- The Linux binary is close to its size budget; engines must not be linked unless asked for.

## Considered Options

- A run-time overlay: a `DriverSource` trait asked after the built-ins, with `Driver::id` returning `&str`.
- Keep `&'static str` ids and `Box::leak` the plugin ids. Rejected: it leaks on every enable/disable cycle.
- A mutable registry with `register`/`unregister`. Rejected: every holder of the registry `Arc` would need a lock and
  every lifecycle path a hook.
- A guest `cancel` export. Rejected: calls to one plugin are serialised, so it could never run while `next-batch` is busy.

## Decision Outcome

### Registration

`sqler-driver-api` gets `DriverSource` (`ids`, `lookup`), `SourceLookup` (`Unknown`, `Available`, `Unavailable(text)`),
`DriverRegistry::add_source` and `available_ids`. `Driver::id` becomes `fn id(&self) -> &str`; the built-in impls keep
returning `&'static str`. `DriverRegistry::connect` tries the built-ins, then the sources in insertion order, then
fails with `UnknownDriver`; `Unavailable(text)` becomes `DriverError::Unavailable(text)`. `ids()`, `get()` and `Debug`
stay built-in only; `available_ids()` is built-ins plus sources, sorted and deduplicated, and a built-in wins. Only the
binaries add sources (layering): the app and the CLI do it in `registry()` behind the `plugins` feature, which is off
by default. `DriverSource::ids` may block (it reads grants) and is never called on the UI thread; the app refreshes the
driver list off-thread together with the plugin snapshot.

The registry id is the `[[drivers]] id` of the manifest, not namespaced, so a driver that later becomes a built-in keeps
its id in `connections.toml`. If two plugins claim one id, the first in load order wins and the later one is
unavailable. A built-in always wins.

`sqler-plugin-host` provides `PluginDrivers`, the `DriverSource`. It decides availability lazily from host state on every
lookup, so enable, disable, revoke and reload need no lifecycle hooks and a revoke takes effect at the next lookup. Before
`attach(&PluginHost)` it lists nothing and answers `Unknown`.

### Two kinds of driver

| | `kind = "host"` | `kind = "guest"` |
| - | - | - |
| Capability | `driver.host:<engine>` | `driver.guest` |
| Code that talks to the database | a native `Driver` linked into the binary (`HostEngines`) | the plugin's own WebAssembly component |
| Connect, TLS, sockets | the host | the guest (it has no sockets; only HTTP egress under `net.http`) |
| Needs `[entry]` | no | yes |
| What the guest sees | nothing; the guest, if any, gets no driver call | the connect parameters only |

A host engine is an `Arc<dyn Driver>` registered in `HostEngines` by the binary, from the existing engine crates behind
default-off features (DuckDB: P15, SQL Server: P16). The plugin is a declarative shell that names the engine. The host
adapter re-checks availability at connect and at every `execute`, `list`, `table` and `definition` of the session; a
statement already streaming completes. A native engine must not assume that `config.driver` equals its own id.

A guest driver is the WIT `driver` export, plus the optional `driver-catalog` export for introspection.
`GuestDriver` maps them onto `Session`, `QueryStream` and `Introspect` through `PluginHost::run(.., CallKind::DriverCall, ..)`, so
the sandbox, the deadline (300 s per call), the memory cap (256 MiB) and the fault counting of
[ADR-0057](0057-plugin-host-runtime.md) are inherited. Every call first checks that `driver.guest` is still effective and
that the session's instance generation is still live; otherwise it fails with "the plugin driver session ended (the plugin
was restarted, disabled or its permissions changed)".

### WIT mapping and the 0.1.1 rule

`driver-catalog` is a new interface (`supported-kinds`, `list`, `columns`, `definition`) in a new world
`driver-catalog-plugin` and in `full`. It is additive, so the WIT package goes from `0.1.0` to `0.1.1` under the rule of
ADR-0055: new interfaces and functions are patch bumps, and a guest built against `0.1.0` still loads because wasmtime
matches imports and exports by semver-compatible version. The host gate puts `driver-catalog` under the existing `driver`
gate; no new capability exists. A guest without `driver-catalog` still works, with no introspection.

- `execute` returns either `rows(stream)` (the host reads `columns` in the same call and yields `Columns`) or
  `affected(n)` (`Columns([])`, then `Done` with `n`).
- The stream asks `next-batch(500)`. The host validates every batch: at most 500 rows, row width equal to the column
  count, summed cell cost within the result byte cap. A violation is a protocol error ("the plugin driver sent malformed
  rows") and poisons the stream. `none` is `Done` and the resource is dropped; an empty batch is only an empty `Rows`
  event, so a guest that never answers `none` is stopped by a cancel or the call deadline.
- `close` calls the guest `close`, then drops the resource. If the instance is gone, it succeeds.

### Cancel

Cancel cancels the in-flight host call (`PendingCall`); the host drops the instance, as for any cancelled call. A cancel
between batches only ends the stream and the session stays usable. A cancel in the middle of a call kills the instance, so
the next `execute` on that session fails with the session-ended text. The same holds for a trap or a timeout: it ends
every open session of that plugin, not only one. A stream yields `Err(Cancelled)` and then ends.

### What a guest driver sees

Only `connect(driver, connect-params)`, and only while `driver.guest` is effective. The parameters are built from the
`ConnectionConfig` the user created with that driver id, after the connect pipeline injected the password: host, port,
user, password, database and the query pairs of the target as `options`. A target that is not a URL becomes `database`.
Parsing is by hand with percent-decoding. The guest never receives the SSH tunnel secret, TLS file paths, any other
connection, the environment, or the config of a host engine. `ConnectParams` redacts the password in `Debug`.

**Wording decision for `driver.guest`.** A guest driver receives the user name and the password of every connection that
uses its driver. The grant dialog must say so: the line for `driver.guest` states that the plugin receives the host,
username and password of every connection created with its driver, not only that it "provides a driver that runs inside
the plugin" (the P6 text). `driver.guest` stays in the group of permissions that need attention. The line now reads "Provide a database driver that
runs inside the plugin. The plugin receives the host, username and password of every connection that uses its driver".
The text lives in `sqler-ui` `plugins/grant/prompt.rs`, with a test that pins the words; no screenshot scene shows a
`driver.guest` line, so none changed. The user guide shows the same text.

### Unavailable states and error texts

A driver is unavailable, with these fixed texts (ids only, in `driver/error.rs`), checked in this order:

1. "driver `{d}` comes from plugin `{p}`, which is disabled"
2. "driver `{d}` needs the permission `{cap}` of plugin `{p}`, which is not approved"
3. "driver `{d}` needs the `{e}` engine, which this build does not include"
4. "driver `{d}` is already provided by plugin `{q}`"

The connect failure shows the text as `driver unavailable: <text>`. During a session, `CallError` maps to fixed texts and
wasmtime text never passes through: trap "the plugin driver crashed", timeout "the plugin driver did not answer in time",
memory "the plugin driver ran out of memory", cancel `Cancelled`, and a disabled, missing, unloadable or panicked plugin
"the plugin driver is not available". A guest `plugin-error` message goes through `query_text`, the scrub of
[ADR-0058](0058-plugin-query-and-catalog.md) (a message that mentions a password or a denial marker becomes the fixed
DENIED text), as a `Connection` error at connect and a `Query` error otherwise.

### Guard parity

A plugin driver has no write path of its own. It is reachable only as a registry `Session`, so the editor run flow,
the statement classifier and the production gate ([ADR-0024](0024-query-execution.md)) apply as for a built-in driver. The
code under `driver/**` names neither `QueryApprover` nor `run_guarded`; a source scan test enforces it.

### Binary size and the fallback

P9 adds no engine and no third-party crate. Engines are linked only by default-off Cargo features of the binaries
(`driver-duckdb`, `driver-mssql`), one `cfg` line each in `engines()`; engine crates never depend on `sqler-plugin-host`.
Host-engine drivers need the `plugins` feature too, because `PluginHost` is runtime-only; an engine-free path would
duplicate the registry, grant and disable state of the host.

### Licence note for P15

The native DuckDB driver (`sqler-driver-duckdb`) pulls `libduckdb-sys`, which brings `webpki-roots` under
CDLA-Permissive-2.0. That licence is not in the allow list of `deny.toml`. P15 must stop and ask the owner before adding
the dependency. No exception is added by this ADR or anywhere else.

## Consequences

- Positive: built-in drivers and registry callers are untouched; plugin drivers come and go without hooks; one guard
  path for all drivers; the guest sees the minimum.
- Negative: a guest driver does get the password of its connections, which is why the dialog says so. A cancel or a
  trap ends all sessions of that plugin. Revoke or disable stops a host-engine session at the next statement, not
  mid-stream. `--open-sessions` before the first refresh sees plugin ids as unknown (the existing failure path). The
  connect pipeline puts a keychain password into the target only for the URL schemes it knows (`postgres`, `mysql` and
  their aliases) and opens SSH tunnels only for them, so a plugin driver with another scheme gets the password only when
  the user wrote it into the target; widening the pipeline is a separate decision.
- Neutral: the CLI honours stored grants and the plugin preferences, never prompts, and discovers `<config>/plugins` only.
