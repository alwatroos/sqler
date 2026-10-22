# Plugin runtime spike summary

## Binary size (release profile, delta = variant - base on the same OS)

Linux shipped binary (size.yml): 50000000 bytes; headroom under 62914560: 12914560 bytes.

| OS | variant | bytes | delta bytes | delta KiB | Linux headroom after delta (bytes) |
| -- | ------- | ----- | ----------- | --------- | ---------------------------------- |
| linux | base | 360000 | 0 | 0.0 | 12914560 |
| linux | cl | 10800000 | 10440000 | 10195.3 | 2474560 |
| macos | cl | 10798496 | n/a | n/a | n/a |

## Latency, memory, fuel and epoch (hello fixture)

| OS | variant | compile ms | load cwasm ms | instantiate p50/p95 us | call p50/p95 ns | idle KiB/instance | spin none/fuel/epoch ms | fuel/epoch overhead % | interrupt ok fuel/epoch |
| -- | ------- | ---------- | ------------- | --------------------- | --------------- | ----------------- | ----------------------- | --------------------- | ----------------------- |
| linux | cl | 8.76 | n/a | 8 / 12 | 42 / 83 | 21.4 | 19.47 / 22.12 / 24.94 | 13.6 / 28.1 | 1 / 1 |
| macos | cl | 8.76 | n/a | 8 / 12 | 42 / 83 | n/a | n/a / n/a / n/a | n/a / n/a | n/a / n/a |

## Guests and toolchain

| OS | key | value |
| -- | --- | ----- |
| linux | guest_duckdb_build | ok |
| linux | guest_duckdb_select1 | 1 |
| linux | guest_mssql_build | fail |
| linux | hello_cc_build | ok |
| linux | hello_wasm_bytes | 1000 |
| linux | toolchain_rustc | 1.99.1 |
| linux | toolchain_wit_bindgen | 0.63.0 |
