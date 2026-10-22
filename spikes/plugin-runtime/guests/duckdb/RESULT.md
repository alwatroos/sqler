# DuckDB guest on wasm32-wasip2: RESULT

**Verdict: NO-GO** (6 of 6 build configurations used; the 4-hour box was not the limit, the configuration count was).

DuckDB 1.10506.1 (bundled C++) can be *built* for `wasm32-wasip2`, but only with wasm exception handling in the
legacy encoding (`try`/`catch` opcodes), and wasmtime 50.0.0 does not support that encoding. The component does not
load: `failed to parse WebAssembly module`. No `select 1` ran.

## GO/NO-GO criteria (set before the attempts)
GO needs all of: (1) the guest builds for `wasm32-wasip2` from unmodified crates.io sources, (2) it loads and runs
in the spike host (wasmtime 50.0.0, Cranelift), (3) `select 1` on `:memory:` prints `SPIKE guest_duckdb_select1=1`.
No patches to DuckDB sources (that would be a fork to maintain). Link-time stubs and force-included shim headers in
the guest are allowed and are listed below. Timebox: 6 build configurations or 4 hours.

## Environment
| Item | Value |
| ---- | ----- |
| Host machine | macOS arm64 (Apple Silicon), local |
| rustc / cargo | 1.99.1 (throwaway rustup under `.tools/`), target `wasm32-wasip2` |
| wasi-sdk | 34.0 (`scripts/install-wasi-sdk.sh`, sha256 pinned), clang 23.1.0-wasi-sdk |
| Crate | `libduckdb-sys` 1.10506.1 with `bundled` (not yanked, `max_stable_version`, checked on crates.io 2026-10-22) |
| Runtime | `plugin-spike-host` variant `cl`, wasmtime/wasmtime-wasi 50.0.0 |
| Command | `sh guests/duckdb/build.sh <outdir>` then `sh guests/duckdb/run.sh <host-bin> <outdir> <outdir>` |
| Wall time per attempt | about 52 s with 12 cores (about 10 min CPU) for the C++ part |

## Pure-Rust alternative
Searched crates.io live (2026-10-22) for a pure-Rust engine that reads DuckDB files: queries "duckdb pure rust",
"duckdb file format reader", "duckdb". Result: none. Hits are wrappers or bindings (`duckdb`, `libduckdb-sys`,
`quack-rs`, forks such as `spiceai_duckdb_fork`, `uqa-*`), or tools using Parquet/other formats. `rudb-native` is its
own storage format, not DuckDB's. Expected outcome confirmed.

## Attempts
### 1. `duckdb` crate, feature `bundled`, wasi-sdk clang++ (FAIL, Rust level)
Flags: `CC/CXX/AR_wasm32_wasip2` = wasi-sdk, `CFLAGS/CXXFLAGS_wasm32_wasip2="--target=wasm32-wasip2 --sysroot=<sdk>/share/wasi-sysroot -O2"`.
Failure before any C++ is compiled: `duckdb` 1.10506.1 depends unconditionally on `arrow` (feature `prettyprint`) ->
`comfy-table` -> `crossterm` 0.28.1, which has no WASI backend (`cannot find function enable_raw_mode in module sys`).
Not fixable by features (`default = []` already). Moved to the raw C API crate `libduckdb-sys`.
Log tail (40 lines):
```
...
 18 |     set_size, set_window_title, size, window_size,
    |                                 ^^^^
error[E0425]: cannot find function `window_size` in module `sys`
   --> <spike>/.tools/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/crossterm-0.28.1/src/terminal.rs:154:10
    |
154 |     sys::window_size()
    |          ^^^^^^^^^^^ not found in `sys`
    |
note: found an item that was configured out
   --> <spike>/.tools/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/crossterm-0.28.1/src/terminal/sys.rs:8:67
    |
  6 | #[cfg(unix)]
    |       ---- the item is gated here
  7 | pub(crate) use self::unix::{
  8 |     disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, size, window_size,
    |                                                                   ^^^^^^^^^^^
note: found an item that was configured out
   --> <spike>/.tools/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/crossterm-0.28.1/src/terminal/sys.rs:18:39
    |
 15 | #[cfg(windows)]
    |       ------- the item is gated here
...
 18 |     set_size, set_window_title, size, window_size,
    |                                       ^^^^^^^^^^^
   Compiling pkg-config v0.3.34
error[E0308]: mismatched types
   --> <spike>/.tools/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/crossterm-0.28.1/src/terminal.rs:107:33
    |
107 | pub fn is_raw_mode_enabled() -> io::Result<bool> {
    |        -------------------      ^^^^^^^^^^^^^^^^ expected `Result<bool, Error>`, found `()`
    |        |
    |        implicitly returns `()` as its body has no tail or `return` expression
    |
    = note:   expected enum `Result<bool, std::io::Error>`
            found unit type `()`
Some errors have detailed explanations: E0308, E0425.
For more information about an error, try `rustc --explain E0308`.
error: could not compile `crossterm` (lib) due to 5 previous errors
warning: build failed, waiting for other jobs to finish...
```

### 2. `libduckdb-sys` (bundled), same flags (FAIL, C++ sources)
The guest now calls the C API (`duckdb_open_ext`, `duckdb_query`, ...) through `libduckdb-sys` (unsafe in one file).
First real C++ compile: 8 translation units fail on missing wasi-libc features: `mmap`/`munmap`/`madvise`/`mlock`
(`sys/mman.h` is behind `_WASI_EMULATED_MMAN`), `sched_getcpu`, `struct winsize`/`TIOCGWINSZ`, `net/if.h`,
`ifaddrs.h`, no semaphore wrapper in `concurrentqueue` (threads), and the macro `ERA` from `<langinfo.h>` clashes with
`DatePartSpecifier::ERA`.
Log tail:
```
  cargo:warning=      |         ~~~~~~~~~^
  cargo:warning=In file included from <spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/ub_src_parallel.cpp:33:
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/parallel/task_scheduler.cpp:473:13: error: use of undeclared identifier 'sched_ge
  cargo:warning=  473 |         auto cpu = sched_getcpu();
  cargo:warning=      |                    ^~~~~~~~~~~~
  cargo:warning=3 errors generated.
  exit status: 1
  cargo:warning=ToolExecError: command did not execute successfully (status code exit status: 1): "<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/bin/clang++" "-Os" "-ffunction-
  cargo:warning=In file included from <spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/ub_src_storage.cpp:15:
  cargo:warning=In file included from <spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/storage/block_allocator.cpp:12:
  cargo:warning=<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/share/wasi-sysroot/include/wasm32-wasip2/sys/mman.h:2:2: error: "WASI lacks a true mmap; to enable minimal mmap em
  cargo:warning=    2 | #error "WASI lacks a true mmap; to enable minimal mmap emulation, \
  cargo:warning=      |  ^
  cargo:warning=In file included from <spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/ub_src_storage.cpp:15:
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/storage/block_allocator.cpp:33:39: error: use of undeclared identifier 'PROT_READ
  cargo:warning=   33 |         const auto ptr = mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  cargo:warning=      |                                              ^~~~~~~~~
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/storage/block_allocator.cpp:33:51: error: use of undeclared identifier 'PROT_WRIT
  cargo:warning=   33 |         const auto ptr = mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  cargo:warning=      |                                                          ^~~~~~~~~~
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/storage/block_allocator.cpp:33:63: error: use of undeclared identifier 'MAP_PRIVA
  cargo:warning=   33 |         const auto ptr = mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  cargo:warning=      |                                                                      ^~~~~~~~~~~
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/storage/block_allocator.cpp:33:77: error: use of undeclared identifier 'MAP_ANONY
  cargo:warning=   33 |         const auto ptr = mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  cargo:warning=      |                                                                                    ^~~~~~~~~~~~~
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/storage/block_allocator.cpp:34:16: error: use of undeclared identifier 'MAP_FAILE
  cargo:warning=   34 |         return ptr == MAP_FAILED ? nullptr : data_ptr_cast(ptr);
  cargo:warning=      |                       ^~~~~~~~~~
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/storage/block_allocator.cpp:43:12: error: use of undeclared identifier 'munmap'
  cargo:warning=   43 |         success = munmap(pointer, size) == 0;
  cargo:warning=      |                   ^~~~~~
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-72840cb842f25d29/out/duckdb/src/storage/block_allocator.cpp:77:35: error: use of undeclared identifier 'MADV_DONT
  cargo:warning=   77 |         success = madvise(pointer, size, MADV_DONTNEED) == 0;
  cargo:warning=      |                                          ^~~~~~~~~~~~~
  cargo:warning=8 errors generated.
  exit status: 1
  cargo:warning=ToolExecError: command did not execute successfully (status code exit status: 1): "<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/bin/clang++" "-Os" "-ffunction-
  --- stderr
  error occurred in cc-rs: command did not execute successfully (status code exit status: 1): "<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/bin/clang++" "-Os" "-ffunction-sect
```

### 3. + emulation defines, no threads, no printing, wasm exceptions, shims (FAIL, one TU left)
Added: `-fwasm-exceptions -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_SIGNAL
-D_WASI_EMULATED_GETPID -DDUCKDB_NO_THREADS -DDUCKDB_DISABLE_PRINT -I shim -include shim/wasi-compat.h`
(`shim/wasi-compat.h` undefines `ERA` and defines `sched_getcpu`; `shim/net/if.h` is empty; `shim/ifaddrs.h` is a stub).
Everything compiles except `third_party/httplib/httplib.hpp` (`sockaddr_un` has no `sun_path`, no `getifaddrs`).
Log tail:
```
  cargo:warning= 4153 |     if (ifa->ifa_addr && ifn == ifa->ifa_name &&
  cargo:warning=      |                                    ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4147:10: note: forward declaration of 'ifaddrs'
  cargo:warning= 4147 |   struct ifaddrs *ifap;
  cargo:warning=      |          ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4155:13: error: member access into incomplete type 's
  cargo:warning= 4155 |          ifa->ifa_addr->sa_family == address_family)) {
  cargo:warning=      |             ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4147:10: note: forward declaration of 'ifaddrs'
  cargo:warning= 4147 |   struct ifaddrs *ifap;
  cargo:warning=      |          ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4156:14: error: member access into incomplete type 's
  cargo:warning= 4156 |       if (ifa->ifa_addr->sa_family == AF_INET) {
  cargo:warning=      |              ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4147:10: note: forward declaration of 'ifaddrs'
  cargo:warning= 4147 |   struct ifaddrs *ifap;
  cargo:warning=      |          ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4157:61: error: member access into incomplete type 's
  cargo:warning= 4157 |         auto sa = reinterpret_cast<struct sockaddr_in *>(ifa->ifa_addr);
  cargo:warning=      |                                                             ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4147:10: note: forward declaration of 'ifaddrs'
  cargo:warning= 4147 |   struct ifaddrs *ifap;
  cargo:warning=      |          ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4162:21: error: member access into incomplete type 's
  cargo:warning= 4162 |       } else if (ifa->ifa_addr->sa_family == AF_INET6) {
  cargo:warning=      |                     ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4147:10: note: forward declaration of 'ifaddrs'
  cargo:warning= 4147 |   struct ifaddrs *ifap;
  cargo:warning=      |          ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4163:62: error: member access into incomplete type 's
  cargo:warning= 4163 |         auto sa = reinterpret_cast<struct sockaddr_in6 *>(ifa->ifa_addr);
  cargo:warning=      |                                                              ^
  cargo:warning=<spike>/guests/duckdb/target/wasm32-wasip2/release/build/libduckdb-sys-da68a60bf2414cae/out/duckdb/third_party/httplib/httplib.hpp:4147:10: note: forward declaration of 'ifaddrs'
  cargo:warning= 4147 |   struct ifaddrs *ifap;
  cargo:warning=      |          ^
  cargo:warning=13 errors generated.
  exit status: 1
  cargo:warning=ToolExecError: command did not execute successfully (status code exit status: 1): "<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/bin/clang++" "-Os" "-ffunction-
  --- stderr
  error occurred in cc-rs: command did not execute successfully (status code exit status: 1): "<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/bin/clang++" "-Os" "-ffunction-sect
```

### 4. + shims for `sys/un.h` and `ifaddrs.h` (FAIL, link: libc++)
All C++ compiles. The link fails: `rust-lld: error: unable to find library -lc++`.
Log tail:
```
rustc: rustc 1.99.1 (58cf8f9c3 2026-10-13)
wasi-sdk clang: clang version 23.1.0-wasi-sdk (https://github.com/llvm/llvm-project 895aa2c896ada719451be2e3673c83da8ddf1141)
CFLAGS_wasm32_wasip2=--target=wasm32-wasip2 --sysroot=<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/share/wasi-sysroot -O2 -fwasm-exceptions -D_WASI_EMULATED_MMAN -D_WASI_EMULA
CXXFLAGS_wasm32_wasip2=--target=wasm32-wasip2 --sysroot=<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/share/wasi-sysroot -O2 -fwasm-exceptions -D_WASI_EMULATED_MMAN -D_WASI_EMU
   Compiling libduckdb-sys v1.10506.1
   Compiling duckdb-guest v0.0.0 (<spike>/guests/duckdb)
error: linking with `wasm-component-ld` failed: exit status: 1
  |
  = note:  "wasm-component-ld" "-flavor" "wasm" "--export" "__main_void" "--export" "cabi_realloc" "-z" "stack-size=1048576" "--stack-first" "--no-demangle" "<sysroot>/lib/rustlib/wasm32-wasip2/lib/se
  = note: some arguments are omitted. use `--verbose` to show all linker arguments
  = note: rust-lld: error: unable to find library -lc++
          error: failed to invoke LLD: exit status: 1
error: could not compile `duckdb-guest` (bin "duckdb-guest") due to 1 previous error
```

### 5. + `-L <sdk>/lib/wasm32-wasip2/eh -lc++abi -lunwind`, emulated-lib link args (FAIL, link: symbols)
Undefined symbols that wasi-libc lacks: `mlock`, `munlock`, `madvise`, `dlopen`, `dlsym`, `dlerror`.
Log tail:
```
rustc: rustc 1.99.1 (58cf8f9c3 2026-10-13)
wasi-sdk clang: clang version 23.1.0-wasi-sdk (https://github.com/llvm/llvm-project 895aa2c896ada719451be2e3673c83da8ddf1141)
CFLAGS_wasm32_wasip2=--target=wasm32-wasip2 --sysroot=<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/share/wasi-sysroot -O2 -fwasm-exceptions -D_WASI_EMULATED_MMAN -D_WASI_EMULA
CXXFLAGS_wasm32_wasip2=--target=wasm32-wasip2 --sysroot=<spike>/guests/duckdb/../../.tools/wasi-sdk/wasi-sdk-34.0-arm64-macos/share/wasi-sysroot -O2 -fwasm-exceptions -D_WASI_EMULATED_MMAN -D_WASI_EMU
   Compiling libduckdb-sys v1.10506.1
   Compiling duckdb-guest v0.0.0 (<spike>/guests/duckdb)
error: linking with `wasm-component-ld` failed: exit status: 1
  |
  = note:  "wasm-component-ld" "-flavor" "wasm" "--export" "__main_void" "--export" "cabi_realloc" "-z" "stack-size=1048576" "--stack-first" "--no-demangle" "<sysroot>/lib/rustlib/wasm32-wasip2/lib/se
  = note: some arguments are omitted. use `--verbose` to show all linker arguments
  = note: rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(8b3f8946b558ec1a-extension_load.o): undefined symbol: dlopen
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(8b3f8946b558ec1a-extension_load.o): undefined symbol: dlerror
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(8b3f8946b558ec1a-extension_load.o): undefined symbol: dlsym
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(8b3f8946b558ec1a-extension_load.o): undefined symbol: dlsym
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(759303d373ff2232-ub_src_common.o): undefined symbol: mlock
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(759303d373ff2232-ub_src_common.o): undefined symbol: munlock
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(759303d373ff2232-ub_src_common.o): undefined symbol: munlock
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(759303d373ff2232-ub_src_common.o): undefined symbol: munlock
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(759303d373ff2232-ub_src_common.o): undefined symbol: munlock
          rust-lld: error: <spike>/guests/duckdb/target/wasm32-wasip2/release/deps/liblibduckdb_sys-05a8056e6f71db44.rlib(759303d373ff2232-ub_src_storage.o): undefined symbol: madvise
          error: failed to invoke LLD: exit status: 1
error: could not compile `duckdb-guest` (bin "duckdb-guest") due to 1 previous error
```

### 6. + Rust link-time stubs for those six symbols (`src/stubs.rs`) (BUILD OK, RUN FAIL)
`cargo build --release --locked --target wasm32-wasip2` succeeds: `duckdb-guest.wasm` is a component of 29,679,089 bytes
(`opt-level = "s"`, `strip = "symbols"`). Output keys: `SPIKE guest_duckdb_build=ok`, `SPIKE guest_duckdb_wasm_bytes=29679089`.
Running it:
```
$ plugin-spike-host-cl run-command duckdb.wasm
SPIKE error=compile: failed to parse WebAssembly module
error: compile: failed to parse WebAssembly module
```
Cause, shown with `wasm-tools 1.262.0 validate`:
```
error: func 133 failed to validate
Caused by:
    0: legacy_exceptions feature required for try instruction (at offset 0x1d139)
```
`-fwasm-exceptions` in wasi-sdk 34 emits the legacy exception-handling encoding, and the `eh` variant of libc++,
libc++abi and libunwind in the sysroot is built the same way. wasmtime 50.0.0 supports only the final (exnref)
encoding: `Config::wasm_exceptions` is on by default with the `gc` feature, and the hidden
`Config::wasm_legacy_exceptions(true)` was tried in a throwaway host copy and answered
`the wasm_legacy_exceptions feature is not supported on this compiler configuration`.
Building without exceptions is not possible: DuckDB throws.

## Why NO-GO
- Criterion 2 fails: the only buildable configuration produces a module wasmtime 50 cannot load.
- Getting to GO would need an exnref-encoding libc++/libc++abi/libunwind (a custom wasi-sysroot build, or a future
  wasi-sdk) and `-mllvm -wasm-use-legacy-eh=false`; that is a toolchain project, not an adoption cost.
- Even when loadable, the build leans on six stubbed POSIX calls, force-included shim headers, no threads
  (`DUCKDB_NO_THREADS`), no extensions (no `dlopen`), no HTTP, and a 29.7 MB component.

## Other findings
- `cargo deny` (cargo-deny 0.20.2, root `deny.toml` via `--config`): advisories ok, bans ok, sources ok, **licenses
  FAILED**: `webpki-roots` 1.0.10 (`CDLA-Permissive-2.0`) via `libduckdb-sys` build-dependency `ureq` 3.5.0. The
  licence is not in the allow list. No ignore and no allow-list change was made; this affects every use of
  `libduckdb-sys` and is a decision for the owner (the DuckDB driver milestone, not this spike).
- Host finding: `run-command` could not pass `--allow-tcp` (see `guests/mssql/RESULT.md`); irrelevant to DuckDB.

## Files
`Cargo.toml`, `Cargo.lock`, `src/main.rs` (raw C API `select 1`), `src/stubs.rs`, `shim/**`, `build.sh`, `run.sh`.
