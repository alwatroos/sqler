# Plugin runtime spike (ADR-0054)

Throwaway spike, not product code. Own cargo workspace (host crate `plugin-spike-host`); each guest is a separate
workspace under `guests/` built for `wasm32-wasip2`, so a failing guest cannot break the host build. It answers: what
does embedding wasmtime cost (size, latency, memory, fuel vs epoch), and can DuckDB and SQL Server run as WASI guests?
Verdicts and decisions are in `docs/src/adr/0054-plugin-runtime-and-driver-feasibility.md`.

Final CI run: `39769856073` (commit `e7930f2`, branch `plugin-runtime-spike`), artifact `plugin-spike-summary`.

## How to reproduce

CI: `.github/workflows/plugin-spike.yml` (push under `spikes/plugin-runtime/**`; `workflow_dispatch` works once the
file is on `main`, input `linux_bytes` for the headroom columns). Note `spike.yml` (UI) also fires on `spikes/**`.

Local (needs a rustup toolchain with `wasm32-wasip2`; a throwaway rustup under `.tools/` works):

```sh
cd spikes/plugin-runtime
sh guests/hello/build.sh results/guests                      # hello.wasm (+ toolchain keys)
sh scripts/measure.sh cl results/guests results/cl           # variants: base cl cl-default pulley pulley-rt winch
sh guests/mssql/build.sh results/guests                      # mssql.wasm or mssql-stub.wasm
sh guests/mssql/run.sh results/cl/plugin-spike-host-cl results/guests results/cl   # needs docker
sh scripts/collect.sh results summary-out                    # merge results.txt files into summary.md
```

Host CLI: `info`, `run-fixture`, `bench`, `precompile [--pulley]`, `run-command <guest.wasm> [--env K=V] [--allow-tcp IP:PORT] [--dir HOST::GUEST]`.
Output is `SPIKE key=value` lines. Cleanup: `docker rm -f sqler-spike-mssql`, `cargo clean` per workspace, remove
`results/` and `.tools/`.

## Results (CI run 39769856073)

Sizes in bytes (release profile of the root workspace); delta against `base` of the same OS.

| variant | Linux | Linux delta | macOS | macOS delta | Windows | Windows delta |
| ------- | ----- | ----------- | ----- | ----------- | ------- | ------------- |
| base | 358,880 | 0 | 360,368 | 0 | 157,696 | 0 |
| cl | 14,883,440 | 14,524,560 | 10,798,192 | 10,437,824 | 16,126,976 | 15,969,280 |
| cl-default | 19,075,728 | 18,716,848 | 14,327,488 | 13,967,120 | 20,105,728 | 19,948,032 |
| pulley | 14,995,456 | 14,636,576 | 10,902,384 | 10,542,016 | 16,227,840 | 16,070,144 |
| pulley-rt | 3,959,968 | 3,601,088 | 3,527,296 | 3,166,928 | 4,515,840 | 4,358,144 |
| winch | 14,598,608 | 14,239,728 | 10,419,216 | 10,058,848 | 15,797,760 | 15,640,064 |

Linux headroom: 62,914,560 - 59,872,488 (`size.yml` run 39765240448) = 3,042,072 bytes. No variant fits with a 1 MiB
reserve; `pulley-rt` misses the budget by 559,016 bytes.

Latency, memory, fuel and epoch (hello fixture):

| OS | variant | compile ms | load cwasm ms | instantiate p50/p95 us | call p50/p95 ns | idle KiB/inst | spin none/fuel/epoch ms | fuel/epoch overhead % | interrupt ok |
| -- | ------- | ---------- | ------------- | ---------------------- | --------------- | ------------- | ----------------------- | --------------------- | ------------ |
| linux | cl | 9.177 | n/a | 8 / 9 | 60 / 61 | 0.3 | 18.675 / 18.580 / 18.554 | -0.5 / -0.7 | 1 / 1 |
| linux | cl-default | 7.481 | n/a | 8 / 9 | 421 / 470 | 8.0 | 21.814 / 21.824 / 21.813 | 0.0 / -0.0 | 1 / 1 |
| linux | pulley | 12.112 | n/a | 6 / 7 | 180 / 200 | 4.0 | 121.296 / 196.609 / 213.485 | 62.1 / 76.0 | 1 / 1 |
| linux | pulley-rt | n/a | 0.156 | 6 / 7 | 230 / 260 | 11.3 | n/a | n/a | n/a |
| linux | winch | 3.846 | n/a | 10 / 11 | 101 / 111 | 2.4 | 24.928 / 31.108 / 25.387 | 24.8 / 1.8 | 1 / 1 |
| macos | cl | 14.002 | n/a | 10 / 34 | 83 / 208 | 21.4 | 25.570 / 23.923 / 25.915 | -6.4 / 1.3 | 1 / 1 |
| macos | cl-default | 4.647 | n/a | 8 / 12 | 334 / 459 | 23.2 | 19.819 / 19.768 / 20.764 | -0.3 / 4.8 | 1 / 1 |
| macos | pulley | 10.412 | n/a | 12 / 22 | 167 / 208 | 88.2 | 258.178 / 501.021 / 579.141 | 94.1 / 124.3 | 1 / 1 |
| macos | pulley-rt | n/a | 0.247 | 32 / 111 | 167 / 167 | 88.5 | n/a | n/a | n/a |
| macos | winch | 2.905 | n/a | 13 / 22 | 83 / 84 | 21.6 | 28.635 / 50.630 / 30.920 | 76.8 / 8.0 | 1 / 1 |
| windows | cl | 26.481 | n/a | 10 / 21 | 100 / 200 | 10.4 | 25.246 / 28.118 / 25.040 | 11.4 / -0.8 | 1 / 1 |
| windows | cl-default | 19.834 | n/a | 20 / 29 | 700 / 1400 | 11.6 | 25.066 / 27.491 / 28.196 | 9.7 / 12.5 | 1 / 1 |
| windows | pulley | 16.556 | n/a | 22 / 29 | 200 / 200 | 79.3 | 115.029 / 187.524 / 201.714 | 63.0 / 75.4 | 1 / 1 |
| windows | pulley-rt | n/a | 0.446 | 42 / 71 | 400 / 500 | 80.6 | n/a | n/a | n/a |
| windows | winch | 5.509 | n/a | 9 / 14 | 100 / 100 | 10.5 | 19.553 / 19.752 / 18.967 | 1.0 / -3.0 | 1 / 1 |

Guests and toolchain: rustc 1.99.1, wit-bindgen 0.63.0 (`hello.wasm` 13,475 bytes), cargo-component 0.21.1
(`hello-cc.wasm` 12,658 bytes, generated bindings from wit-bindgen 0.41.0). DuckDB: build ok (29,678,977 bytes),
`select1 = fail` on all three OSes (`failed to parse WebAssembly module`, legacy exceptions). SQL Server: plain build
`fail` (`rustls-native-certs`), stub build ok (2,124,140 bytes), `select1 = 1`, `tls = verified`, `denied_ok = 1`
on the Linux CI job (compile 2,178 ms, run 13.6 ms, RSS 74,156 KiB).

## Quirks

- macOS timer granularity is about 41.7 ns, Windows 100 ns: small call figures are quantised. The Linux `cl` idle
  0.3 KiB/instance is probably an RSS artifact (page reuse).
- Fuel must be set on the Store before instantiation. `pulley-rt` cannot bench fuel/epoch: a `.cwasm` is bound to its
  engine configuration.
- Winch fuel/epoch percentages are noisy (plain code was slower than epoch code in some runs).
- `cargo-component` builds `wasm32-wasip1` and adapts; do not `cargo fmt --check` `guests/hello-cc` after a build
  (generated `src/bindings.rs`, git-ignored).
- `cargo deny`: global options (`--config`) go before the subcommand. `guests/duckdb` skips only the licences check in
  CI (`webpki-roots`, `CDLA-Permissive-2.0`).
- `run-command` needed two fixes found by the SQL Server guest: `allow_tcp(true)` and an allowed implicit `TcpBind`
  to `0.0.0.0:0` (connect stays limited to `--allow-tcp`).
- The SQL Server image is about 1.5 GB (emulated on Apple Silicon); `start-mssql.sh` waits at most 180 s.
- Raw `SPIKE` lines: artifacts `plugin-spike-<os>-<variant>`, `plugin-spike-guests`, `plugin-spike-mssql`, retention
  30 days.
