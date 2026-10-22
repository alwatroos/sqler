# Benchmarks

Targets for every release build:

| Metric | Target | Source |
| ------ | ------ | ------ |
| Cold start | < 500 ms | prompt §2.2 / AGENTS.md §1 |
| Idle RSS with three connections open | < 150 MB | prompt §2.2 / AGENTS.md §1 |
| Release binary size | < 60 MB per platform | prompt §2.2 / AGENTS.md §1 |

These are tracked numbers with dates and commit hashes, not aspirations.

## Results

| Date | Commit | OS | Cold start (ms) | Idle RSS, 3 connections (MB) | Binary size (MB) | Notes |
| ---- | ------ | -- | --------------- | ---------------------------- | ---------------- | ----- |
| 2026-10-20 | 6fc6229 | macOS 27.0.1 arm64 | 2.4 | n/a until M1 | 3.0 | `sqler-cli` release, default features; cold start = `run :memory: "select 1"` mean of 20 runs via Python `perf_counter`; CLI peak RSS 3.6 MB |
| 2026-10-20 | 1f206e6 | macOS 27.0.1 arm64, Apple M4 Pro | 140.6 | 98.1 (3 connections configured, none opened; see notes) | 29.5 | `sqler` desktop release, default features, `--locked`; cold start = hyperfine mean of 20 runs after 3 warm-ups of `--exit-after-first-frame` (σ 12.8, min 132.7, max 191.1); idle RSS = `ps` after 6 s and 10 s, three runs, 98.0 to 98.3 MiB |
| 2026-10-20 | ba24c9a | macOS 27.0.1 arm64, Apple M4 Pro | 155.1 | 100.0 (3 connections configured: `SQLite`, `PostgreSQL`, `MySQL`; none opened; see notes) | 38.3 | `sqler` desktop release, default features (both server drivers), `--locked`; 40,168,784 bytes; cold start = hyperfine mean of 20 runs after 3 warm-ups of `--exit-after-first-frame` (σ 11.0, min 138.3, max 167.2); idle RSS = `ps` at 6 s and 10 s, three runs, 100.0 to 100.2 MiB; 814 tests run, 814 passed (with `PostgreSQL` 18, `MySQL` 8.4 in Docker; `MariaDB` 11.8 run for `sqler-driver-mysql`: 26 passed) |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 113.7 | 72.0 (3 connections configured: `SQLite`, `PostgreSQL`, `MySQL`; none opened; see notes) | 42.3 | `sqler` desktop release, default features, `--locked`; 44,319,056 bytes; cold start = hyperfine mean of 20 runs after 3 warm-ups of `--exit-after-first-frame` (σ 3.2, min 109.7, max 117.7); idle RSS = `ps` at 6 s and 10 s, three runs, 71.9 to 72.1 MiB (lower than M2 with no code change that explains it, see the M3 note); 1595 tests run, 1595 passed, 2 skipped (with `PostgreSQL` 18, `MySQL` 8.4 and the `start-sshd.sh` container in Docker; `MariaDB` 11.8 run for `sqler-driver-mysql`: 35 passed); coverage not measured (`cargo llvm-cov` not installed) |
| 2026-10-21 | ec3057c | macOS 27.0.1 arm64, Apple M4 Pro | 133.7 | 72.4 (3 `SQLite` sessions open with `--open-sessions`, `3 of 3 open`; median of the six 6 s readings, range 72.1 to 101.4; see notes) | 30.5 | `sqler` desktop release, default features, `--locked`; 31,939,312 bytes; cold start = hyperfine mean of 20 runs after 3 warm-ups of `--exit-after-first-frame` (σ 3.0, min 130.0, max 144.4); idle RSS with the M3 mix configured and not opened (`SQLite`, `PostgreSQL`, `MySQL`) = `ps` at 6 s and 10 s, three runs, 71.0 to 71.1 MiB; `Binary size` workflow run 39572444831 at `ec3057c`: Linux 55,798,352 bytes (53.2 MiB), macOS 31,922,960 bytes (30.4 MiB), Windows 40,875,520 bytes (39.0 MiB), all under 62,914,560; 2237 tests run, 2237 passed, 2 skipped (with `PostgreSQL` 18, `MySQL` 8.4 and the `start-sshd.sh` container in Docker; `MariaDB` 11.8 run for `sqler-driver-mysql`: 38 passed); coverage not measured (`cargo llvm-cov` not installed) |
| 2026-10-21 | 248e740 | macOS 27.0.1 arm64, Apple M4 Pro | 136.8 (same-session `1ea7c39` build: 134.1; see the M4.5 note) | 103.0 (3 `SQLite` sessions open with `--open-sessions`, `3 of 3 open`; median of six 6 s readings, same-session `1ea7c39` build: 101.2; see notes) | 31.3 | `sqler` desktop release, default features, `--locked`; 32,845,424 bytes locally; cold start = interleaved hyperfine, three rounds of 20 runs after 3 warm-ups of `--exit-after-first-frame` (means 136.0, 137.5, 136.9, sigma 4.0 to 5.2) against the `1ea7c39` build (133.5 to 134.8); idle RSS with the M3 mix configured and not opened = 101.8 (median of three; `1ea7c39` build 99.8); `Binary size` workflow run 39637124766 at `248e740`: Linux 56,764,632 bytes (54.1 MiB), macOS 32,829,024 bytes (31.3 MiB), Windows 42,270,720 bytes (40.3 MiB), all under 62,914,560 but +943.6, +884.8 and +1362.5 KiB over M4 (budget +300 KiB: missed); 2465 tests run, 2465 passed, 3 skipped (with `PostgreSQL` 18, `MySQL` 8.4 and the `start-sshd.sh` container in Docker; `MariaDB` 11.8 run for `sqler-driver-mysql`: 38 passed); coverage not measured (`cargo llvm-cov` not installed) |
| 2026-10-22 | 6ed581c | macOS 27.0.1 arm64, Apple M4 Pro | 139.3 (three runs: 136.9, 143.8, 139.3; no same-session older build; see the M5 note) | 104.0 (3 `SQLite` sessions open with `--open-sessions`, `3 of 3 open`; median of five 6 s readings, 103.7 to 104.4; see notes) | 36.1 | `sqler` desktop release, default features, `--locked`; 37,832,304 bytes locally; cold start = hyperfine, 20 runs after 3 warm-ups of `--exit-after-first-frame` in a scratch `HOME` with three `SQLite` `:memory:` entries (means 136.9, 143.8 and 139.3 ms, sigma 11.8 to 13.5, min 124.7, max 175.4); idle RSS with the entries configured and not opened = `ps` at 6 s and 10 s, three runs, 102.5 to 103.6 MiB; with three sessions open = three runs at 6 s and 10 s (103.6 to 104.4 MiB) plus two more at 6 s (104.1, 104.2); `Binary size` workflow run 39780918427 (PR #82 head `f70248e`, the last code change of M5; the merged `6ed581c` adds only the squash): Linux 62,512,360 bytes (59.6 MiB), macOS 37,798,480 bytes (36.0 MiB), Windows 49,543,680 bytes (47.2 MiB), against 62,914,560: Linux has 402,200 bytes (392.8 KiB) of headroom; +5,747,728 (Linux), +4,969,456 (macOS) and +7,272,960 bytes (Windows) over the M4.5 close `248e740`; 3385 tests run, 3385 passed, 5 skipped (`cargo nextest run --workspace --no-tests=pass`, no database servers: the DB-backed tests skip green here and run in the CI `db` job); coverage not measured (`cargo llvm-cov` not installed) |

## Method

Run from the repository root on the commit being recorded:

1. `cargo build --release -p sqler-cli`
2. Binary size: `stat -f %z target/release/sqler-cli` (bytes), MB = bytes / 1048576.
3. Cold start: 3 warm-ups, then 20 runs of `target/release/sqler-cli run :memory: "select 1"`; report the mean. Use `hyperfine` when installed; otherwise time each run with a wall clock (`/usr/bin/time -p` is too coarse, it has 10 ms resolution).
4. CLI peak RSS: `/usr/bin/time -l target/release/sqler-cli run :memory: "select 1"`, "maximum resident set size" (bytes on macOS).
5. Test count: `cargo nextest run --workspace --no-tests=pass`.

### Desktop app (`sqler`)

Run from the repository root on the commit being recorded:

1. `cargo build --release -p sqler-app --locked`
2. Binary size: `stat -f %z target/release/sqler` (bytes), MB = bytes / 1048576 (30,928,816 bytes = 29.5 MB at `1f206e6`). Per OS in CI: the `Binary size` workflow (`size.yml`) prints the bytes for Linux, macOS and Windows next to the 60 MB budget (report only, not a gate).
3. Isolate the config: on macOS set `HOME` to an empty directory that holds `Library/Application Support/SQLer/connections.toml` with three `sqlite` `:memory:` entries (on Linux set `XDG_CONFIG_HOME`).
4. Cold start: `hyperfine -w 3 -r 20 'target/release/sqler --exit-after-first-frame'`. This is the whole process until the first frame is presented, so it needs a display. Without one, record "not measured: no interactive display".
5. Idle RSS: start `target/release/sqler`, wait 6 s, read `ps -o rss= -p <pid>` (KiB), read again after 10 s, stop it. Repeat three times.
6. Idle RSS, three sessions open: put three `sqlite` entries without keychain secrets (or reachable `PostgreSQL`/`MySQL` test containers) in the isolated config, start `target/release/sqler --open-sessions 2> open.log`, wait for the `sqler: open-sessions: 3 of 3 open, idling` line in `open.log`, then read `ps -o rss= -p <pid>` at 6 s and 10 s as in step 5. A `skipped` line means that connection is not open (keychain secret, unknown or changed SSH host key, connect failure); the figure only counts as "three open" when the summary says `3 of 3`. On Windows the release binary has no console, so stderr is visible only when redirected.
7. Test count: `cargo nextest run --workspace --no-tests=pass`.

The figures for these two methods (three sessions open, Linux and Windows sizes) are in the M4 row of the table above and in the M4 note below.

#### Linux binary size (T14)

`size.yml` on `main` reported `sqler` at 83,477,248 bytes on Linux against 44.3 MB on macOS and 43.0 MB on Windows (budget 62,914,560 bytes). Cause: the Linux release binary keeps the ELF symbol table (`.symtab` 8.6 MB plus `.strtab` 14.8 MB on aarch64, 23 MB of 78 MB), which macOS and Windows do not put in the executable, and 16 codegen units. Section table of the unstripped aarch64 build (`size -A`): `.text` 36.5 MB, `.rodata` 6.2 MB, `.eh_frame` 4.8 MB, `.rela.dyn` 2.4 MB, `.gcc_except_table` 2.1 MB; `.debug_*` is absent from the default build. Largest crates in `.text` (`cargo bloat --crates`): `zbus` 4.1 MiB (Secret Service and AccessKit D-Bus code), `std` 2.6, `gpui` 2.4, `gpui_base` 2.2, `gpui_component` 1.6, `gpui_linux` 1.4, `naga` 1.3, `sqler_ui` 1.1, `wgpu_core` 0.8, `accesskit_unix` 0.7 MiB. Both TLS stacks (`aws_lc_sys` 0.4 MiB, `rustls`/`ring`), bundled `SQLite` and the tree-sitter grammars are small next to these, so none of the suspected dependency duplications is the cause.

Measured in Docker (`rust:slim-bookworm` plus the `ci.yml` apt packages, toolchain 1.99.1, `cargo build --release -p sqler-app --locked`, 14 CPUs, 7.7 GiB). aarch64 ran natively, x86_64 (the CI architecture) under emulation, so the x86_64 build times are not comparable to CI:

| Profile | aarch64 bytes | aarch64 build | x86_64 bytes | Verdict |
| ------- | ------------- | ------------- | ------------ | ------- |
| default (before) | 78,221,512 | 81 s | 90,666,088 (`strip = "none"`) | over budget |
| `strip = "symbols"` | 54,884,096 | 75 s | 62,291,760 (`strip` of the binary) | x86_64 only 0.6 MB under the budget |
| + `lto = "thin"` | 56,587,976 | 70 s | not run | bigger, rejected |
| + `lto = "fat"` (16 or 1 codegen units) | not built | killed (SIGKILL, out of memory) after 291 s and 459 s | not run | rejected |
| `strip = "symbols"` + `codegen-units = 1` (applied) | 46,168,064 | 132 s | 53,611,336 | 51.1 MiB, under the budget |
| `strip` + `panic = "abort"` | 45,840,136 | 131 s | not run | rejected: a panic in a task would end the app |
| `strip` + `opt-level = "s"` for everything | 42,956,672 | 84 s | not run | rejected: slower grid and editor code for 3 MB more |

The applied profile is `strip = "symbols"` plus `codegen-units = 1` in `[profile.release]`. The CI Linux figure after the change is printed by `size.yml`; the x86_64 Docker figure of 53.6 MB leaves about 9 MB of headroom (the CI figure was 83.5 MB where Docker built 90.7 MB unstripped, so the CI Linux build is expected to be somewhat smaller than Docker's; unconfirmed until `size.yml` runs on this change). macOS and Windows are unchanged in cause but pick up `codegen-units = 1`; their new figures come from `size.yml` after merge. The release build is about 1.7x slower (aarch64 75 s to 132 s). `cargo bench` inherits `release`, so benchmarks run with the same profile.

Next reductions if the budget tightens: `zbus` and `accesskit_unix` (4.8 MiB of `.text`), the `wgpu`/`naga` backends, and `opt-level = "s"` for non-hot dependencies. Not applied: no measured need.


M2 note (ba24c9a): the binary grew from 29.5 to 38.3 MiB with the `PostgreSQL` and `MySQL` drivers, the tunnel and the keychain; the cold start rose from 140.6 to 155.1 ms and the idle RSS from 98.1 to 100.0 MiB. The three connections of the M2 row were a `SQLite` `:memory:`, a `PostgreSQL` and a `MySQL` entry, with both containers running. The app has no headless way to open a session (a session opens by double-click or from the connection manager, and `--exit-after-first-frame` exits before any), so the RSS with three sessions actually open was not measured. The figure that was measured is the idle app with three saved connections; the sessions, the tunnel and the catalog cache add to it.

M3 note (e98f4c8): measured fresh on the merged M3 `main` with the same method and `HOME` setup. The binary grew from 38.3 to 42.3 MiB (+4.0 MiB, 44,319,056 bytes) with the SQL text services, the editor, completion and the RSA backends; it stays under the 60 MB target on macOS. The cold start fell from 155.1 to 113.7 ms and the idle RSS from 100.0 to 72.0 MiB (71.9 to 72.1 over three runs). Nothing merged in M3 should make the app faster or lighter at idle, so read these two as the state of the machine and the display during the run, not as an improvement: the figures are measured and unexplained, and the maintainer should repeat them on a normal desktop session. The three connections are the M2 ones (`SQLite` `:memory:`, `PostgreSQL`, `MySQL`), configured and not opened, for the same reason as in M2. The test count rose from 814 to 1595 run (1595 passed, 2 skipped). **Editor keystroke latency** is defined here as the headless work per keystroke: the `editor_intellisense` worst p95, plus `bracket_build_p95_ns` of `editor_buffer` when the text changes next to a bracket. At `e98f4c8` the worst of three runs gives 282,042 ns + 1,349,541 ns = 1.63 ms (the bracket build was 0.49 to 0.51 ms in the other two runs, so 0.74 to 0.79 ms), against the 16 ms frame budget. The input-to-paint latency of the widget (key press to the pixels of the popup, typing and scrolling speed on the 10,000-line script) is not measured: it needs a probe in a real window and is a maintainer check. Coverage is not measured because `cargo llvm-cov` is not installed. Linux and Windows desktop numbers are not measured (no GUI runner in `ci.yml`).

M4 note (ec3057c): measured fresh on the merged M4 `main` with the same method, `HOME` setup and Docker containers as for M3 (T1 to T12, T14 and T15 are merged). **Size.** The macOS binary fell from 42.3 to 30.5 MiB (31,939,312 bytes, 12,379,744 bytes less), although M4 added the export writers, the grid features and inline editing. The most likely cause is T14 (`strip = "symbols"` and `codegen-units = 1` in `[profile.release]`, PR #44), which applies to every OS; no before and after build on macOS isolates it, so the split between T14 and the M4 code is not measured. `size.yml` (run 39572444831, `ec3057c`) reports Linux 55,798,352 bytes (53.2 MiB, 7,116,208 bytes under the 62,914,560 budget, down from 83,477,248 before T14), macOS 31,922,960 bytes (30.4 MiB) and Windows 40,875,520 bytes (39.0 MiB); all three are under the 60 MB target, Linux with the least headroom. **Cold start.** 133.7 ms (σ 3.0 ms, min 130.0, max 144.4) against 113.7 ms at `e98f4c8`, +20.0 ms. M2 measured 155.1 ms, and the M3 figure was already labelled unexplained; no specific M4 change is known to add startup work, so the +20 ms is unexplained, not a verified regression. It is far inside the 500 ms target. **Idle RSS.** With the M3 mix configured and not opened: 71.0 to 71.1 MiB (M3 71.9 to 72.1). With three `SQLite` `:memory:` sessions really open (`sqler --open-sessions`, PR #40; every run printed `3 of 3 open, idling`): six runs gave 72.1 to 72.5 MiB in four of them and 100.9 to 101.4 MiB in the other two (the first two runs); the target is 150 MB. The 29 MiB gap has no known cause (same binary and config, not investigated), so the figure is reported as 72.4 MiB (median of the six 6 s readings) with the range 72.1 to 101.4 MiB. The M3 mix (`SQLite`, `PostgreSQL`, `MySQL`) opened only 1 of 3 (`skipped ... the database connection failed` for the two server entries, because the test config carries no password; keychain-backed entries are skipped by design), so it is not a "three open" figure; its RSS was 72.6 to 74.0 MiB. **Not measured.** Coverage (`cargo llvm-cov` is not installed), Linux and Windows desktop cold start and RSS (no GUI runner), and the scroll and paint feel of the M4 grid features in a real window (maintainer checks, listed in AGENTS.md section 9). The test count rose from 1595 to 2237 run (2237 passed, 2 skipped) and the `MariaDB` run of `sqler-driver-mysql` from 35 to 38.

M4.5 note (248e740): measured fresh on the merged M4.5 `main` (D1, U2, U6, U1, U3, U5 and U4 are merged, plus the M5 items T1 and T2) with the same method and `HOME` setup as for M4, and against a second release build of `1ea7c39` measured in the same session, interleaved, because the machine shows two states. **Size.** The macOS binary grew from 31,939,312 to 32,845,424 bytes locally (+884.9 KiB), and `size.yml` (run 39637124766) reports Linux 56,764,632 (54.1 MiB, +943.6 KiB, 5.87 MiB under the 62,914,560 budget), macOS 32,829,024 (31.3 MiB, +884.8 KiB) and Windows 42,270,720 (40.3 MiB, +1362.5 KiB) against `1ea7c39`. The M4.5 budget was +300 KiB in total: **missed on all three OSes**, by about 3x on macOS and Linux and 4.5x on Windows. By step on macOS (`size.yml` per `main` commit): U2 +65.4 KiB, U1 +328.7 KiB, U3 and U5 together +408.5 KiB, U4 +82.0 KiB, the others about 0. The team accepted the U1, U3, U5 and U4 growth in their PRs; the U2 growth and the total were not. Windows grows the most; the cause was not isolated. **Cold start.** Interleaved against the `1ea7c39` build the new binary is 136.8 ms against 134.1 ms (three rounds, +2.7 ms; budget +5 ms: met). The same new binary measured 114.9 ms (sigma 1.2) in the first run of the session and 137.0 ms (sigma 12.8) minutes later, and the old binary 133.5 to 134.8 ms: the machine has a fast state (about 115 ms) and a slow state (about 135 ms), so the M4 report's +20 ms (M3 113.7 ms, M4 133.7 ms) is consistent with machine state. M3 and M4 builds were not compared in one session, so a code share in that +20 ms is not excluded, and the M4 cold start rise stays open. What flips the machine was not investigated. **Idle RSS.** With three `SQLite` sessions open (every run `3 of 3 open`): 102.98 MiB (median of six, 102.8 to 103.0; an earlier batch on the new binary alone read 102.7 to 103.6) against 101.16 MiB for the `1ea7c39` build, +1.8 MiB (budget +5 MiB: met; target 150 MB: met). Configured mix: 101.8 against 99.8 MiB (median of three). The low state seen in M4 (about 72 MiB) appeared once, in the first configured run of the session (73.2 MiB), and all later runs of both binaries sat at about 100 to 105 MiB: the "29 MiB gap" of M4 is consistent with the same machine state, on one binary; neither old binary was seen in the low state this session, so this is not shown for the M4 build, and the RSS spread stays open. **Workbench read** (`load_timing`, 1000 loads): p50 16 to 33 us, max 50 to 290 us (budget 1 ms: met). **Theme parse** (`cargo nextest run -p sqler-ui --release --features screenshots --lib parsing_is_fast`, six runs, no code change): wall time 12, 8, 9, 8, 8 and 8 ms for 20 parses plus the test process start, so each parse takes at most 0.6 ms (typically at most 0.4 ms); an upper bound, not a per-parse timing (budget 1 ms: met on the bound). **Benches.** `grid_viewport` p95 125 ns and heap 113,900,996 bytes (both identical to M4), `grid_sort` sort p95 70.9 to 71.9 ms (M4 70.9 to 71.4), `browser_tree` expand p95 681,375 ns (M4 616,417; one local run, +10.5%, Bench workflow green on all M4.5 PRs, not re-run), completion worst p95 2.49 to 2.52 ms (M4 2.37 to 2.43). No M4.5 item changed the grid model, viewport, order or completion code. `sort_multi_p95_ns` (+1 to +2 %) and the completion `diagnose` and `worst` p95 (+3 to +6 %) were above every M4 run in all three re-runs; the cause is not established and the gates are green. **Tests.** 2237 run at M4, 2465 run now (2465 passed, 3 skipped: the two ignored keychain and bastion tests and the new `load_timing` probe), `MariaDB` 38 passed. **Not measured.** Coverage (`cargo llvm-cov` not installed), Linux and Windows desktop cold start and RSS (no GUI runner), input-to-paint latency of the new shell, and the interactive checks in the PRs' "Pending (maintainer)" lists. Raw figures: `docs/reports/iteration-005b.md`.

M5 note (6ed581c): measured fresh on the merged M5 `main` (all of T1 to T17, T19 and the fix PR #78 are merged) with the same method and `HOME` setup as for M4.5. **Size.** The macOS binary is 37,832,304 bytes locally (36.08 MiB), +4,986,880 bytes (+4.76 MiB) over the 32,845,424 of `248e740`. `size.yml` at PR #82 (run 39780918427, `f70248e`) reports Linux 62,512,360 (59.6 MiB), macOS 37,798,480 (36.0 MiB) and Windows 49,543,680 (47.2 MiB) bytes, +5.48, +4.74 and +6.94 MiB over the M4.5 close (56,764,632, 32,829,024 and 42,270,720). The Linux binary is **402,200 bytes (392.8 KiB) under the 62,914,560 budget**; macOS and Windows have 24 and 12.7 MiB of room. M5 has no size budget of its own in the plan, so nothing is "missed", but the Linux headroom is the main risk for M6 and M7 (a wasmtime plugin host does not fit by default, ADR-0054). Steps measured by `size.yml` on Linux: PR #70 (AI settings, activity item, side view) 58,515,816 (T11 added about 1.75 MB; the reading of the run before it is not quoted here), PR #80 (editor AI actions) 59,608,040, PR #81 (AI session tab) 59,872,488 (+264,448 over PR #80), PR #82 (Agent tab and the `sqler-acp` and `sqler-mcp` host code in the app) 62,512,360 (+2,639,872 = +2.52 MiB, the largest single step). PR #76 added `sqler-cli mcp` (+1.31 MiB for `sqler-cli`, not for `sqler`). The steps between are not listed because only some `main` runs were read; the split of the rest (providers, redaction, assist engine, audit viewer, ACP client) is not measured. **Cold start.** 136.9, 143.8 and 139.3 ms (means of 20 runs; sigma 11.8 to 13.5 ms; the first frame peaks at 70.5 MiB RSS per hyperfine). The M4.5 binary was not rebuilt and interleaved in this session, so a before/after delta is **not** established: the figures sit at the slow state of the machine (about 135 ms) and about 3 to 7 ms above the M4.5 figure of 136.8 ms, which is within the sigma. Target 500 ms: met. **Idle RSS.** With three `SQLite` sessions open (every run `3 of 3 open`): 103.7, 103.9, 104.4 (first three runs) and 104.1, 104.2 MiB (two more), median 104.05 MiB; with the entries configured and not opened 102.6, 103.3 and 103.6 MiB (6 s). The machine was in its high memory state on every run; the low state of about 72 MiB seen in M3 and M4 did not appear. M4.5 measured 103.0 MiB open and 101.8 MiB configured in the same high state, so the M5 delta is about +1.0 to +1.8 MiB, from a comparison across sessions, not interleaved. Target 150 MB: met. **Tests.** 3385 run, 3385 passed, 5 skipped (2465 at M4.5, +920), run without database servers. **Theme parse.** `cargo nextest run -p sqler-ui --release --features screenshots --lib parsing_is_fast`, six runs: 8 ms wall time each (20 parses plus the test process start), so each parse takes at most 0.4 ms; an upper bound, not a per-parse timing (budget 1 ms: met on the bound). **Benches** (`cargo bench -p sqler-ui --bench <name>`, one run each, exit 0): `grid_viewport` p95 125 ns and heap 113,900,996 bytes (identical to M4.5), `browser_tree` expand p50 / p95 621,583 / 634,042 ns (M4.5 p95 681,375), `editor_buffer` bracket build p95 1,311,791 ns and format p95 10,653,042 ns (budget 16 ms), `editor_intellisense` worst p95 276,166 ns (`from_prefix`; budget 16 ms). All within the thresholds. **Coverage.** Not measured: `cargo llvm-cov` is not installed on this machine. **Still open.** Linux and Windows desktop cold start and idle RSS (no GUI runner; the `screenshots` jobs only render scenes), and input-to-paint latency of the shell and of the new AI views (no probe). Raw figures: `docs/reports/iteration-006.md`.

Caveat for idle RSS (M2 and M3 rows): an agent cannot double-click, so the three connections are listed in the sidebar but their sessions were not opened. Three `SQLite` `:memory:` sessions add little, but the figure is not the full "three connections open" metric until the maintainer repeats it with all three opened. The M1 rows are macOS only; Linux and Windows desktop numbers are not measured because `ci.yml` has no GUI runner. The spike table below has CI cold-start and idle RSS for those OSes on the spike build, not on `sqler`.

## Results grid model

Viewport fetch from `sqler_ui::grid::GridModel` holding 1M synthetic rows (6 columns: integer, text, float, boolean, integer, text). Each sample (`p50_ns`, `p95_ns`) is one `viewport()` call plus the construction of a `RowRef` for each of the 50 visible rows; it reads no cell and no per-row memory. A second measurement reads every cell of those rows (`cells_p50_ns`, `cells_p95_ns`) and reads the payload of each cell: the value of an inline cell, and the length plus the first and last byte of a text, JSON or binary cell, so it touches the arenas (an earlier version of the bench only resolved the span of each cell, which does not read the arena); it is gated once the baseline has the key (see the regression rule).

| Date | Commit | OS | Rows | Viewport | p50 (ns) | p95 (ns) | Ingest (ms) | Heap estimate (MB) | RSS delta (MB) | Notes |
| ---- | ------ | -- | ---- | -------- | -------- | -------- | ----------- | ------------------ | -------------- | ----- |
| 2026-10-20 | 1dfec02 plus this PR | macOS 27.0.1 arm64 | 1,000,000 | 50 | 208 | 334 | 133 | 235.0 | n/a (Linux only) | `cargo bench -p sqler-ui --bench grid_viewport`, 100,000 samples, batch 1024; peak RSS of the bench process 242.7 MiB (`/usr/bin/time -l`, includes the runtime and the ingestion thread) |

Re-run at `e98f4c8` (macOS 27.0.1 arm64, Apple M4 Pro): unchanged within the gate (p50 125 ns, p95 167 ns, ingest 153 ms, heap estimate 246,433,024 bytes = 235.0 MiB; the p95 limit of the gate is 25 % plus 100 ns).

Re-run at `ec3057c` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench grid_viewport -- --staged 10000`, three runs, exit 0, load average 2.5 to 4.0): `p50_ns` 83 and `p95_ns` 125 in all three runs (167 at `e98f4c8`; budget 1 ms), `cells_p95_ns` 959, 958 and 1,000 (917 at `d1c6c64`), `ingest_ms` 201, 163 and 162 (153), `layout_p95_ns` 15,417, 15,875 and 15,750 (14,792 at `d1c6c64`; not gated), `staged_p95_ns` 23,500, 24,125 and 23,542 with `staged_stage_ms` 7 (not gated; the 1 ms budget is on `staged_p95_ns`). The heap estimate is 113,900,996 bytes = 108.6 MiB (235.0 MiB at `e98f4c8`): the bounded compact model of PR #42 (ADR-0027) holds the 1M rows in less than half the memory. Peak RSS of the bench process (`/usr/bin/time -l`): 229,228,544 bytes maximum resident set size (218.6 MiB) and 225,116,808 bytes peak memory footprint (214.7 MiB), down from 242.7 MiB.

Re-run at `248e740` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench grid_viewport -- --staged 10000`, three runs, exit 0): `p50_ns` 83 and `p95_ns` 125 in all three runs (unchanged from `ec3057c`; budget 1 ms), `cells_p95_ns` 958, 959 and 1,333, `ingest_ms` 171, 173 and 164, `heap_bytes` 113,900,996 (identical), `layout_p95_ns` 15,042, 19,000 and 15,125, `staged_stage_ms` 7, 6 and 6, `staged_p95_ns` 24,875, 25,250 and 24,917 (23,500 to 24,125 at `ec3057c`, +5 to +7 %, about 1 us); the bench binary under `/usr/bin/time -l`: 229,638,144 bytes maximum resident set size, 225,526,408 bytes peak memory footprint (229,228,544 and 225,116,808 at `ec3057c`). The M4.5 items did not touch `grid/model.rs`, `viewport.rs` or `order.rs`.

Re-run at `6ed581c` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench grid_viewport`, one run, exit 0, 2026-10-22): `p50_ns` 83, `p95_ns` 125, `cells_p95_ns` 1,000, `ingest_ms` 183, `heap_bytes` 113,900,996 (108.62 MiB), `layout_p95_ns` 15,875. Unchanged from `248e740`; no M5 item touched the grid model or viewport.

Bounded, compact model (T1, `3e5ead6` plus this PR, macOS 27.0.1 arm64, Apple M4 Pro, `--reps 3`, minimum over the repetitions). The bench ingests with `GridLimits::UNBOUNDED`, so it still measures a whole 1,000,000-row result. The base column is `3e5ead6`. All PR figures come from one saved run per width, taken after the cell walk was changed to read the payloads (the host was loaded by other processes, load average above 30, so the runs scatter: cells p95 1,042 to 1,542 ns, ingest 172 to 475 ms in repeated runs), and `--compare` of a second run against the 6-column saved run exits 0. The base figures are from the earlier measurement at `3e5ead6`.

| Columns | | p50 (ns) | p95 (ns) | Cells p50 / p95 (ns) | Ingest (ms) | Heap estimate (bytes) | Heap estimate (MB) |
| ------- | - | -------- | -------- | -------------------- | ----------- | --------------------- | ------------------ |
| 6 | base | 125 | 167 | n/a | 135 | 246,433,024 | 235.0 |
| 6 | PR | 83 | 125 | 833 / 1,209 | 202 | 113,900,996 | 108.6 |
| 8 | base | n/a | n/a | n/a | 156 | 310,433,136 | 296.1 |
| 8 | PR | 84 | 125 | 1,000 / 1,667 | 196 | 145,901,108 | 139.1 |

- Gate (AC4, 6 columns, the width the 246,433,024 baseline was measured at): p95 125 ns against the limit of 308 ns (25 % plus 100 ns over the base 167 ns); under the 1 ms budget; heap estimate 113,900,996 bytes against the target of 123,216,512 (half of 246,433,024): 46.2 % of the base, about 114 B per row. The 8-column run is for information (47.0 % of the base heap).
- Layout: 16 bytes per cell in one row-major vector per ingested batch, text, JSON and binary data in per-batch arenas, rare variants in a side table (ADR-0027).
- `cells_*` reads every cell of the 50 visible rows (value of inline cells; length, first and last byte of text, JSON and binary cells) at random positions of a 114 MB model, so it is bound by cold memory reads (about 100 cache lines per sample), not by the cell decoding. The walk of earlier versions never read the cells, so it has no counterpart in the base file. The gated walk of this version does less work than the base walk: the base read each row's `Vec` header (a pointer and a length from the chunk's row array, one memory access per row), the new one builds a `RowRef` from the shared chunk header and touches no per-row memory. The gated key therefore protects the viewport fetch, the chunk lookup and the row iteration; the cell reads are protected by `cells_p95_ns`. For scale, a scratch copy of the base bench that matched on each `Value` without reading text or bytes gave p50 833 ns and p95 1,000 ns at 6 columns, and p50 875 ns and p95 1,125 ns at 8 columns. That walk read less than the current one, so the two are not like for like; the current `cells_*` figures are the reference from now on.
- `ingest_ms` rose from 135 to 202 ms in the saved run (about 170 to 215 ms in repeated runs on the loaded host; not gated): the chunks are now built on the ingestion thread.

### Method

Run from the repository root on the commit being recorded:

1. `cargo bench -p sqler-ui --bench grid_viewport` (release profile). Flags: `--rows` (1000000), `--columns` (6), `--batch` (1024), `--viewport` (50), `--samples` (100000), `--reps` (3), `--save PATH`, `--compare PATH`, `--threshold-pct` (25), `--slack-ns` (100), `--cells-slack-ns` (500), `--budget-p95-ns` (1000000), `--layout-rows` (100000), `--layout-columns` (50), `--staged` (0, off; CI passes 10000). The bench also prints `layout_p50_ns` and `layout_p95_ns`: the time to fetch a viewport and visit every cell through a reversed, partly pinned column layout and a 10x5 selection. They are reported, not gated.
2. The bench ingests the rows through `spawn_ingest(SyntheticSource)` while the main thread polls, then draws 100,000 viewport positions from a fixed-seed xorshift64 generator and times each fetch with `Instant`. p50 and p95 are nearest-rank percentiles.
3. Memory: `heap_bytes` is the model's own estimate (capacity of the cell vectors, text and binary arenas and side tables of the chunks; MB = bytes / 1048576). On Linux the bench also prints `rss_delta_bytes` (`VmRSS` after ingestion minus before). On macOS, record the peak RSS with `/usr/bin/time -l <bench binary>`. A counting allocator would need `unsafe`, which the workspace forbids.
4. Each sample includes about 30 ns of `Instant` overhead.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job. The whole measurement runs `--reps 3` times and the reported p50 and p95 are the minimum over the repetitions (noise on a shared runner only adds time), on the base and on the PR alike. It fails if the PR p95 is more than 25 % plus 100 ns above the base p95, or if the p95 is above 1 ms (`--budget-p95-ns 1000000`). The 100 ns slack is about two timer ticks (one `Instant` pair costs about 41 ns); a 2x slowdown of the 300 ns p95 still fails (checked with `--viewport 200` against a baseline taken with the default 50: p95 584 ns against 292 ns, limit 465 ns). Without a baseline on the base branch only the 1 ms budget applies.

`cells_p95_ns` is gated the same way when the baseline file has that key: the minimum over the repetitions may not exceed the baseline by more than 25 % plus `--cells-slack-ns` (default 500 ns, about one cold cache line of noise per visible row; Linux runners may need a larger value, set it in `bench.yml` if a first run shows it). When the baseline has no `cells_p95_ns` (the base branch is older than this change), the bench prints a skip note and compares only `p95_ns`, so the rule is inert on the PR that introduces it and applies from the next PR on. Checked by hand: a baseline with `cells_p95_ns=100` fails with exit code 1, one with `100000` passes.

The rule is limit = baseline minimum x 1.25 + `--cells-slack-ns`. Worked out for the 6-column table above: a baseline `cells_p95_ns` of 1,209 ns gives 1,209 x 1.25 + 500 = 2,011 ns; one of 917 ns (the figure measured at `d1c6c64`, below) gives 1,646 ns.

`staged_*` (T11): with `--staged 10000` the bench makes the 1,000,000-row model an editable grid (one table `bench`, key `c0`), stages 10,000 text-cell edits through the grid editor, spread evenly over the rows, and times `GridTab::display_cell` for the 300 cells (50 rows x 6 columns) of a viewport that starts on a staged row. `staged_p50_ns` and `staged_p95_ns` are the minimum over the repetitions. `staged_p95_ns` is held to the 1 ms budget (`--budget-p95-ns`) and, only when the baseline file has the key `staged_p95_ns`, to baseline x 1.25 + `--cells-slack-ns` like `cells_p95_ns`; against an older base the bench prints a note and compares nothing. `staged_edits` and `staged_stage_ms` (the time to stage every edit) are reported and not gated. `bench.yml` passes `--staged 10000` for the PR, and for the base only when the base source of the bench contains the flag. In the interleaved rounds each round carries the staged keys; the judge call takes their minimum over the rounds like the others, and the baseline `staged_p95_ns` counts only if every base file has it (`--pr` files without staged keys simply leave them out; the judge report adds `staged_p50_ns`, `staged_p95_ns` and `base_staged_p95_ns`). Measured on the T11 branch (macOS 27.0.1 arm64, Apple M4 Pro, `--reps 3 --staged 10000`): `staged_p50_ns` 22,583, `staged_p95_ns` 24,333 (a second run: 22,500 and 24,041), `staged_stage_ms` 7; the viewport `p95_ns` stayed at 125 and `cells_p95_ns` at 959.

Interleaving (T15): the grid step of `bench.yml` follows the browser tree scheme. The base commit is checked out first; both sides are built (`--no-run`), then 6 rounds run in the order base, PR, PR, base, base, PR, each round one repetition (`--reps 1`, so the base gets the flag only if its source knows `--reps`) saved to its own file. A last judge-only call (`--pr` per PR file, `--compare` per base file, `--slack-ns 100`) takes the minimum per key over the rounds on each side (`p95_ns`, `cells_p95_ns` and the p50s; a baseline `cells_p95_ns` counts only if every base file has it) and applies the rule above to those minima. The keys, the 25 % and the slacks (100 ns, 500 ns) are unchanged; only the measurement is. Judge-mode report: `mode=judge`, `rounds=N`, the merged keys and `base_p95_ns` / `base_cells_p95_ns`. Checked by hand (macOS arm64): 3 default base rounds against 3 PR rounds with `--viewport 200` fail with exit 1 (`p95` 292 ns against 84 ns, limit 205 ns; `cells_p95` 2,584 ns against 958 ns, limit 1,697 ns); 3 default PR rounds pass (exit 0); a PR file without `p50_ns` exits 2. Over 5 default local runs the keys were stable: p50 83 ns, p95 84 ns, cells p50 709 to 791 ns, cells p95 958 to 1,000 ns. Over the 23 Bench runs that have both sides (2026-10-20 to 2026-10-21) no grid gate failed: PR/base p95 ranged 0.40 to 1.12 and PR/base cells p95 0.75 to 1.04, and the smallest margin to the limit was 37 % for p95 and 30 % for cells p95 (base p95 131 to 491 ns, base cells p95 1,499 to 3,005 ns). So the data does not show a grid flake; the interleaving is applied because the grid gate has the same structure as the tree gate (sequential runs on a shared runner) and the change costs about 30 s, not because a failure was observed. The keys, the 25 % and the slacks stay as they were.

`layout_*` (T5, not gated): 100,000 rows x 50 columns, 2 pinned, reversed order, a 10x5 selection; each sample fetches a viewport and visits every visible cell through the layout and the selection. Measured at `d1c6c64` (macOS 27.0.1 arm64, Apple M4 Pro, best of 3 repetitions): `layout_p50_ns` 13,916 and `layout_p95_ns` 14,792 (a second run: 13,959 and 14,542; other runs gave up to about 15.7 us p95). The same run gives the viewport `p95_ns` 84 and `cells_p95_ns` 917 for 6 columns.

## Results grid view

Data, not pass/fail. The grid tab (T8) draws a `gpui-component` virtual table over the 1M-row model above. Two measurements: a headless frame data cost (CPU only, no GPU) and a probe in the real window.

Frame data cost: `cargo run --release -p sqler-ui --example grid_scroll`. It scrolls 1,000,000 rows from top to bottom in 96 px steps, 32 px rows and a 760 px viewport, and times the cell text of every visible cell per simulated frame.

| Date | Commit | OS | Rows | Max rows per frame (cap) | Frame data cost p50 / p95 / max (µs) | `jump_to_end` (µs) | Heap (MB) | Notes |
| ---- | ------ | -- | ---- | ------------------------ | ------------------------------------ | ------------------ | --------- | ----- |
| 2026-10-20 | 23a3f72 | macOS 27.0.1 arm64, Apple M4 Pro | 1,000,000 | 28 (29) | 9.4 / 9.8 / 40.8 (second run: 9.6 / 10.1 / 63.2) | 9.0 | 235.0 | 333,327 frames; ingest 133 to 158 ms; two runs |
| 2026-10-21 | 3e5ead6 plus T1 | macOS 27.0.1 arm64, Apple M4 Pro | 1,000,000 | 28 (29) | 8.6 / 9.0 / 88.4 | 7.5 | 108.6 | 333,327 frames; ingest 182 ms; one run, bounded compact model (the synthetic grid is unbounded) |

In-app probe: `SQLER_GRID_PROBE=1` on `sqler`, with a `connections.toml` that holds one SQLite `:memory:` entry. It opens the synthetic grid, waits for the end of loading, scrolls to 600 evenly spaced rows (one per presented frame), then to the last row, and prints `SQLER grid_*` lines. RSS is read with `ps` (macOS) or `VmRSS` (Linux). Peak RSS comes from `/usr/bin/time -l`.

| Date | Commit | OS | Rows | Ingest (ms) | Frames | Frame interval p50 / p95 / max (ms) | Max rows drawn per frame | Last row drawn | RSS (MB) | Peak RSS (MB) |
| ---- | ------ | -- | ---- | ----------- | ------ | ----------------------------------- | ------------------------ | -------------- | -------- | ------------- |
| 2026-10-20 | 23a3f72 | macOS 27.0.1 arm64, Apple M4 Pro | 1,000,000 | 290.6 | 601 | 8.3 / 8.3 / 9.3 | 24 | yes | 346.5 | 346.6 |
| 2026-10-20 | 23a3f72 | macOS 27.0.1 arm64, Apple M4 Pro | 1,000,000 | 293.0 | 601 | 8.3 / 8.9 / 12.0 | 24 | yes | 346.5 | 346.6 |
| Linux X11, Wayland | | | | | | not measured: no GUI runner in `ci.yml` | | | | |
| Windows | | | | | | not measured: no GUI runner in `ci.yml` | | | | |

The frame interval is bound by the display (8.3 ms is a 120 Hz screen), so it shows responsiveness, not data cost. RSS with 1,000,000 rows open is above the 150 MB idle target, which is defined for an idle app with three connections and no large result, so it is recorded and not gated. The RSS rows above were measured before the compact model (the synthetic grid then held about 235 MB); the heap estimate of the same 1,000,000 rows is now 108.6 MB (see the model bench above). The in-app probe was not repeated in T1: it needs a display (maintainer check). The maintainer's interactive check (scrolling by hand) is still pending.

## Results grid sort and filter

Client-side sort and filter of the rows the model holds (PR #50, ADR-0031), measured headless on the 1M-row, 6-column synthetic model above (`GridLimits::UNBOUNDED`; columns: integer, text `r{i}c1`, float, boolean, integer, text). Each `*_p95_ns` is the p95 of `--samples` (5) full `GridOrder::step(None)` runs from a fresh order; `viewport_sorted_p95_ns` is the p95 of 100,000 random 50-row windows of the text-sorted order, each mapped through `RowOrder::model_row` and reading every cell; `slice_max_ns` is the longest single `step(Some(6 ms))` call of a text sort. All keys are the minimum over `--reps 3`. A second 1M-row model with three columns (decimal `-9999999.9999..9999999.9999`, URL-like text `https://example.com/api/v1/customers/{8 digits}/orders` in pseudo-random order, and a column of ints and floats in equal parts) covers the sorts the 8-byte key cache cannot always decide: `sort_dec_p95_ns`, `sort_url_p95_ns`, `sort_mixed_p95_ns` (p95 of 3 full sorts) and the longest 6 ms step of each, `slice_dec_max_ns`, `slice_url_max_ns`, `slice_mixed_max_ns`. The machine was shared with other builds (load average 9 to 16) during these runs; the three runs agree within 3 %.

| Date | Commit | OS | Rows | Key | Measurement | Budget | Gated |
| ---- | ------ | -- | ---- | --- | ----------- | ------ | ----- |
| 2026-10-21 | 2ead2b5 (PR #50) | macOS 27.0.1 arm64, Apple M4 Pro | 1,000,000 | `sort_p95_ns` (column 1 text, ascending) | 68.4 ms (68.4 to 70.5 over 3 runs) | 1.5 s | yes |
| | | | | `sort_int_p95_ns` (column 0 integer, descending) | 32.4 ms | | no |
| | | | | `sort_multi_p95_ns` (column 3 boolean up, column 1 text down) | 132.4 ms | | no |
| | | | | `filter_p95_ns` (quick filter "99", all columns) | 112.1 ms (112.1 to 117.3) | 500 ms | yes |
| | | | | `filter_cond_p95_ns` (column 2 `>` 500000 and column 1 contains 7) | 10.0 ms | | no |
| | | | | `viewport_sorted_p95_ns` | 1.9 us | 1 ms | yes |
| | | | | `slice_max_ns` | 6.03 ms | 16 ms | budget only |
| | | | | `order_heap_bytes` (text sort, published) | 17,000,000 (17 B per row) | | no |
| | | | | `order_peak_heap_bytes` (a text sort replacing another, longest point of the job) | 25,194,632 (about 25 B per row) | | no |
| | | | | `sort_dec_p95_ns` (decimal column) | 111.3 ms | 500 ms | budget only |
| | | | | `sort_mixed_p95_ns` (ints and floats) | 104.8 ms | 500 ms | budget only |
| | | | | `sort_url_p95_ns` (text with a 37-byte shared prefix) | 2.20 s | 15 s | yes |
| | | | | `slice_dec_max_ns` / `slice_mixed_max_ns` / `slice_url_max_ns` | 6.20 ms / 6.21 ms / 8.03 ms | 16 ms each | budget only |

All budgets are met (the text sort uses 5 % of its budget, the quick filter 23 %, the sorted viewport 0.2 %, the decimal and mixed sorts 22 % and 21 % of 500 ms, the URL sort 15 % of 15 s). The longest step is 6.0 to 6.4 ms for a 6 ms budget on the columns the key cache decides (text with distinct prefixes, decimals, ints and floats) and 8.0 ms on the URL column, where each comparison reads two cells; all are under the 16 ms frame budget on this machine. A step is the budget plus one unit of work (a sorted run of about 2 ms, 1,024 scanned rows or merged elements), so a slower machine or a slower comparison lengthens it by that unit, not by a whole run. The URL sort is 30 times slower than the text sort with distinct prefixes because the 8-byte key cannot decide any comparison there; it is the known slow case. The quick filter is slower than the text sort because it formats the non-text cells (integer, float, boolean) of every row into the scratch buffer for the substring test. Shared Linux CI runners can be several times slower than this machine, which is why the absolute budget of the URL sort is 15 s (a guard against a catastrophe, about 7 times the figure here); a regression of that sort is caught by the relative comparison against the base commit measured in the same job.

Re-run at `ec3057c` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench grid_sort`, three runs, exit 0, load average 2.1 to 4.1): `sort_p95_ns` 71.2, 71.4 and 70.9 ms (68.4 ms at PR #50; budget 1.5 s), `filter_p95_ns` 111.3, 110.3 and 110.9 ms (112.1 ms; budget 500 ms), `viewport_sorted_p95_ns` 1,875, 1,917 and 1,917 ns (budget 1 ms), `slice_max_ns` 6.04, 6.04 and 6.02 ms (budget 16 ms), `sort_url_p95_ns` 2.18, 2.17 and 2.18 s (2.20 s; budget 15 s), `sort_int_p95_ns` 32.3, 32.2 and 32.3 ms, `sort_multi_p95_ns` 139.8, 138.9 and 139.3 ms (132.4 ms, +5.6 %), `filter_cond_p95_ns` 9.9, 10.3 and 10.3 ms, `sort_dec_p95_ns` 114.8, 114.4 and 114.6 ms, `sort_mixed_p95_ns` 109.4, 109.3 and 109.2 ms, `slice_dec_max_ns` / `slice_mixed_max_ns` / `slice_url_max_ns` 6.48 / 6.47 / 8.04 ms (first run), `order_heap_bytes` 17,000,000 and `order_peak_heap_bytes` 25,195,592 (25,194,624 in the third run). Unchanged within the gate and within 25 % of the PR #50 figures; the largest change is +5.6 % on `sort_multi_p95_ns`.

Re-run at `248e740` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench grid_sort`, three runs, exit 0): `sort_p95_ns` 71.9, 70.9 and 71.4 ms (71.2, 71.4 and 70.9 at `ec3057c`; budget 1.5 s), `sort_int_p95_ns` 32.3, 32.3 and 32.7 ms, `sort_multi_p95_ns` 140.8, 141.2 and 142.4 ms (138.9 to 139.8), `filter_p95_ns` 111.2, 112.5 and 112.9 ms (110.3 to 111.3), `filter_cond_p95_ns` 10.5, 10.1 and 10.5 ms, `viewport_sorted_p95_ns` 2,083, 1,916 and 2,041, `slice_max_ns` 6.03 ms, `order_peak_heap_bytes` 25,194,624 to 25,195,592 (identical), `sort_dec_p95_ns` 114.8, 115.9 and 120.2 ms, `sort_url_p95_ns` 2.21, 2.36 and 2.29 s (2.17 to 2.18 s at `ec3057c`; the `url` sort is the slow column and +1 to +9 % here), `sort_mixed_p95_ns` 109.6, 110.9 and 110.8 ms. Within the machine's run-to-run range.

### Method

1. `cargo bench -p sqler-ui --bench grid_sort` (release profile). Flags: `--rows` (1000000), `--columns` (6, at least 4), `--batch` (1024), `--viewport` (50), `--samples` (5), `--window-samples` (100000), `--reps` (3), `--save PATH`, `--compare PATH`, `--threshold-pct` (25), `--sort-slack-ns` (20000000), `--filter-slack-ns` (10000000), `--viewport-slack-ns` (500), `--budget-sort-ns` (1500000000), `--budget-filter-ns` (500000000), `--budget-viewport-ns` (1000000), `--budget-slice-ns` (16000000), `--budget-sort-slow-ns` (500000000, decimals and mixed), `--budget-sort-url-ns` (15000000000). Exit 0 ok, 1 budget or regression, 2 usage error or unreadable baseline.
2. The bench ingests the rows with `spawn_ingest(SyntheticSource)`, then times full jobs of a new `GridOrder` (case-insensitive collation) with `Instant`. Window positions come from a fixed-seed xorshift64 generator.
3. `order_heap_bytes` is `GridOrder::heap_bytes()` after a text sort: 4 bytes per row of order, 4 per covered model row of inverse and 9 per row for the one sort key (tag and bits). `order_peak_heap_bytes` is the largest `heap_bytes()` seen between 6 ms steps while a second text sort (descending) replaces a published one: it adds the kept rows, the two merge buffers and the new inverse.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job (rule of PR #33). The bench takes the minimum over `--reps 3` for every key, on the base and on the PR alike (noise on a shared runner only adds time). `--compare` fails a gated key when the PR figure is above the baseline times 1.25 plus an absolute slack: 20 ms for `sort_p95_ns`, 10 ms for `filter_p95_ns`, 500 ns for `viewport_sorted_p95_ns` (about one cold cache line per visible row), and the same 25 % plus 20 ms for `sort_url_p95_ns`. A key that the baseline file lacks is not compared; the bench prints a skip note, so the rule is inert on the PR that adds it (the base has no `grid_sort` bench) and applies from the next PR on. The base step passes `--reps 3` only if the base source of the bench knows the flag. The budgets (1.5 s, 500 ms, 1 ms, 16 ms, and for the three added columns 500 ms, 500 ms, 15 s and 16 ms per longest step) fail the run with or without a baseline. `sort_int_p95_ns`, `sort_multi_p95_ns`, `filter_cond_p95_ns`, `order_heap_bytes`, `order_peak_heap_bytes` and the other keys of the added columns are reported, not gated (they only have their budgets; `sort_url_p95_ns` has its budget and the relative rule). Checked by hand: a baseline of `sort_p95_ns=1` with no slack exits 1; a baseline without the keys exits 0 with four notes; an unknown flag exits 2.

## Catalog tree

`sqler-catalog` on a synthetic 5,000-table schema (one schema `main`, 8 columns per table). Source `fake` is an in-memory `Introspect` that returns ready futures, so it measures the catalog and not a database. Source `sqlite` builds the same 5,000 tables in a `:memory:` `SQLite` session and is a reference figure only (20 samples, no budget).

| Date | Commit | OS | Source | Schemas cold p50 / p95 (ns) | Expand 5,000 tables cold p50 / p95 (ns) | Cached read p50 / p95 (ns) | Refresh schema p50 / p95 (ns) | Refresh all (ms) | Notes |
| ---- | ------ | -- | ------ | --------------------------- | --------------------------------------- | -------------------------- | ----------------------------- | ---------------- | ----- |
| 2026-10-20 | 7d80fbf plus this PR | macOS arm64 | fake | 833 / 1,042 | 203,125 / 351,167 | 41 / 42 | 441,167 / 443,959 | 4.44 (5,002 nodes) | 200 samples, 100,000 cached reads; budgets: cached read p95 10 us, expand p95 16 ms |
| 2026-10-20 | 7d80fbf plus this PR | macOS arm64 | sqlite (`:memory:`) | 8,375 / 14,250 | 2,100,500 / 2,260,834 | 41 / 42 | 2,453,750 / 2,623,625 | 552.1 (5,002 nodes) | 20 samples; reference, includes the driver and `SQLite` |

Re-run at `e98f4c8` (macOS arm64, Apple M4 Pro, source `fake`): the gated figure is unchanged within the gate (cached read p50 / p95 42 / 84 ns, one timer tick of difference, inside the 25 % + 150 ns rule). Not gated: expand cold p50 / p95 243,000 / 492,083 ns (was 203,125 / 351,167 ns at `7d80fbf`, about 40 % more at p95, budget 16 ms), schemas cold 2,375 / 6,084 ns, refresh schema 218,959 / 223,125 ns, refresh all 4.30 ms; one run. Linux CI, PR #29: cached read p95 91 ns on the base and 131 ns on the PR.

Re-run at `248e740` (macOS 27.0.1 arm64, Apple M4 Pro, source `fake`, one run): `hit_p50_ns` 42 and `hit_p95_ns` 84 (unchanged), `expand_cold_p50_ns` 235,125 and `expand_cold_p95_ns` 421,708, `schemas_cold_p95_ns` 3,000, `refresh_schema_p95_ns` 227,917, `refresh_all_ms` 4.011.

Notes:

- The cached read is at timer resolution. One `Instant` pair costs about 41 ns on this machine, so 41 / 42 ns says the read is below one tick, not that it costs 42 ns. Its p95 is not meaningful beyond "below one tick". The 10 us budget and the `--compare` rule (25 % + 150 ns slack, about three timer ticks, so one tick of jitter on a shared runner cannot fail the gate) still catch a real regression, such as a read that reaches the worker.
- The bench-regression guard is weak for the cached read: its p95 sits at timer resolution, so the `--compare` rule can only catch a large jump (the absolute 10 us budget is the real guard), not a small slowdown.
- Cold figures vary between runs (expand p50 was 137 to 203 us over several runs on the same machine).
- Refresh all re-fetches 5,002 cached nodes (the schema list, the 5,000-table listing and every `table()`), then applies them in one write.

### Method

Run from the repository root on the commit being recorded:

1. `cargo bench -p sqler-catalog --bench catalog_tree` (release profile). Flags: `--tables` (5000), `--columns` (8), `--samples` (200), `--hit-samples` (100000), `--source fake|sqlite` (fake), `--save PATH`, `--compare PATH`, `--threshold-pct` (25), `--slack-ns` (150), `--budget-hit-p95-ns` (10000), `--budget-expand-p95-ns` (16000000).
2. `schemas_cold_*`: a new catalog per sample, then `children` for the schema list. `expand_cold_*`: `children` for the N tables of `main`, cold. `hit_*`: `cached()` on that listing. `refresh_schema_*`: `refresh(Subtree(main))` with the list cached. `refresh_all_ms`: `refresh(All)` after `table()` was loaded for every table. p50 and p95 are nearest-rank percentiles.
3. Exit code 0 passes, 1 is a regression or budget failure, 2 is a usage error. Budgets apply to `fake` only.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job, and fails if `hit_p95_ns` is more than 25 % plus 50 ns above the base, or if a budget is exceeded. Without a baseline on the base branch only the budgets apply.

## Catalog index

`sqler-catalog` snapshot and name index on a synthetic catalog: one schema `main` with 5,000 tables, 500 views and 500 functions, and 8 columns on each of 50 loaded tables. Source is an in-memory `Introspect` that returns ready futures, so it measures the catalog and not a database. The design is in [ADR-0022](adr/0022-catalog-snapshot-name-index.md).

| Date | Commit | OS | Snapshot take p50 / p95 (ns) | Prefix lookup p50 / p95 (ns) | Fuzzy lookup p50 / p95 (ns) | Load 50 tables (ms) | Publish, nothing changed (ms) | Index size (bytes) | Notes |
| ---- | ------ | -- | ---------------------------- | ---------------------------- | --------------------------- | ------------------- | ----------------------------- | ------------------ | ----- |
| 2026-10-20 | 7a7dd54 plus this PR | macOS arm64 | 0 / 42 | 30,167 / 145,917 | 138,667 / 157,708 | 0.113 | 0.450 | 220,600 | run 1; 200 samples, 2,000 lookups per kind |
| 2026-10-20 | 7a7dd54 plus this PR | macOS arm64 | 0 / 42 | 21,208 / 138,042 | 121,875 / 155,250 | 0.109 | 0.433 | 220,600 | run 2, same setup |
| 2026-10-20 | 7a7dd54 plus this PR | macOS arm64 | 0 / 42 | 21,833 / 138,750 | 142,167 / 156,167 | 0.109 | 0.477 | 220,600 | run 3, same setup |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 42 / 84 | 76,792 / 241,167 | 132,375 / 151,583 | 0.401 | 0.537 | 220,600 | fresh, run 1 (the first run after the build); 200 samples, 2,000 lookups per kind |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 0 / 42 | 22,417 / 158,333 | 133,208 / 153,625 | 0.101 | 0.526 | 220,600 | fresh, run 2, same setup |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 0 / 42 | 36,708 / 162,417 | 134,333 / 155,750 | 0.102 | 0.515 | 220,600 | fresh, run 3, same setup |

At `e98f4c8` the prefix p95 is 158,333 to 241,167 ns over three runs (the 7a7dd54 runs were 138,042 to 145,917 ns), still about 4 times under the 1 ms budget; the first run after a build was the high one. Linux CI, PR #29: snapshot p95 50 ns, prefix p95 322,381 ns, fuzzy p95 260,054 ns.

Budgets (fake source): snapshot p95 10 us, prefix p95 1 ms, fuzzy p95 5 ms, index size 4 MiB. Prefix p95 is about 140 to 240 us (158 to 241 us at `e98f4c8`) and fuzzy p95 about 150 to 157 us, so both are far inside their budgets.

Notes:

- The snapshot take is one read lock and one `Arc` clone, below the timer resolution here (one `Instant` pair is about 41 ns), so p50 reads 0 and p95 one tick. It is the same kind of figure as the cached read of the catalog tree.
- Prefix needles cycle through `t`, `table_0`, `table_049` and `col_`. The broad needles (`t`, `table_0`) match 5,000 names and set the p95; the narrow ones are a binary search and cost a few microseconds. Fuzzy needles are `tbl49` and `vw1`, each a scan of all 5,500 object names plus the loaded columns.
- Results are bounded to 50 hits, so the cost of a broad needle is the scan plus a partial sort, not the size of the match set.
- Publish re-fetches every cached node (three listings and 50 tables) and applies an unchanged result: equal entries keep their `Arc` and index, only the generation grows.
- The index holds about 220 KB for 5,500 names and 400 columns; names are stored lowercase.
- The refresh-all figure of the catalog tree bench is in [Catalog tree](#catalog-tree); ADR-0022 compares it before and after this change.

### Method

Run from the repository root on the commit being recorded:

1. `cargo bench -p sqler-catalog --bench catalog_index` (release profile). Flags: `--tables` (5000), `--columns` (8), `--loaded` (50), `--samples` (200), `--lookup-samples` (2000), `--save PATH`, `--compare PATH`, `--threshold-pct` (25), `--slack-ns` (50), `--budget-snapshot-p95-ns` (10000), `--budget-prefix-p95-ns` (1000000), `--budget-fuzzy-p95-ns` (5000000), `--budget-index-bytes` (4194304).
2. `snapshot_*`: `Catalog::snapshot()` timed alone. `prefix_*` and `fuzzy_*`: `CatalogSnapshot::lookup` on one snapshot, cycling through the needles above, default limit 50. `load_tables_ms`: `load_tables` for the first `--loaded` tables. `publish_ms`: p50 of `refresh(All)` runs with nothing changed. `index_bytes`: `CatalogSnapshot::index_bytes()`. p50 and p95 are nearest-rank percentiles.
3. Exit code 0 passes, 1 is a regression or budget failure, 2 is a usage error.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job, and fails if `prefix_p95_ns` is more than 25 % plus 50 ns above the base, or if a budget is exceeded. Without a baseline on the base branch only the budgets apply.

## Object browser tree

`sqler_ui::browser::ObjectTree` on a synthetic schema of 5,000 tables, driven the way the workspace does: expand the schema, answer the listing, rebuild `rows()`. The bench uses no catalog or database, so it measures the tree model only. The catalog cost is in [Catalog tree](#catalog-tree).

| Date | Commit | OS | Expand 5,000 tables p50 / p95 (ns) | Expand folder p95 (ns) | Select p50 / p95 (ns) | Notes |
| ---- | ------ | -- | ---------------------------------- | ---------------------- | --------------------- | ----- |
| 2026-10-20 | 5672fc0 plus this PR | macOS arm64 | 627,125 / 1,037,708 | 188,750 | 1,000 / 1,959 | run 1; 200 samples, 20,000 select samples; budgets: expand p95 16 ms, select p95 1 ms |
| 2026-10-20 | 5672fc0 plus this PR | macOS arm64 | 649,459 / 1,115,375 | 192,541 | 1,291 / 3,208 | run 2, same setup |

Re-run at `e98f4c8` (macOS arm64, Apple M4 Pro, best of 3 repetitions): unchanged within the gate (expand p50 / p95 639,084 / 648,000 ns, folder p95 119,917 ns, select p50 / p95 1,250 / 2,500 ns; limits: expand p95 25 % + 250 us over the base, budgets 16 ms and 1 ms). Linux CI, PR #33: expand p95 2,135,860 ns on the base and 1,914,726 ns on the PR.

Re-run at `248e740` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench browser_tree`, one run, exit 0): `expand_p50_ns` 624,583, `expand_p95_ns` 681,375, `folder_p95_ns` 129,833, `select_p50_ns` 1,000, `select_p95_ns` 2,125. At `ec3057c` the figures were 609,917, 616,417, 121,041, 1,000 and 2,042, so expand p95 is +10.5 % in this single local run. The `Bench` workflow (base against PR, same runner) was green at the final head of every M4.5 PR (#59 to #64), so the gate did not trip; the local difference was not reproduced or explained (the bench measures the headless tree model, whose code M4.5 did not change) and is reported as an open item with the cause not established.

Re-run at `6ed581c` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench browser_tree`, one run, exit 0, 2026-10-22): `expand_p50_ns` 621,583, `expand_p95_ns` 634,042, `folder_p95_ns` 123,541, `select_p50_ns` 1,000, `select_p95_ns` 2,166. Within the gate and the budgets; the `248e740` p95 of 681,375 was a single high run.

Expand p95 is about 0.7 ms against the 16 ms budget (one frame at 60 Hz), and select p95 is 2 to 3 us against 1 ms. The rendered sidebar is a virtualized list, so the cost of drawing does not grow with the 5,000 rows; that part is checked by eye (see the PR checklist), not by this bench.

### Method

1. `cargo bench -p sqler-ui --bench browser_tree` (release profile). Flags: `--tables` (5000), `--samples` (200), `--select-samples` (20000), `--reps` (3), `--warmup` (30), `--save PATH`, `--compare PATH` (repeatable), `--pr PATH` (repeatable, judge mode), `--threshold-pct` (25), `--slack-ns` (250000), `--floor-ns` (1000000), `--simulate-slowdown`, `--budget-expand-p95-ns` (16000000), `--budget-select-p95-ns` (1000000). The first `--warmup` iterations are not recorded. `--simulate-slowdown` makes the timed expand do its work twice (a test-only 2x regression; the second tree is built outside the timed span). With `--pr`, nothing is measured: the report is the minimum per key over the `--pr` files, judged against the minimum over the `--compare` files, and it prints `mode=judge`, `rounds=<files>` and `base_expand_p50_ns` / `base_expand_p95_ns`.
2. `expand_*`: expand the schema, load the 5,000-table listing, rebuild the rows. `folder_p95_ns`: expand the `Tables` folder under it. `select_*`: change the selection. p50 and p95 are nearest-rank percentiles.
3. Exit code 0 passes, 1 is a regression or budget failure, 2 is a usage error.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request. Base and PR rounds alternate inside one step (T15; see the noise analysis below): 8 rounds, each a single in-process repetition (`--reps 1`), in the order base, PR, PR, base, base, PR, ... so that neither side owns the cold start or the slow minute. Each round saves its report. A final judge-only call (`--pr` per PR file, `--compare` per base file) takes the minimum per key over the rounds of each side and decides. The rule on those minima:

- `expand_p50_ns` fails when it is above `max(base_p50 x 1.25 + 250 us, 1 ms)` (`--threshold-pct 25`, `--slack-ns 250000`, `--floor-ns 1000000`). The floor keeps a base that happens to be very fast from producing a limit near the timer and allocator noise.
- `expand_p95_ns` is informational: a `WARN:` line above `2 x base_p95 + 2 x slack`, never a failure. Its absolute budget (16 ms) still fails, and so does the select p95 budget (1 ms).
- Without a baseline on the base branch only the budgets apply. A base whose bench source has no `--reps` gets no `--reps` flag (grep guard in the workflow); `--pr`, `--warmup` and `--floor-ns` are never passed to the base.

Why p50 and not p95: with 200 samples the p95 is the tenth-largest value, so a single descheduling burst of ten expands moves it; the p50 moves only if half the samples slow down, which is a real shift. The minimum over rounds drops the rounds that landed on a slow minute of the shared runner (noise only adds time), and alternating the order spreads a drift of the runner over both sides instead of loading it on one.

Limits, worked out: a base minimum p50 of 1.20 ms gives 1.20 x 1.25 + 0.25 = 1.75 ms; one of 0.62 ms (this machine) gives 1.02 ms (above the floor by 20 us). A 2x regression fails when the baseline is at least about 0.5 ms (below that, the 1 ms floor or the 250 us slack is above 2x): on Linux CI (median base p50 1.4 ms) 2x is 2.8 ms against a limit of 2.0 ms.

Checks by hand (macOS arm64, Apple M4 Pro, 5000 tables, bench binary from the T15 tree):

- Saved-baseline method: `--save base.txt` gave expand p50 625,916 ns. `--simulate-slowdown --compare base.txt` gave p50 1,231,083 ns against a limit of 1,032,395 ns, exit 1 (`FAIL: expand p50 ... regressed`). A normal run against the same file exited 0.
- Judge method: 3 base files and 3 `--simulate-slowdown` PR files (`--reps 1` each) judged with `--pr` / `--compare`: expand p50 1,229,875 ns against 611,292 ns (limit 1,014,115 ns), exit 1. Normal against normal, 3 base and 3 PR files, repeated 10 times: 0 failures.
- Halved baseline (the old doc check): with the p50 and p95 of the baseline halved to 312,958 / 324,167 ns, a normal run (about 0.62 ms) exits 0, with or without `--floor-ns 0`. That is the rule working as designed, not a miss: against a 0.31 ms baseline the limit is 0.64 ms because of the 250 us slack, and 0.62 ms is not a 2x regression of anything the gate can see. The 2x demos above start from the real baseline.
- For contrast, the old rule (p95, 25 % + 50 ns in CI): `expand p95 1,868,787 ns regressed against baseline 883,585 ns (limit 1,104,531 ns)` failed run 39471570387 on 2026-10-20, with p50 718,436 against 726,307 ns.

### Noise analysis (T15)

Observed: the old gate (p95 of 200 samples, 25 % + 50 ns, base and PR benches run minutes apart, one repetition each on the Linux runner) failed 5 of the last 100 Bench runs (2026-10-20 18:04 to 2026-10-21 06:22 UTC), all five on the browser-tree expand p95 and none on any other gate. In four of the five the p50 was within 7 % of the base and only the p95 had doubled or worse (the fifth, `rsa-alternatives`, had a p50 of 1.74 ms against 0.68 ms as well). Sources of noise, in the order they matter:

1. **Runner contention between the base and the PR run.** The two benches ran in separate steps with a build between them. The base p50 swung 0.71 to 3.13 ms over the runs and the PR p50 0.84 to 3.95 ms; inside one run the two sides differ by a factor of up to 2.6 in p50 (runs 39522092691: 0.71 against 1.84 ms, 39527230085: 1.36 against 2.88 ms) and in both directions (39518119097: 3.05 against 1.46 ms). A shift of the whole distribution on one side cannot be told from a regression with sequential runs, whatever percentile is used. The retro-applied p50 rule on single runs (below) would have failed 4 runs for this reason alone. Fix: interleave (ABBA) and take the minimum over rounds.
2. **Few samples at a high percentile.** On the 22 tabulated runs p95 / p50 was above 2.5 on 7 base figures (above 3 on 2) and on 3 PR figures (base of 39517969629: p50 1.17 ms, p95 3.26 ms; the PR side of the same run: 1.11 / 1.13 ms). The p95 of 200 samples is the 190th value: ten slow iterations (a scheduler tick, a neighbour VM) set it. Median of the p95 over the 22 base runs: 2.60 ms against a median p50 of 1.42 ms. Fix: gate on the p50, keep the p95 as a warning.
3. **Cold first iterations.** The bench took no warm-up, so the first allocations of the 5,000-table listing (allocator growth, page faults, cold instruction cache) were samples. Locally the effect is small once the best of 3 repetitions is taken (below), but it is cheap to remove: 30 unrecorded iterations first (`--warmup 30`).
4. **Timer resolution** is not a factor at this scale: one `Instant` pair costs about 41 ns on macOS against a 0.6 ms expand, 0.007 %.
5. **Allocator state** matters through the cold start and through the other benches' leftovers; in the interleaved scheme every round is a fresh process, so nothing carries over.

Local measurements (macOS 27.0.1 arm64, Apple M4 Pro, idle machine, release bench binary; this machine is the quiet case, CI is not):

| Setup | expand p50 (ns), 5 runs | expand p95 (ns), 5 runs |
| ----- | ------------------------ | ------------------------ |
| default (`--reps 3 --warmup 30`) | 622,375 / 612,500 / 614,292 / 610,500 / 612,833 | 648,042 / 631,708 / 632,292 / 624,708 / 631,416 |
| `--warmup 0` (best of 3 reps) | 612,500 / 612,791 / 619,208 / 611,583 / 610,584 | 626,875 / 626,417 / 638,708 / 625,708 / 624,791 |
| `--reps 1 --warmup 0` | 613,416 / 613,250 / 616,583 / 611,667 / 616,042 | 661,291 / 623,708 / 632,834 / 628,333 / 638,209 |
| `--reps 1 --warmup 30` | 612,125 / 613,084 / 613,459 / 614,542 / 625,000 | 639,333 / 657,416 / 631,834 / 628,250 / 658,542 |

The p50 spread is 1.0 to 2 % on an idle machine in every setup, the p95 spread 4 to 6 % (single repetitions), so the macOS runs cannot show the CI effects; the warm-up is not measurable here (the first iteration costs less than the p95 resolution of 200 samples). It is kept because the Linux CI base showed cold-start outliers (p95 / p50 above 3 in 2 of 22 runs, above 2.5 in 7) and the cost is 30 iterations of 0.6 ms. This is a judgement, not a measured gain; say so if it needs to be revisited. The `grid_viewport` and the CI table below carry the evidence for the rest.

CI data, last 22 Bench runs with a browser-tree figure on both sides (old bench: one run per side, p95-gated; the p50 columns are printed by the old bench too). "p50 rule, single run" retro-applies the new threshold to those single runs (it is what the new rule would say without interleaving); "p95 rule in force" is the old gate:

| Run | Branch | Date | Base p50 (ms) | PR p50 (ms) | p50 PR/base | p50 rule, single run | Base p95 (ms) | PR p95 (ms) | p95 PR/base | p95 rule in force | Job |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 39511007499 | export-text-writers | 10-20 | 1.47 | 1.41 | 0.95 | pass | 3.93 | 3.87 | 0.99 | pass | success |
| 39513530762 | grid-bounded-model | 10-20 | 1.45 | 1.48 | 1.02 | pass | 3.92 | 3.98 | 1.02 | pass | success |
| 39514446344 | grid-bounded-model | 10-20 | 1.15 | 1.13 | 0.98 | pass | 3.29 | 1.21 | 0.37 | pass | success |
| 39516199348 | staged-edits-model | 10-20 | 3.13 | 3.15 | 1.00 | pass | 3.20 | 3.20 | 1.00 | pass | success |
| 39516457224 | linux-binary-size | 10-20 | 1.55 | 1.62 | 1.05 | pass | 1.91 | 1.95 | 1.02 | pass | success |
| 39516706188 | cli-export | 10-20 | 1.44 | 1.43 | 0.99 | pass | 1.54 | 1.51 | 0.98 | pass | success |
| 39517058811 | staged-edits-model | 10-20 | 1.52 | 2.50 | 1.65 | fail | 2.59 | 2.56 | 0.99 | pass | success |
| 39517969629 | cli-export | 10-20 | 1.17 | 1.11 | 0.95 | pass | 3.26 | 1.13 | 0.35 | pass | success |
| 39518119097 | grid-columns-selection | 10-20 | 3.05 | 1.46 | 0.48 | pass | 3.48 | 1.62 | 0.47 | pass | success |
| 39519279591 | export-parquet-xlsx | 10-21 | 1.41 | 1.45 | 1.03 | pass | 1.46 | 2.98 | 2.05 | fail | failure |
| 39520414293 | export-parquet-xlsx | 10-21 | 2.56 | 1.18 | 0.46 | pass | 2.61 | 2.53 | 0.97 | pass | success |
| 39522092691 | grid-columns-selection | 10-21 | 0.71 | 1.84 | 2.60 | fail | 1.83 | 1.90 | 1.04 | pass | success |
| 39527167827 | grid-cell-detail | 10-21 | 1.43 | 1.45 | 1.01 | pass | 1.56 | 1.54 | 0.99 | pass | success |
| 39527230085 | grid-copy-as | 10-21 | 1.36 | 2.88 | 2.12 | fail | 2.89 | 2.94 | 1.02 | pass | success |
| 39528689537 | grid-sort-filter | 10-21 | 1.86 | 3.95 | 2.13 | fail | 4.08 | 4.21 | 1.03 | pass | success |
| 39529727634 | grid-sort-filter | 10-21 | 1.44 | 1.34 | 0.93 | pass | 1.57 | 1.49 | 0.95 | pass | success |
| 39530672804 | grid-export | 10-21 | 0.87 | 0.84 | 0.97 | pass | 2.79 | 2.89 | 1.04 | pass | success |
| 39552491949 | grid-export | 10-21 | 1.35 | 1.42 | 1.05 | pass | 1.39 | 1.49 | 1.07 | pass | success |
| 39552612160 | grid-cell-detail | 10-21 | 0.89 | 0.85 | 0.96 | pass | 3.10 | 0.92 | 0.30 | pass | success |
| 39552987230 | grid-copy-as | 10-21 | 0.92 | 0.92 | 1.00 | pass | 0.99 | 0.99 | 1.00 | pass | success |
| 39554975641 | grid-export | 10-21 | 1.02 | 1.04 | 1.02 | pass | 1.08 | 2.35 | 2.17 | fail | failure |
| 39555341031 | grid-cell-detail | 10-21 | 1.14 | 1.16 | 1.01 | pass | 1.17 | 1.18 | 1.02 | pass | success |

Retro-application: the old p95 rule failed 2 of these 22 runs (39519279591: p95 1.46 against 2.98 ms; 39554975641: 1.08 against 2.35 ms; three older failures are not in the table: p95 4.17 against 3.17 ms, 1.87 against 0.88 ms and 7.75 against 0.73 ms, with p50 within 7 % in the first two and 1.74 against 0.68 ms in the third). The new threshold applied to single runs would have passed both of them (p50 within 3 %) but failed 4 others (39517058811, 39522092691, 39527230085, 39528689537: PR p50 1.65 to 2.60 times the base) which the old rule passed because their p95 was equal on both sides. Those four are whole-distribution shifts between two sequential runs, with nothing in the PR touching the tree: a p50 gate without interleaving is as flaky as the p95 gate, only in different runs. That is the reason for the ABBA rounds with a minimum per side, not a different percentile alone. Whether the interleaved rule passes every one of such runs can only be shown by the reruns below.

Note on this PR's own run: the base (main) still has the old browser bench without warm-up, which is biased slow, so the PR side looks faster than the base. The bias is transitional; from the next PR on, both sides run the same code.

CI reruns of this change (PR #53, run 39559885296, five attempts of the Bench job on the same commit, all green; Linux x86_64 runner, interleaved scheme, 8 browser rounds and 6 grid rounds). Each figure is the minimum over the rounds of that side; the limit is `max(base p50 * 1.25 + 250 us, 1 ms)` for the browser expand p50 and `base * 1.25 + 100 ns` / `+ 500 ns` for the grid p95 / cells p95; margin = (limit - PR) / limit. Times in ns.

| Attempt | Browser PR p50 | Base | Limit | Margin | Grid PR p95 | Base | Limit | Margin | Cells PR p95 | Base | Limit | Margin |
| ------- | -------------- | ---- | ----- | ------ | ----------- | ---- | ----- | ------ | ------------ | ---- | ----- | ------ |
| 1 | 1,465,045 | 1,437,407 | 2,046,759 | 28.4 % | 230 | 231 | 389 | 40.8 % | 2,214 | 2,224 | 3,280 | 32.5 % |
| 2 | 676,137 | 695,497 | 1,119,371 | 39.6 % | 141 | 141 | 276 | 49.0 % | 1,713 | 1,692 | 2,615 | 34.5 % |
| 3 | 1,537,551 | 1,564,389 | 2,205,486 | 30.3 % | 240 | 241 | 401 | 40.2 % | 2,294 | 2,374 | 3,468 | 33.8 % |
| 4 | 1,167,075 | 1,184,480 | 1,730,600 | 32.6 % | 191 | 201 | 351 | 45.6 % | 2,013 | 2,033 | 3,041 | 33.8 % |
| 5 | 663,822 | 855,352 | 1,319,190 | 49.7 % | 141 | 141 | 276 | 49.0 % | 1,643 | 1,623 | 2,529 | 35.0 % |

The smallest margins are 28.4 % (browser expand p50), 40.2 % (grid p95) and 32.5 % (cells p95), so none is under 10 %. The runner speed still moved by 2.3x between attempts (browser p50 664 us to 1.54 ms), but PR and base moved together within each attempt (PR/base ratio 0.78 to 1.02), which is what the interleaving is for. The base browser bench here is main's old one (no warm-up), so attempt 1 to 5 compare a warmed PR against a cold base, a bias in the PR's favour of at most the first iteration.

## Completion

`sqler_completion::complete`, `hover` and `diagnose` on a synthetic schema of 5,000 tables (8 columns each) in `public` plus 20 more schemas of 10 tables, `PostgreSQL` dialect, driven through `MemorySchema`. The bench is engine-only: no editor, no popup and no catalog, so it is not the end-to-end figure that T10 measures with the editor.

| Date | Commit | OS | complete p50 / p95 (ns) | Worst scenario p95 (ns) | Notes |
| ---- | ------ | -- | ----------------------- | ----------------------- | ----- |
| 2026-10-20 | 7a7dd54 plus this PR | macOS arm64, Apple M4 Pro | 116,000 / 514,333 | 986,833 (`from_fuzzy`) | run 1; 2,000 samples per scenario; budget p95 16 ms |
| 2026-10-20 | 7a7dd54 plus this PR | macOS arm64, Apple M4 Pro | 114,167 / 449,458 | 487,500 (`from_fuzzy`) | run 2, same setup |
| 2026-10-20 | 7a7dd54 plus this PR | macOS arm64, Apple M4 Pro | 115,792 / 465,167 | 581,250 (`from_fuzzy`) | run 3, same setup |
| 2026-10-20 | 7a7dd54 plus this PR | macOS arm64, Apple M4 Pro | 71,583 / 442,291 | 480,250 (`from_fuzzy`) | run 4, after the identifier-folding change; best of 3 repetitions (`--reps 3`) |
| 2026-10-20 | 66f541f (PR #35) | macOS arm64, Apple M4 Pro | 56,125 / 386,917 | 2,065,041 (`diagnose_big_script`) | best of 3 repetitions; `hover_p95_ns` 112,083, `diagnose_p95_ns` 2,058,917; the completion scenarios are unchanged |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 45,791 / 399,208 | 2,487,958 (`diagnose_big_script`) | fresh, run 1 of 3; best of 3 repetitions; `hover_p95_ns` 115,292, `diagnose_p95_ns` 2,356,875; worst completion scenario `from_fuzzy` 408,667 |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 48,292 / 392,958 | 2,387,916 (`diagnose_big_script`) | fresh, run 2 of 3; `hover_p95_ns` 114,000, `diagnose_p95_ns` 2,374,334; worst completion scenario `from_fuzzy` 407,500 |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 57,958 / 405,042 | 2,406,625 (`diagnose_big_script`) | fresh, run 3 of 3; `hover_p95_ns` 115,000, `diagnose_p95_ns` 2,397,459; worst completion scenario `from_fuzzy` 426,791 |
| 2026-10-21 | 248e740 | macOS 27.0.1 arm64, Apple M4 Pro | 50,667 / 403,291 | 2,523,250 (`diagnose_big_script`) | fresh, run 1 of 3 (`cargo bench -p sqler-completion --bench completion`); `hover_p95_ns` 142,250, `diagnose_p95_ns` 2,483,833 |
| 2026-10-21 | 248e740 | macOS 27.0.1 arm64, Apple M4 Pro | 65,458 / 396,167 | 2,493,416 (`diagnose_big_script`) | fresh, run 2 of 3; `hover_p95_ns` 145,041, `diagnose_p95_ns` 2,454,667 |
| 2026-10-21 | 248e740 | macOS 27.0.1 arm64, Apple M4 Pro | 52,833 / 392,459 | 2,515,292 (`diagnose_big_script`) | fresh, run 3 of 3; `hover_p95_ns` 139,125, `diagnose_p95_ns` 2,428,750; `from_fuzzy_p95_ns` 409,750 to 427,084; the worst scenario is 3 to 6 % above `ec3057c` (2.37 to 2.43 ms) in all three runs; no completion code changed in M4.5, cause not established |

DoD figure at `e98f4c8` (worst of three runs): `complete_p95_ns` 405,042 ns (0.41 ms), worst completion scenario p95 426,791 ns (`from_fuzzy`), worst of all twelve scenarios 2,487,958 ns (`diagnose_big_script`, 2.5 ms); all against the 16 ms budget, so completion p95 on 5,000 tables is met with about 39 times to spare (about 6 times for the slowest diagnose scenario). Linux CI figures: PR #30 `complete_p95_ns` 464,994 ns, worst 478,323 ns; PR #35 `complete_p95_ns` 690,583 ns, `hover_p95_ns` 257,328 ns, `diagnose_p95_ns` 5,038,306 ns, worst 5,149,531 ns (base 693,868 / 704,067 ns).

Per scenario p95 in run 2 (ns): `from_empty` 211,541; `from_prefix` 404,209; `from_fuzzy` 487,500; `select_list_join5` 51,958; `alias_dot` 2,292; `where_prefix` 12,583; `insert_columns` 1,958; `big_script` (2,000 statements, cursor mid-file) 121,541.

Hover and diagnose p95 on the same schema (T7 run, ns): `hover_column` (`a3.col_0` in a 5-table join) 2,625; `hover_big_script` (table name in the middle of a 2,000-statement script) 113,000; `diagnose_join5` (5-table join, one misspelt column) 139,917; `diagnose_big_script` (the 2,000-statement script plus one statement with an unknown column) 2,065,041. Diagnosing the whole script takes about 2 ms, 8 times under the budget; most of it is scanning 2,000 statements, because the relation lookups go through a per-call name index.

The worst completion p95 is about 0.5 to 1.0 ms against the 16 ms budget (one frame at 60 Hz), roughly 16 to 32 times under it. The cost is dominated by scanning the 5,000 relation names (`FROM` with a prefix or a fuzzy match); in-scope column completion, which is what the editor asks for most, takes microseconds. The Linux CI figures are recorded by the `Bench` workflow summary (`completion-pr.txt`) on the pull request and are not part of this table until that run exists.

### Method

1. `cargo bench -p sqler-completion --bench completion` (release profile). Flags: `--tables` (5000), `--columns` (8), `--samples` (2000 per scenario), `--save PATH`, `--compare PATH`, `--reps` (3), `--threshold-pct` (25), `--slack-ns` (250000), `--budget-p95-ns` (16000000).
2. Twelve scenarios, each a fresh request per sample after 20 warm-up requests. Eight time `complete`: `from_empty` (`FROM |`), `from_prefix` (`FROM table_00|`), `from_fuzzy` (`FROM t_4|`), `select_list_join5` (select list over a 5-table join), `alias_dot` (`a.|`), `where_prefix` (`WHERE col_|` over a join), `insert_columns` (`INSERT INTO t (|`) and `big_script` (a 2,000-statement script, cursor in the middle). Two time `hover`: `hover_column` (cursor on `a3.col_0` in the 5-table join) and `hover_big_script` (cursor on the table name of the middle statement of the 2,000-statement script). Two time `diagnose`: `diagnose_join5` (the 5-table join with one misspelt column) and `diagnose_big_script` (the 2,000-statement script plus one statement with an unknown column). A scenario whose completion has no items, whose hover is `None` or whose diagnose finds nothing fails the run.
3. The measurement loop (all scenarios) runs `--reps` times in process; `complete_p95_ns`, `hover_p95_ns` and `diagnose_p95_ns` are each the minimum over the repetitions (each metric from its own best one), while the per-scenario lines and `worst_p95_ns` come from the repetition with the lowest `complete_p95_ns`. Output is `key=value` lines: `<scenario>_p50_ns` and `<scenario>_p95_ns`, `complete_p50_ns` and `complete_p95_ns` over the samples of the eight completion scenarios only (so the baseline stays comparable), `hover_p95_ns` and `diagnose_p95_ns` over the samples of their scenarios, and `worst_p95_ns`, the largest scenario p95 of all twelve. p50 and p95 are nearest-rank percentiles.
4. Exit code 0 passes, 1 is a regression or a budget failure, 2 is a usage error or an unreadable baseline.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job, and fails if `complete_p95_ns`, taken as the minimum over 3 in-process repetitions, is more than 25 % plus 250 us (`--slack-ns 250000`, 1/64 of the budget) above the same figure on the base, or if `complete_p95_ns`, `hover_p95_ns`, `diagnose_p95_ns` or `worst_p95_ns` exceeds the 16 ms budget. `hover_p95_ns` and `diagnose_p95_ns` are compared the same way when the base's file has them (a base from before T7 does not; `bench.yml` therefore gates them only once the base branch has a baseline with those keys, and until then only the 16 ms budget applies to them; the gate code is checked by comparing against a saved file that has the keys). Without a baseline on the base branch only the budget applies. The p95 of this bench is in the hundreds of microseconds and sits in the tail of the relation-scan scenarios, so the fixed 50 ns slack of the ns-scale benches would flake on a shared runner; the minimum over repetitions drops a noisy repetition and the 250 us floor keeps a real 2x regression (about +450 us on the M4, more on a runner) failing. If the gate still flakes, report it here rather than widening the default silently.

## Gate review (T15)

The same data (the last 100 Bench runs, of which the 22 to 23 with both sides in the logs were read) was checked for the other gated benches. Only the browser expand p95 ever failed in that window. `catalog_tree` `hit_p95_ns` (100,000 samples, 25 % + 150 ns): PR/base ratio 0.90 to 1.07 over 22 runs, base 99 to 148 ns, 0 failures under the current rule. `catalog_index` `prefix_p95_ns` (25 % + 50 ns): ratio 0.84 to 1.05, base 153 to 328 us, 0 failures. `completion` `complete_p95_ns` (minimum of `--reps 3`, 25 % + 250 us): ratio 0.81 to 1.03, base 368 to 760 us, 0 failures. Nothing here points at noise, so their rules are unchanged (no gate was loosened). `grid_sort` is wired into `bench.yml` on this base and was reviewed from the same logs: 8 runs have both the base and the PR figure (`--reps 3`, minimum), from PRs that mostly did not touch the grid code, so the ratios are mostly runner noise. `sort_p95_ns` (25 % + 20 ms): PR/base ratio 0.97 to 1.06, base 101 to 157 ms. `filter_p95_ns` (25 % + 10 ms): ratio 0.95 to 1.02, base 147 to 200 ms. `viewport_sorted_p95_ns` (25 % + 500 ns): ratio 0.81 to 1.04, base 2.9 to 4.0 us, the largest absolute rise 91 ns. `sort_url_p95_ns` (25 % + 20 ms): ratio 0.93 to 1.05, base 3.4 to 4.6 s. 0 failures, the worst ratio (1.06) is far from 1.25, so the grid sort rules are unchanged. The sample is small (8 pairs, the bench is new in the window), so it is a first look, not a long record. The one `catalog_index` failure seen after that window (`prefix_p95_ns` 291 us against a base of 156 us, limit 195 us, branch `grid-inline-editing`, run 39566076260) is a single sample and was not investigated here; if it repeats, interleave that bench the same way. `editor_*` and `export_throughput` are not in `bench.yml`, so there is nothing to review for them; a bench added later should follow the interleaved A/B plus judge pattern of the browser step. If one of the ns-scale gates (`hit_p95_ns` with a 150 ns slack, `prefix_p95_ns` with a 50 ns slack) ever flakes, the first step is the same interleaving, not a wider slack.

## SQL editor buffer

`sqler_ui::editor` headless, on the 10,000-line `synthetic` script (541,580 bytes) that `dev:open-large-sql-script` opens ([ADR-0025](adr/0025-sql-editor-component.md)): the bracket index of the whole buffer, a bracket lookup at the cursor, the SQL formatter over the whole buffer and the search for the next occurrence of a word. It stops where GPUI starts: typing, scrolling, highlighting and the paint of the bracket frame are not measured and are a maintainer check on a display.

| Date | Commit | OS | Bracket build p95 (ns) | Bracket lookup p95 (ns) | Format p95 (ns) | Next occurrence p95 (ns) | Notes |
| ---- | ------ | -- | ---------------------- | ----------------------- | --------------- | ------------------------ | ----- |
| 2026-10-20 | d577740 (PR #36) | macOS arm64 | about 1,000,000 | not quoted | about 10,700,000 | about 52,000 | quoted from the PR body, not re-measured; 10,000 lines; budget 16 ms |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 1,349,541 | 250 | 11,016,542 | 53,791 | fresh, run 1 of 3 (bracket build p50 907,417, format p50 8,968,042); 30 samples |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 487,750 | 167 | 9,138,291 | 51,417 | fresh, run 2 of 3 (bracket build p50 478,583, format p50 8,913,875) |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 512,000 | 167 | 9,071,458 | 53,417 | fresh, run 3 of 3 (bracket build p50 483,416, format p50 8,858,292) |

Bracket build and format are the two budgeted figures (16 ms, one frame at 60 Hz): the worst format p95 is 11.0 ms and the worst bracket build p95 is 1.35 ms, so both are met. Format is the closest to its budget (about 31 % to spare in the worst run); it runs only on the explicit `editor:format-sql` command, not per keystroke. The bracket lookup is at timer resolution (one `Instant` pair is about 41 ns, so 167 to 250 ns is a few ticks). The first run after a build was the slow one for the bracket build (1.35 ms against 0.49 to 0.51 ms).

Re-run at `6ed581c` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench editor_buffer`, one run, exit 0, 2026-10-22): bracket build p50 / p95 950,625 / 1,311,791 ns, bracket lookup p95 167 ns, format p50 / p95 8,610,709 / 10,653,042 ns, next occurrence p95 58,708 ns (budget 16 ms for bracket build and format: met; format is the closest at about 33 % to spare). Not part of `bench.yml`.

### Method

1. `cargo bench -p sqler-ui --bench editor_buffer` (release profile). Flags: `--lines` (10000), `--samples` (30), `--budget-p95-ns` (16000000). Not part of `bench.yml`; put the numbers in the PR body.
2. Output is `key=value` lines: `bracket_build_p50_ns` and `bracket_build_p95_ns`, `bracket_lookup_p95_ns`, `format_p50_ns` and `format_p95_ns`, `occurrence_p95_ns`. p50 and p95 are nearest-rank percentiles over the samples.
3. Exit code 0 passes, 1 when the bracket build or the format p95 exceeds the budget, 2 is a usage error.

## SQL editor intellisense

`sqler_ui::editor::intellisense` end to end, headless: a fresh `catalog.snapshot()`, `CatalogSchema::new` and one operation per sample (the engine and catalog work behind a keystroke, ADR-0022), on the synthetic source of 5,000 tables (8 columns each) in `public` plus 20 more schemas of 10 tables, `PostgreSQL` dialect. The catalog worker runs on a tokio runtime and the columns of the tables a scenario names are loaded before sampling. The input is a ready `&str` and a byte offset. It stops where the GPUI providers start: `Rope::to_string`, the byte-offset to LSP-position mapping, the conversion to the editor's LSP types, the debounce and the paint of the popup are not measured, they are a maintainer check on a display (T10 PR body).

| Date | Commit | OS | Worst p95 (ns) | Notes |
| ---- | ------ | -- | -------------- | ----- |
| 2026-10-20 | 66f541f plus this PR | macOS arm64, Apple M4 Pro | 274,375 (`from_prefix`) | 2,000 samples per scenario; budget p95 16 ms; about 58 times under it |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 282,042 (`from_prefix`) | fresh, run 1 of 3; 2,000 samples per scenario; about 57 times under the budget |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 254,125 (`from_prefix`) | fresh, run 2 of 3 |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 269,750 (`from_prefix`) | fresh, run 3 of 3 |

p50 / p95 per scenario (ns): `statement_start` (empty statement) 8,084 / 10,709; `from_prefix` (`SELECT * FROM t_04|`) 231,708 / 274,375; `qualified_column` (`SELECT u.| FROM t_00010 u`) 4,875 / 5,042; `alias_join` (column behind the alias of a join) 5,583 / 5,708; `hover` (column with a foreign key) 4,166 / 4,209; `diagnose` (five statements, one unknown column) 125,625 / 128,375.

Run 1 at `e98f4c8`, p50 / p95 per scenario (ns): `statement_start` 9,083 / 10,916; `from_prefix` 236,417 / 282,042; `qualified_column` 4,875 / 5,250; `alias_join` 5,584 / 5,708; `hover` 4,167 / 4,250; `diagnose` 125,958 / 129,083. The DoD figure for the editor end to end is the worst of three runs, 282,042 ns (0.28 ms).

The popup the editor shows most often (a column behind an alias) takes microseconds; the slowest case is a table-name prefix, which scans and ranks the 5,000 relation names. Building the `CatalogSchema` per request is cheap: it holds `Arc`s of the cached listings and fills the per-table state only for the tables the engine looks at.

Re-run at `6ed581c` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench editor_intellisense`, one run, exit 0, 2026-10-22), p50 / p95 per scenario (ns): `statement_start` 8,292 / 11,708; `from_prefix` 239,208 / 276,166; `qualified_column` 4,959 / 5,084; `alias_join` 5,625 / 5,750; `hover` 4,250 / 4,375; `diagnose` 126,667 / 132,000; worst p95 276,166 (`from_prefix`), about 58 times under the 16 ms budget.

### Method

1. `cargo bench -p sqler-ui --bench editor_intellisense` (release profile). Flags: `--tables` (5000), `--columns` (8), `--samples` (2000 per scenario), `--budget-p95-ns` (16000000). Not part of `bench.yml`.
2. Six scenarios as listed above. The run fails if the qualified scenario does not list the loaded columns or the hover finds nothing.
3. Output is `key=value` lines: `<scenario>_p50_ns` and `<scenario>_p95_ns`, and `worst_p95_ns`, the largest p95. p50 and p95 are nearest-rank percentiles. Exit code 0 passes, 1 when the worst p95 exceeds the budget or the setup fails, 2 is a usage error.

## Export writers

`cargo bench -p sqler-export --bench export_throughput -- [--rows N --batch N --format ID]` streams synthetic rows (5 columns: integer, text, float, null, timestamp; batches of 1,000) into a sink that discards the bytes, for every format the enabled features compile, and prints `<id>_rows_per_sec=` and `<id>_mib_per_sec=`. Row generation is included. `--format` runs one format, for a per-format peak RSS. Not in `bench.yml`. Decisions: [ADR-0029](adr/0029-parquet-and-xlsx-writers.md).

Measured on 2026-10-21 with `rustc 1.99.1`, release profile, `--locked`. macOS: Apple M4 Pro, arm64. Linux: Docker `rust:1.99.1` on the same arm64 host (`aarch64`).

| Format | OS | Rows | Rows/s | MiB/s | Peak RSS (MB) |
| ------ | -- | ---- | ------ | ----- | ------------- |
| `parquet` (Snappy) | macOS | 100,000 | 3.19 M | 55.9 | 27.6 |
| `parquet` (Snappy) | macOS | 1,000,000 | 4.38 M | 76.3 | 34.1 |
| `xlsx` | macOS | 100,000 | 0.76 M | 18.8 | 3.3 |
| `xlsx` | macOS | 1,000,000 | 0.55 M | 13.6 | 3.3 |
| `parquet` (Snappy) | Linux aarch64 | 100,000 | 4.37 M | 76.6 | 15.3 |
| `parquet` (Snappy) | Linux aarch64 | 1,000,000 | 5.31 M | 92.3 | 16.5 |
| `xlsx` | Linux aarch64 | 100,000 | 0.83 M | 20.4 | 2.8 |
| `xlsx` | Linux aarch64 | 1,000,000 | 0.77 M | 19.1 | 2.8 |

(Parquet and `xlsx` rows come from the single-format builds; the `csv`, `json`, `sql` and Markdown figures and the build with both formats are in ADR-0029.) Peak RSS at 100,000 and at 1,000,000 rows is the same within the size of one row group, which is the point: memory does not depend on the row count. The run-to-run noise is about 10 %.

Size cost of the formats, bench binary, release profile with T14's `strip = "symbols"` and `codegen-units = 1` (so the binaries are already stripped), bytes:

| Features | macOS | Linux aarch64 |
| -------- | ----- | ------------- |
| none | 497,488 | 529,240 |
| `xlsx` | 598,544 | 594,776 |
| `parquet` | 1,157,536 | 1,184,624 |
| both | 1,241,856 | 1,315,696 |

(Before T14, with symbols kept and 16 codegen units: macOS 632,464 / 784,912 / 1,527,648 / 1,645,360, Linux 738,064 / 909,088 / 1,655,392 / 1,823,928.)

`sqler-app` on macOS, `cargo build --release --locked -p sqler-app`: 44,362,448 bytes before T3, 44,362,448 with the default (`export-xlsx`), 44,376,272 with `export-parquet`. The delta is about 0 until the UI links the registry (T9). Clean `cargo build --release -p sqler-export --lib` on macOS: none 2.3 s, `xlsx` 2.6 s, `parquet` 7.0 s, both 7.2 s.

Memory proof at 1M rows (`tests/streaming_memory.rs`, `cargo test --release`; RSS after batch 1,000 minus RSS after batch 100): Parquet +5.8 MB on macOS and +2.2 MB on Linux, `XLSX` +0 on both; the test allows +12 MiB.

Re-run at `ec3057c` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-export --bench export_throughput`, default 1,000,000 rows and all formats of the default build, one run, exit 0, load average 1.9): `csv` 3,731,816 rows/s and 225.4 MiB/s, `json` 5,479,264 and 568.7, `sql` 3,780,677 and 304.8, `markdown` 4,073,660 and 288.8, `parquet` 6,270,505 and 109.1, `xlsx` 811,937 and 20.1. The Parquet and `xlsx` figures are higher than the 2026-10-21 table above (4.38 M rows/s and 76.3 MiB/s, 0.55 M and 13.6 at 1,000,000 rows), which was built before T14 and measured on a different load; the older table has no `csv`, `json`, `sql` or Markdown rows to compare. The bench has no budget; no regression.

Re-run at `248e740` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-export --bench export_throughput`, one run, exit 0): `csv` 3,891,642 rows/s and 235.1 MiB/s, `json` 5,420,478 and 562.6, `sql` 3,752,680 and 302.5, `markdown` 3,953,794 and 280.3, `parquet` 6,054,855 and 105.4, `xlsx` 793,682 and 19.6. All within about 4 % of `ec3057c`.

## Grid export

`cargo bench -p sqler-ui --bench grid_export -- [--rows N --format ID --source loaded|fresh]` (T9, [ADR-0032](adr/0032-grid-export.md)) measures the export pipeline of the grid: producer, bounded feed, `write_feed` on a blocking thread and the writer, into a hidden partial file renamed on success. `loaded` copies the rows out of a bounded `GridModel` with `LoadedCursor`; `fresh` streams a `SQLite` recursive CTE through `fresh_rows`. It prints `<source>_<id>_rows_per_sec=`. Not in `bench.yml` and not gated.

Measured on 2026-10-21 on macOS 27.0.1 arm64 (Apple M4 Pro), release (`bench` profile), 1,000,000 rows x 6 columns (second run, by the tester):

| Source | Format | Rows/s | Time | File |
| ------ | ------ | ------ | ---- | ---- |
| loaded | `csv` | 4.34 M | 230 ms | 49.8 MB |
| loaded | `json` | 3.65 M | | |
| loaded | `parquet` | 3.32 M | 301 ms | 31.5 MB |
| loaded | `xlsx` | 0.41 M | 2.5 s | |
| fresh | `csv` | 0.86 M | | |
| fresh | `parquet` | 0.66 M | | |

Memory: the feed holds at most 4 batches; the unit test `memory_does_not_depend_on_the_row_count` checks at most `FEED_CAPACITY + 2` batches of 1,000 rows in flight over 1000 x 1000 rows. `grid_viewport` was not re-run (no grid code changed).

Re-run at `ec3057c` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench grid_export -- --source <loaded|fresh> --format <id>`, 1,000,000 rows x 6 columns, one run each, exit 0, load average 3.7): `loaded` `csv` 5,544,651 rows/s (180 ms, 49,777,801 bytes; 4.34 M at PR #51), `json` 8,181,286 (122 ms, 85,777,785 bytes; 3.65 M), `parquet` 3,660,537 (273 ms, 31,459,768 bytes; 3.32 M), `xlsx` 383,706 (2,606 ms, 29,324,281 bytes; 0.41 M); `fresh` `csv` 971,118 (1,029 ms, 42,277,801 bytes; 0.86 M) and `parquet` 732,252 (1,365 ms, 31,500,342 bytes; 0.66 M). The default call (`loaded` `csv`) gave 5,627,531 rows/s in 177 ms. All are at or above the PR #51 figures except `loaded` `xlsx`, 6 % lower and inside the 25 % rule. `grid_viewport` was re-run in the section above.

Re-run at `248e740` (macOS 27.0.1 arm64, Apple M4 Pro, `cargo bench -p sqler-ui --bench grid_export`, `loaded` `csv` only, one run, exit 0): 5,229,639 rows/s (191 ms, 49,777,801 bytes), against 5,627,531 rows/s (177 ms) at `ec3057c`: -7 %, one run each; the other formats were not re-run.

### Parquet in the app default (T9)

`cargo build --release -p sqler-app --locked` in Docker `rust:1.99.1` on the arm64 host (Linux `aarch64`, apt packages of AGENTS.md section 3, profile `strip = "symbols"`, `codegen-units = 1`), on the T9 tree. Rule: `export-parquet` goes into the default iff the Linux binary with it is at most 61,865,984 bytes (62,914,560 minus 1 MiB).

| Build | `sqler` bytes | MiB | Build time |
| ----- | ------------- | --- | ---------- |
| without Parquet (`--no-default-features --features driver-sqlite,driver-postgres,driver-mysql,export-xlsx`) | 46,626,808 | 44.5 | 1 min 54 s (cold, 14 CPUs) |
| with Parquet (default + `export-parquet`) | 47,282,168 | 45.1 | 28.7 s (incremental) |
| delta | 655,360 | 0.63 | |

Same two builds for Linux x86_64 (Docker `--platform linux/amd64`, emulated on the arm64 host; 5 min 43 s cold without Parquet, 56 s incremental with):

| Build (x86_64) | `sqler` bytes | MiB |
| -------------- | ------------- | --- |
| without Parquet | 54,165,824 | 51.7 |
| with Parquet | 54,947,264 | 52.4 |
| delta | 781,440 | 0.75 |

With Parquet the x86_64 binary is 6,918,720 bytes under the 61,865,984 limit. Authoritative: the `Binary size` run (`size.yml`) on the T9 pull request. `main` before T9 reported 53,703,760 bytes for x86_64 without Parquet. If that run shows more than 61,865,984 bytes, revert the `export-parquet` entry in the `sqler-app` default.

## Results grid copy

`cargo bench -p sqler-ui --bench grid_copy -- [--rows N --columns N --reps N --budget-slice-ms B]` (T8, PR #49) measures copy-as on a 100,000-row x 10-column `GridModel` (1,000,000 cells): `gather_slice_p50_ns` and `gather_slice_p95_ns` are one `CopyJob::gather` call over `GATHER_SLICE_CELLS` (50,000) cells, which is the work the UI thread does per turn, and `render_ms_tsv` and `render_ms_insert` are `CopyJob::render` of the whole selection, which runs on the background executor. Budget: the gather slice p95 at most 8 ms (`--budget-slice-ms`, default 8). Exit codes: 0 ok, 1 the slice p95 is over the budget, 2 bad arguments or a failed copy. Not in `bench.yml`.

| Date | Commit | OS | Rows x columns | Slices | `gather_slice_p50_ns` | `gather_slice_p95_ns` | Budget | TSV render | INSERT render |
| ---- | ------ | -- | -------------- | ------ | --------------------- | --------------------- | ------ | ---------- | ------------- |
| 2026-10-21 | ec3057c | macOS 27.0.1 arm64, Apple M4 Pro | 100,000 x 10 | 60 | 397,500 | 490,250 (6.1 % of the budget) | 8 ms | 27.6 ms, 7,291,671 bytes | 19.0 ms, 9,184,672 bytes |
| 2026-10-21 | 248e740 | macOS 27.0.1 arm64, Apple M4 Pro | 100,000 x 10 | 60 | 393,166 | 555,042 (6.9 % of the budget) | 8 ms | 28.3 ms, 7,291,671 bytes | 18.8 ms, 9,184,672 bytes |

One run, exit 0, load average 1.9. The slice is 16 times under the budget on this machine, so a machine 10 times slower still keeps a copy from blocking a frame. There was no earlier figure to compare with.

## UI framework spike

Data, not pass/fail; spike build, not the product. Measured on `spikes/ui-framework` (ADR-0001), release profile
with `--locked`, unstripped, with 1M synthetic rows and the SQL editor open and no database
connections. Idle RSS here is not the "three connections" metric of the targets above. CI rows come
from [run 39418467982](https://github.com/alwatroos/sqler/actions/runs/39418467982) and use software
renderers or VM Metal; first frame varies a lot between runs on shared VMs.

| Date | Commit | Target | How obtained | Cold start (ms) | First frame (ms) | RSS idle, 1M rows (MB) | Binary size (MB) | Renderer | Notes |
| ---- | ------ | ------ | ------------ | --------------- | ---------------- | ---------------------- | ---------------- | -------- | ----- |
| 2026-10-20 | 4d3d5d1 | macOS 27.0.1 arm64, Apple M4 Pro | manual | 157.8 | 191.7 | 151.4 | 26.4 | metal | mean of 3 `measure.sh` runs; cold start is hyperfine mean (20 runs) |
| 2026-10-20 | 3c59a65 | macOS (macos-latest, arm64 VM) | CI | 203.8 | 933.9 | 124.2 | 26.4 | metal | first run: cold 494.7, first frame 2981.6 |
| 2026-10-20 | 3c59a65 | Linux X11 (ubuntu-latest, xvfb) | CI | 200.7 | 515.0 | 264.3 | 57.7 | llvmpipe | 4 vCPU VM, software Vulkan; first run: first frame 1853.5 |
| 2026-10-20 | 3c59a65 | Linux Wayland (ubuntu-latest, cage) | CI | 181.9 | 2285.1 | 267.5 | 57.7 | llvmpipe | software Vulkan |
| 2026-10-20 | 3c59a65 | Windows (windows-latest) | CI | 206.0 | 185.0 | 103.2 | 26.3 | WARP | software D3D |

Binary size on Linux is 57.7 MB, close to the 60 MB target, unstripped and with the default release profile.
Watch it when the real UI lands.

### Method

`first_frame_ms` is `main()` start to the first presented frame, without exec and loader time. Cold
start is the `hyperfine` mean of the whole process (`--exit-after-first-frame`, 3 warm-ups, 20 runs).
The procedure is `spikes/ui-framework/scripts/measure.sh` (Linux, macOS) and
`spikes/ui-framework/scripts/measure.ps1` (Windows); CI runs it in `.github/workflows/spike.yml`.

## Plugins in the app (P7)

Measured for [ADR-0060](adr/0060-plugins-in-the-workbench.md) on macOS 27.0.1 arm64 (Apple M4 Pro), local release builds of the uncommitted P7 tree, `--locked`; Linux and Windows sizes come from the `Binary size` workflow of the pull request (no GUI runner for cold start).

| Build | Size (bytes) | Cold start, hyperfine mean of 20 (ms), three rounds |
| ----- | ------------ | ------------------------------------------------------ |
| default | 38,186,592 | 119.1, 122.3, 115.8 (sigma 2.3 to 9.3) |
| `--features plugins` | 49,272,272 (+11,085,680) | 117.6, 123.0, 117.5 (sigma 2.1 to 4.3) |
| `--features plugins`, `--plugin-dir` (a staged 143 KB component, not approved) | same binary | 123.3, 117.3, 117.7 (sigma 2.2 to 3.4) |

Method: `hyperfine -N -w 3 -r 20 "<binary> --exit-after-first-frame"` with `HOME` pointing at a scratch directory whose configuration folder holds `plugins/smoke` (the staged `smoke` fixture); the three commands in one hyperfine call, the order rotated between the rounds. The differences are inside the noise, in both directions: the plugins build adds only the construction of the grant book and an idle backend before the first frame, and the first scan (`refresh`) is requested after it. The 143 KB component of `--plugin-dir` is read and hashed by that scan, never compiled, because it is not approved. Linux and Windows cold start: not measured.
