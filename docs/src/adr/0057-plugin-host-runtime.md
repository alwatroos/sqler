# ADR-0057: Plugin host runtime

- Status: accepted
- Superseded in part by [ADR-0069](0069-binary-size-budget-70-mb.md) (2026-10-23): the binary size budget is now 80 MB (83,886,080 bytes, amendment of ADR-0069, decision of 2026-10-23); the 60 MB figures below are the budget at the time of writing.
- Date: 2026-10-22
- Deciders: SQLer maintainers

## Context and Problem Statement

[ADR-0054](0054-plugin-runtime-and-driver-feasibility.md) measured the runtime variants and chose epoch interruption.
[ADR-0055](0055-plugin-abi-wit.md) fixed the ABI and the WASI set, [ADR-0056](0056-plugin-sandbox-and-capabilities.md)
the capabilities and limits. `sqler-plugin-host` (P4) now has to turn them into code: discover plugins, compile and
instantiate them, bound every call, scope WASI and outgoing HTTP by the grant, and keep a misbehaving guest from taking
anything else down. The shipped binary must not change (60 MB budget, ADR-0054), and nothing may compile or
instantiate a plugin on the first-frame path ([ADR-0007](0007-desktop-binary-and-ui-tokio-bridge.md)).

## Decision Drivers

- A trap, a panic, an out-of-memory, a runaway loop or a guest blocked in a host wait never affects the host or another
  plugin.
- Default deny: the effective capabilities are the declared set intersected with the approved set.
- Declarative plugins (P9 host drivers) must load without wasmtime.
- One TLS stack (rustls on ring), no `webpki-roots`, no `rsa`.
- Later items (P5, P8 to P12) add one module and one linker line each, without touching this core.

## Considered Options

- Two layers: an engine-free core (default features) and a `runtime` layer behind `cl` / `pulley-rt`; epoch yields with
  a wall-clock deadline per call; one `Store` per plugin, dropped after any fault.
- Fuel metering instead of epochs.
- The default `wasmtime-wasi-http` sender (own TLS stack, `webpki-roots`).
- An on-disk compilation cache.
- A `Store` per call.

## Decision Outcome

Chosen: the first option. The crate has two layers.

### Layers and features

`default = []` is the engine-free core: `discover` (manifests, API check, canonical entry path, component digest, grant
key), `resolve_preopens`, the `GrantSource`, `SettingsStore`, `Egress` and `EgressFactory` traits, `NoEgress`, the error
types and the log sanitiser. It never links wasmtime, so a manifest-only plugin loads with it alone.

| feature | adds |
| ------- | ---- |
| `runtime` (internal) | wasmtime (`runtime`, `component-model`, `async`, `std`; no default features), wasmtime-wasi `p2`, wasmtime-wasi-http `p2` (default sender off), tokio `rt, time, sync, macros`, tokio-util, http-body-util |
| `cl` | `runtime` + Cranelift: components are compiled in-process |
| `pulley-rt` | `runtime` + the Pulley interpreter. It links, but `compile` returns `LoadError::NoCompiler`: loading precompiled `.cwasm` needs `unsafe` `Component::deserialize` and the engine decision is P7's |
| `net` | `NetEgress` over the workspace reqwest/rustls (ring) client, tokio `net` and `time` |

`sqler-app` depends on none of them. CI asserts that `cargo tree -p sqler-app -e normal` has no `wasmtime` with default
features (`deny` job), so the shipped binary does not grow until a later item turns a feature on.

### Engine

wasmtime **50.0.0**, the newest non-yanked release on 2026-10-22; wasmtime-wasi and wasmtime-wasi-http have the same
version. One `Engine` per `PluginHost`, created on the first compile (`OnceLock`), never in `PluginHost::new`. The
configuration is the ADR-0054 one: component model on, epoch interruption on, fuel off, `wasm_backtrace_details`
disabled, Cranelift at `OptLevel::Speed` under `cl`, and every WebAssembly proposal pinned (next section). No on-disk cache and no parallel compilation: compile happens when
a plugin is first called, on a blocking thread, and the result is cached in memory by `ComponentDigest`. On compile the
file is read again and hashed again; a different digest is `LoadError::ComponentChanged`, so a component swapped after
discovery never runs under the grant of the old one. (`async_support` is a no-op in wasmtime 50 and is not called.)

#### Pinned proposals

`engine::FEATURES` sets every proposal explicitly, so a wasmtime update that changes a default cannot widen the guest
surface. The reason that mattered: wasmtime enables the **threads** proposal by default, and a core module inside a
component could then declare `(memory 1 65536 shared)`. Growth of a shared memory does not go through the
`ResourceLimiter`, so one guest could reach its declared maximum (4 GiB) past `memory_bytes`. The ABI has no use for
threads, so they are off, and `Config::shared_memory(false)` (wasmtime's own default) is set as well. A shared memory and
a `memory64` memory are tested to fail at compile time (`tests/fixtures/wat/shared-memory.wat`, `memory64.wat`); the
shared-memory test fails when the proposal is turned back on.

| proposal | state | why |
| -------- | ----- | --- |
| bulk-memory, multi-value, reference-types, SIMD, mutable-global, sign-extension, non-trapping float-to-int, extended-const | on | standard in Rust's `wasm32-wasip2` output; no effect on memory accounting |
| multi-memory | on | every memory is counted by the limiter, in a total across the store (`memories_count_together_against_the_cap`); the 16-memory limit applies |
| threads, shared-everything-threads | **off** | shared memories bypass the limiter |
| memory64 | **off** | not needed by the ABI (wasm32); larger declarations |
| GC, function-references | off | not used by the toolchains the SDK supports |
| relaxed-SIMD | off | results may differ between hosts |
| tail-call | off | not emitted by default; keeps stack use predictable under the epoch yields |
| exceptions, legacy exceptions, stack-switching, wide-arithmetic, custom-page-sizes | off | not used |
| component model: async, async-stackful, more-async-builtins, threading, error-context, GC, 64-bit, map | off | the p2 ABI of ADR-0055 uses none of them |

wasmtime installs its trap handlers process-wide when the first engine is created. That is lazy here, and in the shipped
default it never happens.

### Lifecycle and states

`PluginHost::load(Discovery)` registers plugins and replaces the previous set; it does no compile. A plugin is `Idle`
(registered), `Live` (instantiated) or `Disabled { reason }` with `Faults { count, last }`, `Load(LoadError)` or `User`.
`init` is the first call: it instantiates if needed, which links with the effective grants and runs `lifecycle.init`
under the init deadline. Every later (re)instantiation runs `init` again. An `init` error counts as a fault. The WIT
`lifecycle` has no shutdown, so unloading drops the store. `refresh_grants` bumps the grant generation, **cancels the
calls in flight** (they end with `Cancelled`, which is not a fault) and drops the instance; the next call relinks with the
grants as they are then. Without the cancel a revoked `net.http` or `fs.read` would keep working until the running call's
deadline (30 s for commands, 300 s for driver calls). The effective set a call sees is fixed when its instance is built;
what changes at `refresh_grants` is only that the old instance stops at once. Calls queued behind the one running are
cancelled with it (they hold a token of the same generation) and the caller retries. A load error (`Link`, `Compile`, `ComponentChanged`, `EntryMissing`, `NoCompiler`) disables
the plugin with `DisableReason::Load`; `enable` retries. A missing export is `CallError::MissingExport` and leaves the
plugin `Idle`.

### Threading, deadlines and cancel

The host takes a tokio `Handle` in `HostOptions` (injected like the app's `sqler-db` runtime) and starts no runtime. A
call is a task on that handle: it locks the plugin's `tokio::sync::Mutex<Option<Live>>` (calls to one plugin are
serialised, different plugins run in parallel), instantiates if needed, sets `epoch_deadline_async_yield_and_update(1)`
and `select!`s the call against `tokio::time::sleep(deadline)` and a cancel token. An epoch thread
(`sqler-plugin-epoch`, started with the engine, joined on `shutdown`) bumps the epoch every `limits.epoch_tick` (10 ms),
so a spinning guest yields to the executor every tick and the timer and the token are polled. A guest blocked in a host
wait (a clock `poll`, an HTTP future) is a pending future, so the same `select!` drops it. `PendingCall<R>` is a oneshot
future usable on any executor, with `cancel()`.

| `CallKind` | deadline (ADR-0056) |
| ---------- | ------------------- |
| `Init` | 5 s |
| `Render`, `Status` | 1 s |
| `Completion` | 250 ms |
| `Command`, `AiTool` | 30 s |
| `ExportBatch` | 5 s |
| `DriverCall` | 300 s |

### Limits and fault policy

An own `ResourceLimiter` per store with running totals: linear memory capped at `limits.memory_bytes` in total across
all of the store's memories (64 MiB; 256 MiB when the manifest has a guest driver), and 1,000,000 table elements in total
across all tables; a request past either sets a flag and fails (a growth is an error, which traps); 64 instances, 64
tables, 16 memories. The WASI resource table (streams, `wasi:http` fields and bodies) is capped at 10,000 entries per
plugin; the memory behind one entry, for example a body being buffered, is host heap outside the memory cap. It is
bounded by the egress budget (next paragraph), the body caps and the call deadline (residual risk).

Egress concurrency is capped in the send hook, before anything is buffered: a plugin may have **4** requests in flight
(`EGRESS_MAX_REQUESTS_PER_PLUGIN`) and hold **32 MiB** of request and response bytes
(`EGRESS_MAX_BYTES_PER_PLUGIN`); across all plugins of one host the numbers are **16** requests and **96 MiB**. A request
over any of them is refused at once with the wasi-http error `connection-limit-reached`, never queued. Request bytes are
charged as they are read and given back when the request ends; response bytes are charged when it arrives and stay
charged until the guest drops the response body, so unread responses cannot pile up either. The budgets belong to the
plugin entry (like the log limit), so a restart does not refill them, and leases are given back when the request future
is dropped (cancel, unload). Residual: the bytes of a request still being sent (up to 8 MiB request plus 16 MiB response
each) are not charged before they exist, so the worst case is 4 x 24 MiB per plugin and 16 x 24 MiB per host, plus one
blocking-pool thread per DNS lookup in flight (at most the request cap).

| outcome | `CallError` | counts as a fault |
| ------- | ----------- | ----------------- |
| trap, guest panic (wasm32 aborts: `unreachable`) | `Trap` | yes |
| guest `wasi:cli/exit` (`I32Exit`) | `Trap` ("guest exited (status N)") | yes |
| limiter flag set | `MemoryLimit` | yes |
| deadline | `Timeout` | yes |
| host task panicked (`JoinError::is_panic`) | `HostPanic` | yes |
| cancel | `Cancelled` | no |
| plugin returned `plugin-error` | `Guest` | only from `init` |
| plugin disabled | `Disabled` | no |

After every fault or cancel the instance is dropped (the store is in an unknown state). A success resets the counter; the
**third consecutive fault disables** the plugin (`DisableReason::Faults`). `enable` lifts it and resets the count. The
fault limit, like the other numbers in this ADR, is a constant in one place and reversible.

### WASI scope

| WASI | state | why |
| ---- | ----- | --- |
| environment, arguments | empty | no ambient data |
| stdin | none | |
| stdout, stderr | line-buffered into the log (info, warn), same rate limit as `log.write` | a panic message reaches the log |
| `sockets` | TCP, UDP and name lookup off; address check denies everything | the only network is wasi-http through `Egress` |
| `filesystem` | read-only preopens, one per **approved** `fs.read` scope, at `/` + scope (`/data`, `/plugin`, `/data/cache`) | `FsPerms::ReadOnly`; wasmtime-wasi (cap-std) refuses `..` and symlinks leaving a preopen |
| `http` | `outgoing-handler` sends through the host's `Egress`; there is no default sender | see below |
| `clocks` (wall, monotonic) | linked | see below |
| `random` | linked | see below |

The honest statement about clocks and random: **they stay linked**. [ADR-0055](0055-plugin-abi-wit.md) lists no capability
that grants them, and the Rust standard library needs them (`HashMap` seeds, `Instant`, `thread::sleep`). A plugin can
therefore read the time and draw random numbers, and it can sleep. Sleeping is harmless because every wait is bounded by
the call deadline (the probe's one-hour `sleep` command ends in `Timeout`). Reading the clock gives a plugin a timing side
channel; the project accepts it. Scopes that cannot be resolved (`Missing`, `Escapes`, `NoHome`) get no preopen, a
warning in the log and an entry in `PluginStatus::scope_issues` (with a `Display` reason the app can show); the plugin
still loads. The `plugin` and `data` roots are owned by the package author and the host respectively, so they are not
followed through links: the root must not itself be a symlink or, on Windows, any reparse point (junction, mount
point; checked with `FILE_ATTRIBUTE_REPARSE_POINT`), and its canonical path must be `<canonical parent>/<name>`. Before
any file system call the root text is checked lexically, the same on every OS: it is split at `/` and `\` (a
`\\?\` verbatim path does not treat `/` as a separator, so `<root>/..` once looked like a named directory on Windows),
may contain no `..` segment, and must end in a real name (not `.`, `/`, a bare drive). Verbatim `\\?\C:` and
`\\?\UNC` prefixes are stripped on both sides of every comparison; other namespaces (`\\.\`, `\\?\GLOBALROOT`,
`\\?\Volume{..}`) are refused. On Windows a name with `:` (alternate data stream), a trailing dot or space, or a DOS
device name (`NUL`, `COM1`, ...) is refused too. Canonical forms are compared exactly, not case-insensitively: both
sides come from `canonicalize`, which yields the on-disk case and long names (8.3 short names are expanded), and a
case-insensitive match could equate two directories of a case-sensitive Windows directory. Otherwise a package could ship `data -> ~/.ssh` and `fs.read:data` would expose it. Only
the `home` root (a place the user chose) may be a link. The `data` directory is created by the host, never taken from the
package; a root that is a file is `Missing`.

### Outgoing HTTP

wasmtime-wasi-http is built without its default sender (it brings tokio-rustls and `webpki-roots`, licence
`CDLA-Permissive-2.0`, and a second TLS stack). The send hook maps the request to an `EgressRequest` (body over 8 MiB is
`HttpRequestBodySize`) and calls the plugin's `Egress`, created when the instance is built from `EgressFactory::for_plugin(id, effective)`.
`EgressError` maps to the wasi-http error codes: `Denied` to `HttpRequestDenied`, `Dns` to `DnsError`, `Connect` to
`ConnectionRefused`, `Tls` to `TlsProtocolError`, `TooLarge` to `HttpResponseBodySize`, `Protocol` to `HttpProtocolError`.
Without the `net` feature the factory is `NoEgress`: every request is denied.

`NetEgress` (feature `net`) checks, in this order:

1. scheme `http` or `https` only (`BadScheme`);
2. the effective set has a `net.http` capability (`NoCapability`);
3. the host: exactly one trailing dot is stripped, the name is lowercased and **never IDNA-decoded** (punycode stays
   `xn--`, also in messages); userinfo, IP literals in any notation and empty labels (`a.com..`) are `BadHost`;
4. `effective.allows_http(host, port)` with the default ports 80 and 443 (`HostNotGranted`);
5. resolution through a vetted resolver: the exact name `localhost` is answered `127.0.0.1` / `::1` without a lookup (it
   is reachable only when a pattern names `localhost`); any other name goes through the system resolver and the request is
   refused if **any** address is forbidden or there is none, so DNS rebinding and `nip.io`-style names fail. The
   connection uses the vetted list, not a second lookup.

So the claim "the sandbox never reaches a non-public address" holds for every name except an explicitly granted
`localhost`, which reaches loopback on purpose (the tests and local services rely on it).

Forbidden addresses: unspecified, loopback, private, CGNAT `100.64/10`, link-local, multicast, broadcast, reserved
`240/4`, documentation, benchmarking `198.18/15`, `192.0.0/24`, ULA `fc00::/7`, `fe80::/10`, `fec0::/10`,
`2001:db8::/32`, and the IPv4-mapped, NAT64 `64:ff9b::/96`, 6to4 and Teredo forms of a forbidden v4.

The client has `no_proxy()` (an environment proxy would route around the check) and `redirect(Policy::none())` (a 3xx
is returned to the guest, never followed, so a redirect cannot reach an ungranted host). A guest `host` header and the
hop-by-hop headers are dropped. Bodies are buffered: 8 MiB for a request, 16 MiB for a response (checked on
`content-length` and while reading). Connect timeout 10 s, total 30 s. Streaming is a later, additive change.

### Grants, settings and logs

The effective set is **enforced per call, not by the linker**. One shared `Linker` links every interface for every
plugin, so an import is present whether or not it is granted. What denies is the per-call check: `effective` and
`sqler_plugin_api::gate` in the host interfaces and the `unbound` stubs, the `Egress` of the plugin (denying all without a
`net.http` grant), and the empty preopen set. P5 and P8 to P12 must therefore not assume that an import is absent when
ungranted, and must gate every call themselves.

The host reads grants through `GrantSource::approved(&GrantKey) -> Option<CapabilitySet>`, defined in
`sqler-plugin-api` (ADR-0059) and re-exported by this crate; the grants store of P6 implements it. The effective set is `declared ∩ approved`; a key the user never approved gives the empty set, and the plugin still loads and
runs (denied calls return `capability-denied`). Because the key contains the component digest, a swapped component has no
grant.

Settings are read-only for guests (`settings.get`). A key must parse as a `SettingKey` and be declared; a stored value of
another variant than the declared default yields the default. `write_setting` (the app, P7) refuses undeclared keys and
type mismatches. Log lines go through a token bucket (capacity 100, refill 20 lines per second); when the bucket has
refilled, one extra line says how many were suppressed. A line is cut at 4096 bytes on a character boundary and `\r`,
`\n` and other control characters are escaped, so a guest cannot forge log lines.

### Error taxonomy

`LoadError` (discovery and compile; `is_incompatible` is true for `Incompatible` and `Link`, the ADR-0055 "same kind"
rule), `CallError` (the table above plus `MissingExport`, `NotFound`, `Load`, `Engine`), `GuestError` (the WIT
`error-code`; the host's `From` over the generated enum is exhaustive, so a new WIT case fails the build), `SettingsError`.
All are `#[non_exhaustive]` and `Clone`.

### Extension protocol for later items

Each of P5 (`query`, `catalog`), P8 (`ui`), P9 (`driver`), P10 (`export`), P11 (`completion`), P12 (`ai`) owns
`src/<name>/**`, adds one `#[cfg(feature = "runtime")] mod <name>;` line in `lib.rs` and one line in
`runtime/link.rs::add_imports`. P5 and P8 delete their `runtime/unbound/<interface>.rs` and its `mod` line. The `unbound`
stubs apply `sqler_plugin_api::gate` first (a missing capability is `capability-denied`) and otherwise answer `failed`
"not available in this host". Host traits an item needs go into `Services` (a `TypeId` map). Exports are looked up per
interface with the generated `GuestIndices` from `Live::pre`, never through a world: a component that lacks an export
would otherwise fail to instantiate. The temporary `ui-handler` seam (`ui_seam`, with `call_command` and `call_status`)
was removed in P8; the typed `PluginHost::{run_command, render_panel, panel_event, status}` replace it
([ADR-0060](0060-plugins-in-the-workbench.md), Addendum P8).

### Lint outcome

The generated bindings (`wasmtime::component::bindgen!`, world `full`, async imports and exports) compile under
`#![forbid(unsafe_code)]`, `missing_docs` and clippy pedantic with `-D warnings` and no `allow`. The crate keeps
`[lints] workspace = true`. The guest side (`wit-bindgen`) still needs the `unsafe` exception in `plugins/`
(AGENTS.md section 4).

### Measurements

macOS 27.0.1 arm64, release build of `examples/probe` with `cl,net`, one run, `probe` and `smoke` fixtures (release
guests; the debug probe guest is 5.0 MB):

| plugin | engine create | compile | instantiate p50 / p95 | `init` p50 |
| ------ | ------------- | ------- | --------------------- | ---------- |
| `smoke` (15 KB) | 1.34 ms | 16.6 ms | 14 / 21 us | 875 ns |
| `probe` (120 KB) | 0.27 ms | 58.4 ms | 15 / 18 us | 667 ns |
| `probe` (debug, 5.0 MB) | 0.21 ms | 102.8 ms | 18 / 23 us | 1,458 ns |

Instantiate stays three orders of magnitude under the ADR-0054 assumed budget (p95 5 ms); compile is paid once per
digest. The release `probe` example with `cl,net` is **11,718,192 bytes** on macOS arm64 (it carries the host core,
wasmtime with Cranelift and reqwest with rustls/ring, stripped by the workspace profile). It is not comparable with the
ADR-0054 Linux figure (+14.5 MB for `cl` over the 59.9 MB app); P7 measures the real delta inside `sqler-app`.

## Alternatives rejected

- **Fuel.** ADR-0054: epochs cost about nothing with Cranelift and, unlike fuel, are tied to wall-clock time, which is the
  limit a user perceives. Fuel stays off.
- **wasi-http default sender.** A second TLS stack, `webpki-roots` (`CDLA-Permissive-2.0`, outside the allowed
  licences) and no hook for the host check. The `Egress` trait is the single place where grants, resolution and caps are
  applied.
- **On-disk compilation cache.** It would have to be keyed on the digest, the wasmtime version and the engine
  configuration, and invalidated safely; compile is paid once per process for a plugin that is actually used.
- **A `Store` per call.** Instantiating costs 14 to 18 us, but `init` would run per call and plugin state would not
  survive between calls. One store per plugin, dropped on a fault, keeps state and isolates faults.

## Consequences

- The shipped binary is unchanged. P7 decides which feature `sqler-app` turns on, with its own size measurement.
- `PluginHost::new` does no I/O and starts no thread; `is_idle()` is true until the first compile, so the app can assert
  that the first frame did no plugin work. Discovery is blocking I/O and belongs off the UI thread.
- A guest can read clocks and random numbers (stated above); it cannot reach env, args, stdin, sockets or any path outside
  its approved read-only preopens, and it reaches the network only through `Egress`.
- Shutdown never wedges a call. `shutdown` sets its flag, cancels every call and stops the epoch thread; a call that
  races with it either completes or ends with an `Engine` "shut down" error. Handing out the "call in flight" guard that
  keeps the epoch thread alive checks the stop flag under the mutex `stop` uses, so guest code never runs without a
  ticking epoch (a spinning guest would then ignore its deadline and the cancel token). The engine is not created after
  `shutdown` either.
- Fault accounting happens while the plugin lock is held, so a queued call sees the disable the third fault caused. The
  log rate limit belongs to the plugin, not to an instance: a re-instantiation after a fault or an exit does not refill it.
- Residual, local-process only: the `fs.read` scope check canonicalises the scope path and the preopen opens it by path
  again, two lookups. A process that can write to the plugin root could swap a directory between them; guests have no
  write capability, so only another local process can. Inside a preopen, cap-std refuses `..`, absolute paths and
  symlinks that leave it; the guest test `a_symlink_inside_a_granted_scope_cannot_lead_out_of_it` proves it.
- P7 must give plugins their own tokio runtime, not the `sqler-db` one: a spinning guest holds an executor worker for one
  poll per epoch tick until its deadline (up to 300 s for a driver call). P7 must also never add `target/plugins` as a
  default plugin path in release builds (the probe component is staged there). A development plugin with the id of an
  installed one shadows it and reads that plugin's `config_plugins/<id>/data` once approved; dev paths are explicit user
  input, so this is accepted.
- A spinning or sleeping guest costs at most its deadline of one executor slot per tick; three consecutive faults turn a
  plugin off until the user enables it.
- `.cwasm` loading, streaming HTTP bodies and a guest shutdown hook are later, additive changes.
- Windows: canonical paths are compared with the `\\?\` prefix stripped on both sides; symlink tests skip when the
  privilege is missing. The lexical root rules are unit-tested on every OS; the reparse-point check, junction and
  stream/device-name behaviour of real Windows file systems are only exercised by the Windows CI job.

## References

- [ADR-0002](0002-plugin-abi-wasm-component-model.md), [ADR-0054](0054-plugin-runtime-and-driver-feasibility.md),
  [ADR-0055](0055-plugin-abi-wit.md), [ADR-0056](0056-plugin-sandbox-and-capabilities.md)
- [Plugin host architecture](../architecture/plugins.md)
- `crates/sqler-plugin-host/`, `plugins/fixtures/probe/`, `crates/sqler-plugin-host/tests/guests.rs`
