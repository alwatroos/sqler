# SQL Server (tiberius) guest on wasm32-wasip2: RESULT

**Verdict: NO-GO as shipped, with a working workaround that needs a one-crate stand-in.**
`select 1` over verified TLS (rustls on ring), a read-only CA from `--dir`, and a refused connection to an unlisted
port all worked under wasmtime 50.0.0, **but only after replacing the transitive crate `rustls-native-certs` with a
stub** (`experiments/native-certs-stub`). The unmodified dependency tree does not compile for `wasm32-wasip2`. By the
rule "no patching of third-party sources" that is NO-GO; the workaround is reported so the owner can weigh it (it is
about 15 lines and an upstream issue, but it would be a `[patch]` to maintain).

## GO/NO-GO criteria (set before the attempts)
GO needs all of: (1) builds for `wasm32-wasip2` from unmodified crates.io sources, (2) runs in the spike host,
(3) `select 1` prints `SPIKE guest_mssql_select1=1` over TLS with certificate verification against a CA given by
`--dir` (`guest_mssql_tls=verified`), (4) a connection to an address not in `--allow-tcp` is refused
(`guest_mssql_denied_ok=1`). Timebox: 6 build configurations or 4 hours. Used: 5 configurations.

## Environment
| Item | Value |
| ---- | ----- |
| Machine | macOS arm64 (Apple Silicon), local; SQL Server in Docker 29.9.1 as `linux/amd64` (emulated) |
| rustc / cargo | 1.99.1 (throwaway rustup under `.tools/`), target `wasm32-wasip2` |
| wasi-sdk | 34.0, clang 23.1.0-wasi-sdk (for the C part of ring / aws-lc-sys) |
| Crates (all verified live, none yanked) | `tiberius` 0.13.0, `rustls` 0.23.46 (features `ring,std,tls12`), `ring` 0.17.14, `aws-lc-rs` 1.18.2 / `aws-lc-sys` 0.46.0 (forced by tiberius), `tokio-rustls` 0.26.6, `rustls-native-certs` 0.8.5, `futures` 0.3.34, `futures-timer` 3.0.4 |
| Server | `mcr.microsoft.com/mssql/server:2022-CU27-ubuntu-22.04` (tag verified in `tags/list` on 2026-10-22), SQL Server 2022 RTM-CU27 16.0.4295.3 |
| Server TLS | own CA, certificate `CN=localhost`, `subjectAltName=DNS:localhost`, `tlsprotocols` 1.2, `forceencryption = 1` (`start-mssql.sh`) |
| Runtime | `plugin-spike-host` variant `cl`, wasmtime/wasmtime-wasi 50.0.0 |
| Command (final) | `sh guests/mssql/build.sh <out>` and `sh guests/mssql/run.sh <host-bin> <out> <out>` |

## Attempts
### 1. tiberius `rustls` + `tds73`, default build (FAIL, crate level)
`cargo build --release --target wasm32-wasip2` with wasi-sdk clang as `CC_wasm32_wasip2`.
`rustls-native-certs` 0.8.5 has no backend for the target (`cannot find module or crate platform in this scope`).
tiberius' `rustls` feature enables it unconditionally (`rustls = [tokio-rustls, tokio-util, rustls-native-certs,
rustls-webpki, rustls-pki-types]`); `rustls-webpki-roots` includes `rustls`. Not avoidable with features.
Second finding from `cargo tree`: tiberius pulls `tokio-rustls` with default features, so `aws-lc-rs` is always
built; ring has to be selected by installing a process-default provider (tiberius 0.13 uses an installed provider and
falls back to aws-lc-rs).
Log tail:
```
   Compiling pkg-config v0.3.34
   Compiling fs_extra v1.3.0
   Compiling dunce v1.0.5
   Compiling pin-project-lite v0.2.17
   Compiling zeroize v1.9.1
   Compiling aws-lc-rs v1.18.2
   Compiling futures-sink v0.3.34
   Compiling futures-core v0.3.34
   Compiling rustversion v1.0.23
   Compiling futures-io v0.3.34
   Compiling futures-channel v0.3.34
   Compiling once_cell v1.21.4
   Compiling rustls-pki-types v1.15.1
   Compiling slab v0.4.12
   Compiling memchr v2.8.3
   Compiling futures-task v0.3.34
   Compiling rustls v0.23.46
   Compiling log v0.4.34
   Compiling autocfg v1.5.1
   Compiling untrusted v0.9.0
   Compiling tokio v1.54.1
   Compiling subtle v2.6.1
   Compiling thiserror v2.0.21
   Compiling bytes v1.12.1
   Compiling tracing-core v0.1.36
   Compiling scopeguard v1.2.0
   Compiling multiversion_no_op v1.0.0
   Compiling cfg-if v1.0.5
   Compiling rustls-native-certs v0.8.5
   Compiling secrecy v0.10.3
error[E0433]: cannot find module or crate `platform` in this scope
   --> <spike>/.tools/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rustls-native-certs-0.8.5/src/lib.rs:123:14
    |
123 |         _ => platform::load_native_certs(),
    |              ^^^^^^^^ use of unresolved module or unlinked crate `platform`
    |
    = help: if you wanted to use a crate named `platform`, use `cargo add platform` to add it to your `Cargo.toml`
For more information about this error, try `rustc --explain E0433`.
error: could not compile `rustls-native-certs` (lib) due to 1 previous error
warning: build failed, waiting for other jobs to finish...
```
Probe (scratch crate, not in the repo): `rustls` with `ring` and, separately, `aws-lc-rs` both compile for
`wasm32-wasip2` with wasi-sdk clang. So the C crypto code is not the blocker.

### 2. tiberius without TLS (`--no-default-features`), plain TDS (BUILD OK, RUN OK after host fixes)
`guest_mssql_build=ok`, 718,081 bytes. Container in `plain` mode (encryption not forced), `EncryptionLevel::NotSupported`.
First run under the host failed: `tcp connect: Permission denied`. Cause is in the host, not the guest:
`run-command` never calls `allow_tcp(true)` (wasmtime-wasi default is TCP off), and Rust's `std` on wasip2 does an
implicit `bind` to `0.0.0.0:0` before `connect`, which `socket_addr_check` (TcpBind) also refuses.
With both fixed in a throwaway host copy (see Host findings): `SPIKE guest_mssql_select1=1`,
`SPIKE guest_mssql_denied_ok=1` (kind `PermissionDenied` for the unlisted port 14340). This proves the std
`TcpStream` + `futures::executor::block_on` path (no `wstd` needed).

### 3. TLS build with `rustls-native-certs` stub (BUILD OK, RUN FAIL: handshake hang)
`cargo build --release --target wasm32-wasip2 --config 'patch.crates-io.rustls-native-certs.path="experiments/native-certs-stub"'`
plus a direct `rustls` dependency (`ring`) and `install_default()` of the ring provider. 2,145,667 bytes.
Run: the guest stalled for about 110 s, then `peer closed connection without sending TLS close_notify`. The same
code compiled natively failed identically, so it was not WASI. A TCP proxy trace showed the server's full handshake
flight arriving and the client then waiting: tokio-rustls finishes its handshake with one more read that must
report `Pending`; my first adapter blocked in `read`. The server then timed out the login.

### 4. Adapter reads report `Pending` (BUILD OK, RUN FAIL: timer)
`Blocking::new` sets a 1 ms read timeout (`TcpStream::set_read_timeout`, supported by wasip2 std);
`poll_read` maps `WouldBlock`/`TimedOut` to `wake_by_ref` + `Pending`. Native run: `select 1` = 1 with verified TLS.
Under wasmtime: `panicked at futures-timer-3.0.4/src/native/delay.rs:126: timer has gone away` (tiberius wraps the
handshake in `futures_timer::Delay`, whose helper thread cannot be spawned on wasip2).

### 5. + `handshake_timeout(None)` and `command_timeout(None)` (BUILD OK, RUN OK with the stub)
Config only, no source patch. Final guest: 1,947,032 bytes (stub build via `build.sh`).
```
SPIKE guest_compile_ms=746.960
denied connect: kind=PermissionDenied msg=Permission denied (os error 2)
SPIKE guest_mssql_tls=verified
SPIKE guest_mssql_denied_ok=1
SPIKE guest_mssql_select1=1
SPIKE guest_run_ms=23.564
SPIKE guest_rss_kib=93248
```
`results.txt` written by `run.sh`: `guest_mssql_variant=stub`, `guest_mssql_select1=1`, `guest_mssql_tls=verified`,
`guest_mssql_denied_ok=1`. The default `build.sh` run also records `guest_mssql_build=fail`
(`guest_mssql_error=error[E0433]: cannot find module or crate platform in this scope`) plus
`guest_mssql_stub_build=ok`, `guest_mssql_stub_wasm_bytes=1947032`.

## Host findings (W1 file `host/src/rt.rs`, `run_command`; first execution of `run-command`)
1. Add `b.allow_tcp(true);` next to `socket_addr_check` (default is TCP off, so every connect is refused).
2. The check must allow `SocketAddrUse::TcpBind` for `0.0.0.0:0` (std binds before connecting), e.g.
   `ok = allowed.contains(&addr) || (matches!(use_, SocketAddrUse::TcpBind) && addr.ip().is_unspecified() && addr.port() == 0)`;
   `SocketAddrUse` is `wasmtime_wasi::sockets::SocketAddrUse`. TcpConnect to unlisted addresses stays refused.
3. `guest_mssql_denied_ok` is only meaningful with (2): before it, every connect (listed or not) was denied.
Both were applied only in a throwaway copy of the host under the scratchpad. With the unfixed W1 host, `run.sh`
reports `select1=fail` for mssql.

## Other findings
- `cargo deny` (0.20.2, root `deny.toml`): advisories ok, bans ok, licenses ok, sources ok. No ignore, no `rsa`.
- Library timeouts need threads: tiberius needs `handshake_timeout(None)` and `command_timeout(None)` on wasip2
  (no timeout on a stalled server; the host's epoch/fuel limits are the bound).
- Plain `std::net::TcpStream` is enough; `wstd` was not needed.
- The CA key stays out of the guest: `run.sh` preopens a directory holding only `ca.crt`.
- The server was slow to start under emulation only the first time; readiness is a bounded 180 s loop.

## Why NO-GO (and what would flip it)
- Criterion 1 fails: unmodified tiberius + rustls does not compile for the target (`rustls-native-certs`).
- It flips to GO if tiberius makes `rustls-native-certs` optional (upstream), or the owner accepts a `[patch]`
  stand-in (about 15 lines, kept in `experiments/native-certs-stub`), and if the host fixes above land.
- Everything else was demonstrated: ring and aws-lc-sys build with wasi-sdk, TLS verified against a CA from `--dir`,
  `std` sockets under `socket_addr_check`, refusal of unlisted addresses.

## Files
`Cargo.toml`, `Cargo.lock`, `src/main.rs`, `experiments/native-certs-stub/**`, `start-mssql.sh`, `build.sh`, `run.sh`.
Cleanup: `docker rm -f sqler-spike-mssql` (no named volumes, no network created).
