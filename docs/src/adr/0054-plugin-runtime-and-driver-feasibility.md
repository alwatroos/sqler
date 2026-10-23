# ADR-0054: Plugin runtime and driver feasibility (P1 spike)

- Status: accepted
- Superseded in part by [ADR-0069](0069-binary-size-budget-70-mb.md) (2026-10-23): the binary size budget is now 70 MB (73,400,320 bytes); the 60 MB figures below are the budget at the time of writing.
- Date: 2026-10-22
- Deciders: SQLer maintainers (decided by the team; the product owner is informed of the items marked below)

## Context and Problem Statement

[ADR-0002](0002-plugin-abi-wasm-component-model.md) chose WebAssembly components on wasmtime for plugins. Three
things were still open before the plugin host (M6) is built:

1. What does embedding wasmtime cost in binary size, start-up latency and memory, on all three OSes, against the
   60 MB (62,914,560 bytes) budget of the shipped binary?
2. Can the two database engines that the first-party plugins need, DuckDB and SQL Server, run as WASI 0.2 guests?
3. Which guest toolchain, and which way to limit a guest's CPU time (fuel or epoch interruption)?

This ADR records the P1 spike (`spikes/plugin-runtime/`, a throwaway cargo workspace outside the root one, CI in
`.github/workflows/plugin-spike.yml`). Final run: GitHub Actions run `39769856073`, commit `e7930f2` on branch
`plugin-runtime-spike` (all jobs green; the DuckDB guest and the plain SQL Server build report `fail` through their
result keys, as expected). The spike code is not merged into `crates/`.

## Decision Drivers

- Release binary below 60 MB on Linux; today's `main` is already close ([size.yml](../benchmarks.md) run `39765240448`:
  59,872,488 bytes, so the headroom is 3,042,072 bytes, about 2.9 MiB).
- Assumed budgets (team-set metrics, reversible in P4/P7): instantiate p95 <= 5 ms, call p50 <= 10 us, a 1 MiB size
  reserve.
- No fork or patch of a third-party crate to maintain (supply-chain and maintenance risk).
- No new `cargo deny` ignore (AGENTS.md section 10), no `rsa` crate.

## Considered Options

- Embed wasmtime in the shipped binary (Cranelift, Winch, Pulley interpreter) and run DuckDB / SQL Server as guests.
- Embed wasmtime, but run the two engines natively behind the driver seam (the fallback designed in the plan).
- No plugin runtime in the shipped binary.

## Decision Outcome

Decided by the team:

1. **DuckDB as a WASI 0.2 guest: NO-GO.**
2. **SQL Server (`tiberius`) as a guest: NO-GO as a shipped dependency.** It runs only with a stand-in for one
   transitive crate.
3. **Both engines use the fallback.** The plugin (manifest, dialect, catalog SQL, UI contributions) lives in the repo
   under `plugins/`. The native engine is provided by the host through the driver seam (P9), in the existing stub
   crates `sqler-driver-duckdb` and `sqler-driver-mssql` behind cargo features. P15 and P16 validate the plugin API
   with that shape and record API gaps. P17's ADR-0063 decides whether a native engine is promoted into a guest.
4. **The host `plugins` feature is OFF by default in the shipped binary** (no variant fits the headroom, see
   "Runtime configuration"). The host is written so the wasmtime engine is a cargo feature choice, with `cl` and
   `pulley-rt` as the two supported engine features.
5. **Epoch interruption** limits wall-clock time of a guest call; fuel only where deterministic accounting is needed.
6. **Guest toolchain: `wit-bindgen` + `cargo build --target wasm32-wasip2`.**

Product owner informed (not a team-internal detail): the size default of item 4 (a build with plugin support is not
the default build for now), and that DuckDB and SQL Server will be native engines behind features, not sandboxed
guests.

### Runtime configuration

Host crate `plugin-spike-host`, wasmtime and wasmtime-wasi 50.0.0 (`default-features = false`, verified live on
crates.io, not yanked), `memory-stats` 1.2.0. Feature sets, as printed by the host's `info` command:

| variant | wasmtime features (plus `wasmtime-wasi/p2`) |
| ------- | ------------------------------------------- |
| `cl` | `runtime, component-model, async, std, cranelift` |
| `cl-default` | wasmtime default features (reference only) |
| `pulley` | `runtime, component-model, async, std, cranelift, pulley` (compiles to Pulley, `Config::target("pulley64")`) |
| `pulley-rt` | `runtime, component-model, async, std, pulley` (interpreter, **no compiler**; loads a `.cwasm`) |
| `winch` | `runtime, component-model, async, std, winch` |

`async` is required by `wasmtime-wasi/p2`; in 50.0 synchronous calls work with it.

Binary size, CI figures (run 39769856073), root release profile (`strip = "symbols"`, `codegen-units = 1`), bytes.
Delta = variant minus `base` of the same OS. The delta is an upper bound: in the app, `std` and shared dependencies
overlap.

| variant | Linux | Linux delta | macOS delta | Windows delta |
| ------- | ----- | ----------- | ----------- | ------------- |
| base | 358,880 | 0 | 0 (360,368) | 0 (157,696) |
| cl | 14,883,440 | 14,524,560 | 10,437,824 | 15,969,280 |
| cl-default | 19,075,728 | 18,716,848 | 13,967,120 | 19,948,032 |
| pulley | 14,995,456 | 14,636,576 | 10,542,016 | 16,070,144 |
| pulley-rt | 3,959,968 | 3,601,088 | 3,166,928 | 4,358,144 |
| winch | 14,598,608 | 14,239,728 | 10,058,848 | 15,640,064 |

Linux headroom: 62,914,560 - 59,872,488 = 3,042,072 bytes (1,993,496 after the 1 MiB reserve).

| variant | Linux binary with the delta | over the 60 MB budget by |
| ------- | --------------------------- | ------------------------ |
| cl | 74,397,048 | 11,482,488 |
| winch | 74,112,216 | 11,197,656 |
| pulley | 74,509,064 | 11,594,504 |
| pulley-rt | 63,473,576 | 559,016 |

Decision rule from the plan: the smallest variant whose Linux delta fits the headroom with a 1 MiB reserve and whose
`instantiate_p95_us` <= 5000 and `call_p50_ns` <= 10000. Latency passes everywhere (below), size passes nowhere: the
smallest variant, `pulley-rt`, is 1,607,592 bytes over the headroom-with-reserve and 559,016 bytes over the budget
itself. So **`plugins` is off by default (P4, P7)**.

Latency, memory and limits (CI, hello fixture; instantiate = Store + WasiCtx + instantiate from an `InstancePre`):

| OS | variant | compile ms | load .cwasm ms | instantiate p50/p95 us | call p50/p95 ns | idle KiB/instance |
| -- | ------- | ---------- | -------------- | ---------------------- | --------------- | ----------------- |
| linux | cl | 9.2 | n/a | 8 / 9 | 60 / 61 | 0.3 (probably an RSS artifact) |
| linux | winch | 3.8 | n/a | 10 / 11 | 101 / 111 | 2.4 |
| linux | pulley | 12.1 | n/a | 6 / 7 | 180 / 200 | 4.0 |
| linux | pulley-rt | n/a | 0.16 | 6 / 7 | 230 / 260 | 11.3 |
| windows | cl | 26.5 | n/a | 10 / 21 | 100 / 200 | 10.4 |
| windows | winch | 5.5 | n/a | 9 / 14 | 100 / 100 | 10.5 |
| windows | pulley-rt | n/a | 0.45 | 42 / 71 | 400 / 500 | 80.6 |
| macos | cl | 14.0 | n/a | 10 / 34 | 83 / 208 | 21.4 |
| macos | pulley-rt | n/a | 0.25 | 32 / 111 | 167 / 167 | 88.5 |

All variants are far inside the assumed budgets (5 ms, 10 us). Compare cl with the interpreter on CPU-bound work
(`spin`, Linux): 18.7 ms with Cranelift and 121.3 ms with Pulley (6.5 times slower), plus Winch 24.9 ms. macOS timers
tick at about 41.7 ns and Windows at 100 ns, so the small call figures are quantised. `cl-default` (wasmtime default
features) costs 4.2 MB more than `cl` and 421 ns per call against 60 ns on Linux; it is not used.

#### What `pulley-rt` costs, and why it is not the default yet

`pulley-rt` is a viable shape for a shipped default. It adds 3.6 MB (Linux), 3.2 MB (macOS) and 4.4 MB (Windows), loads
a precompiled guest in 0.16 to 0.45 ms and instantiates in well under a millisecond. The costs:

- **Guests must be precompiled at build time.** There is no compiler in the binary, so a plugin cannot be compiled when
  the user installs it. A `.cwasm` is bound to the exact wasmtime version and engine configuration (fuel or epoch
  setting, memory settings) of the binary that loads it; a mismatch is rejected. Plugin distribution would ship either
  `.cwasm` for the Pulley target built against the same wasmtime as the app, or the app would have to carry a compiler.
  Whether one Pulley `.cwasm` loads on every OS was not tested here (the CI built one per OS).
- **Interpreter speed**: about 6.5 times slower than native code on CPU-bound work, 4 times slower per call (230 ns
  against 60 ns). Fine for plugin glue (manifest, dialect, catalog SQL, UI trees); not for a guest that scans rows.
- It does not fit today: +3.6 MB against 3.0 MB of headroom, and `main` keeps growing (M5, M6).

Options to ship a runtime anyway:

| option | cost | note |
| ------ | ---- | ---- |
| `pulley-rt` default | +3.6 MB Linux, precompile at build time | needs about 0.6 MB freed (or 1.6 MB with the reserve); the real delta in the app is smaller than this standalone figure, measure it in P7 |
| `plugins-full` (a second build with `cl`) | +14.5 MB Linux (74.4 MB total) | over the 60 MB budget; a separate artifact, not the default download |
| lazily downloaded helper (a separate process or library with Cranelift) | 0 in the main binary | needs a signed download, a version match, an IPC or loading story; P4 decides if it is worth it |

**Recommendation for P4 and P7:** build the host so the engine is a cargo feature (`cl` for tests and `plugins-full`,
`pulley-rt` for the compact shape), ship with `plugins` OFF by default, and in P7 measure the real size delta of
`pulley-rt` inside `sqler-app`. If the real delta is at most the headroom minus the 1 MiB reserve, turn `pulley-rt`
on by default and precompile guests in the build; otherwise keep it off and publish `plugins-full`. Winch is not
worth its size (it is as large as `cl` and slower); Pulley with a compiler (`pulley`) only matters for building the
`.cwasm` files and belongs in a build tool, not in the app. On x86_64 (Linux, Windows) and arm64 (macOS) Winch built
and ran the fixture without a fallback (`winch=n/a` was never printed).

#### Fuel or epoch

Both interrupt an unbounded `spin` within 1 s on every variant and OS (`fuel_interrupt_ok = epoch_interrupt_ok = 1`).
Overhead on the `spin` loop against the plain engine, in percent (fuel / epoch; negative values are noise):

| OS | cl | winch | pulley |
| -- | -- | ----- | ------ |
| linux | -0.5 / -0.7 | 24.8 / 1.8 | 62.1 / 76.0 |
| macos | -6.4 / 1.3 | 76.8 / 8.0 | 94.1 / 124.3 |
| windows | 11.4 / -0.8 | 1.0 / -3.0 | 63.0 / 75.4 |

**Recommendation: epoch interruption for wall-clock limits** (a ticker thread bumping the epoch every 10 ms, a
deadline per call). It costs about nothing with Cranelift and little with Winch, and the rule "epoch unless fuel is
the only one that works or epoch costs more" holds for `cl` and `winch`. With the Pulley interpreter epoch costs 11
to 30 points more than fuel; if `pulley-rt` is the shipped engine, compare once in P4, but both are small next to the
interpreter's own slowdown. Use fuel only where a deterministic budget is needed (reproducible tests). Fuel must be
set on the Store before instantiation (component start code consumes fuel, otherwise `all fuel consumed` at
instantiate). The epoch/fuel mode is part of the engine configuration, so it also has to match at precompile time for
`.cwasm` files; the spike's `pulley-rt` bench could not run fuel/epoch for that reason (the `pulley` variant carries the
figures).

### Guest toolchain

**`wit-bindgen` 0.63.0 + `cargo build --target wasm32-wasip2`** (rustc 1.99.1). Both routes built and ran the fixture
(`fixture_add = 5`) on CI:

| | wit-bindgen + wasm32-wasip2 | cargo-component 0.21.1 |
| - | --------------------------- | ---------------------- |
| hello component | 13,475 bytes | 12,658 bytes |
| target | `wasm32-wasip2` directly (a component out of the box) | builds `wasm32-wasip1` and adapts it |
| bindings | wit-bindgen 0.63.0 | wit-bindgen 0.41.0 behind `wit-bindgen-rt`, generated into `src/bindings.rs` |
| extra install | none (rustup target) | `cargo install cargo-component`, about 91 s; last release 2025-03-18 |

The size difference is irrelevant. The direct route needs no extra tool, follows the current `wit-bindgen`, and the
stale `cargo-component` is a risk for plugin authors. The `wasm32-wasip2` target is a rustup target, so CI uses
`rustup target add wasm32-wasip2`. Note: `cargo info wit-bindgen` printed 0.61.1 from a stale index; the crates.io
API said 0.63.0.

### DuckDB: NO-GO

Evidence: `spikes/plugin-runtime/guests/duckdb/RESULT.md`, CI run 39769856073 (Linux, macOS, Windows hosts).

- The `duckdb` crate does not build for `wasm32-wasip2` (unconditional `arrow` -> `comfy-table` -> `crossterm`, no
  WASI backend). The spike used `libduckdb-sys` 1.10506.1 (bundled C++, raw C API), wasi-sdk 34.0 clang++, six
  configurations (the box).
- DuckDB builds (29,678,977 bytes in CI, `guest_duckdb_build = ok`) with force-included shim headers and six link-time
  stubs, no DuckDB source touched, but it needs the **legacy** wasm exception encoding (`try`/`catch`): wasi-sdk 34
  emits it and wasmtime 50 rejects it (`failed to parse WebAssembly module`; `legacy_exceptions feature required for
  try instruction`). `guest_duckdb_select1 = fail` on Linux, macOS and Windows.
- No pure-Rust engine that reads DuckDB files exists on crates.io (searched live on 2026-10-22).
- Not fixable without a different toolchain output (an exnref-encoding sysroot) or a fork; either is out of scope.

### SQL Server (`tiberius`): NO-GO as shipped

Evidence: `spikes/plugin-runtime/guests/mssql/RESULT.md`, CI run 39769856073 (Linux job `mssql`).

- `tiberius` 0.13.0 with rustls on ring does not compile for `wasm32-wasip2`: its `rustls` feature forces
  `rustls-native-certs` 0.8.5, which has no wasip2 backend (`cannot find module or crate platform`). Not avoidable
  with features. It also pulls `aws-lc-rs` through `tokio-rustls` defaults; ring is selected by installing a process
  default provider.
- With a stand-in crate for `rustls-native-certs` (wired in through `cargo --config patch...`, kept under
  `experiments/`) the guest worked end to end: std `TcpStream` behind a `futures-io` adapter, `select 1` = 1 over
  **verified** TLS (CA given through a read-only `--dir`), and a connection to an address not in `--allow-tcp` refused
  (`guest_mssql_select1 = 1`, `_tls = verified`, `_denied_ok = 1`, 2.1 MB component, compile 2.2 s, run 13.6 ms,
  74 MiB RSS on the CI Linux runner).
- It also needs workarounds: a 1 ms read timeout with `Pending` on `WouldBlock` (a plain blocking read deadlocks
  the TLS handshake) and no handshake/command timeouts (`futures-timer` needs a thread).
- A maintained `[patch]` on a TLS crate, plus those workarounds, is a supply-chain and maintenance risk, so the team
  does not accept it as a shipped dependency. An upstream change (making the dependency optional or adding a no-op
  backend) would reopen this; P17's ADR-0063 is the place.

### Host bugs the guest runs found (fixed in the spike host)

`run-command` ran for the first time with the SQL Server guest. Two host bugs were found and fixed in
`host/src/rt.rs`, and the SQL Server run was repeated against a local container (SQL Server 2022 CU27 emulated):
`select1 = 1`, `tls = verified`, `denied_ok = 1`.

- wasmtime-wasi 50 defaults to TCP off: `b.allow_tcp(true)` is needed, with the address check doing the limiting.
- Rust std on wasip2 binds implicitly before `connect`: `socket_addr_check` allows `TcpBind` for `0.0.0.0:0` only;
  `TcpConnect` stays limited to `--allow-tcp`. Without that, `denied_ok = 1` would be a false positive.

These are API facts for the plugin host (P4): a `net` capability maps to `allow_tcp` plus an address check that
permits only the declared hosts, and a capability that is not declared leaves TCP off.

### The fallback in detail

- `plugins/duckdb/` and `plugins/mssql/` (P15, P16) hold the plugin: manifest `sqler.toml`, dialect, catalog SQL and
  UI contributions, written against the real plugin API.
- The engine is native code in `sqler-driver-duckdb` / `sqler-driver-mssql` behind cargo features, reached through the
  driver seam (P9). The plugin names the driver capability; the host runs the engine.
- This validates the API shape without a guest engine. P15 and P16 record API gaps. If the plugin parts themselves
  are guests, they need the runtime; whether declarative parts can load without wasmtime is a question for P4/P7.
- The native engine is the promotion candidate: if a later toolchain (wasi-sdk exceptions in the exnref encoding,
  an upstream fix for `rustls-native-certs`) makes a guest engine GO, ADR-0063 can promote it.
- The native DuckDB driver has the same `libduckdb-sys` build-dependency chain (`ureq` -> `webpki-roots`,
  `CDLA-Permissive-2.0`), which the root `deny.toml` does not allow. That is a licence decision for the owner when
  the native driver is built.

### Supply-chain check (`cargo deny`)

The `deny` job runs `cargo deny` 0.20.2 with the root `deny.toml` (`--config`, not edited) for the host workspace and
the four guest workspaces: green in run 39769856073. No new advisory ignore and no `rsa` crate. Two points:

- The tree of `wasmtime-wasi` is heavy (`wit-component`, `icu_*`, `url`); licences pass.
- `guests/duckdb` is skipped for the **licences** check only (advisories, bans and sources still run), with a comment
  in the workflow. It is a NO-GO guest that is never shipped; its only failure is `webpki-roots` 1.0.10
  (`CDLA-Permissive-2.0`) through the `libduckdb-sys` build-dependency `ureq`. The alternative, adding the licence to
  the root allow list, is a policy decision and was not made.

### Consequences

- The shipped binary has no wasmtime for now; the 60 MB budget stays intact. Third-party guest plugins need a
  `plugins` build until P7 decides on `pulley-rt` or a helper.
- P4 designs the host around a cargo feature for the engine and an epoch ticker for time limits; guests are
  `wasm32-wasip2` components built with `wit-bindgen`.
- DuckDB and SQL Server ship as native engines behind features. Their binary-size cost is a P15/P16/P9 matter, not
  part of this ADR.
- Spike limits: figures are for a trivial fixture (add, spin), not for a real plugin; macOS numbers use CI runners
  for comparability; Linux idle memory per instance is below the RSS granularity; Winch fuel/epoch percentages are
  noisy.

### Confirmation

The CI run above (`.github/workflows/plugin-spike.yml`, artifact `plugin-spike-summary`, 30 days) and the local SQL
Server re-run after the host fixes. Raw tables and quirks: `spikes/plugin-runtime/README.md`.

## Pros and Cons of the Options

### Embed wasmtime and run both engines as guests

- Bad: DuckDB does not load (legacy exceptions); SQL Server needs a patched TLS crate.

### Embed wasmtime, engines native behind the driver seam (chosen)

- Good: no unproven toolchain in the product path; the plugin API is validated on two real engines.
- Bad: the engines are not sandboxed and add their native size to the binary.

### No plugin runtime in the shipped binary

- Good: no size cost.
- Bad: no third-party plugins; ADR-0002 would have no runtime. Kept as the default until the size question is
  answered, not as the end state.

## More Information

- [ADR-0002](0002-plugin-abi-wasm-component-model.md), [Dependencies](../dependencies.md)
- Spike code: `spikes/plugin-runtime/` (`README.md`, `guests/duckdb/RESULT.md`, `guests/mssql/RESULT.md`)
- CI run `39769856073`, commit `e7930f2`. A later push only changed documentation under the same paths.
- Earlier validation run `39765816216` (commit `25a4a38`): same sizes within a few hundred bytes (Linux `cl` 14,883,584).
