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
2. Binary size: `stat -f %z target/release/sqler` (bytes), MB = bytes / 1048576 (30,928,816 bytes = 29.5 MB at `1f206e6`).
3. Isolate the config: on macOS set `HOME` to an empty directory that holds `Library/Application Support/SQLer/connections.toml` with three `sqlite` `:memory:` entries (on Linux set `XDG_CONFIG_HOME`).
4. Cold start: `hyperfine -w 3 -r 20 'target/release/sqler --exit-after-first-frame'`. This is the whole process until the first frame is presented, so it needs a display. Without one, record "not measured: no interactive display".
5. Idle RSS: start `target/release/sqler`, wait 6 s, read `ps -o rss= -p <pid>` (KiB), read again after 10 s, stop it. Repeat three times.
6. Test count: `cargo nextest run --workspace --no-tests=pass`.

M2 note (ba24c9a): the binary grew from 29.5 to 38.3 MiB with the `PostgreSQL` and `MySQL` drivers, the tunnel and the keychain; the cold start rose from 140.6 to 155.1 ms and the idle RSS from 98.1 to 100.0 MiB. The three connections of the M2 row were a `SQLite` `:memory:`, a `PostgreSQL` and a `MySQL` entry, with both containers running. The app has no headless way to open a session (a session opens by double-click or from the connection manager, and `--exit-after-first-frame` exits before any), so the RSS with three sessions actually open was not measured. The figure that was measured is the idle app with three saved connections; the sessions, the tunnel and the catalog cache add to it.

M3 note (e98f4c8): measured fresh on the merged M3 `main` with the same method and `HOME` setup. The binary grew from 38.3 to 42.3 MiB (+4.0 MiB, 44,319,056 bytes) with the SQL text services, the editor, completion and the RSA backends; it stays under the 60 MB target on macOS. The cold start fell from 155.1 to 113.7 ms and the idle RSS from 100.0 to 72.0 MiB (71.9 to 72.1 over three runs). Nothing merged in M3 should make the app faster or lighter at idle, so read these two as the state of the machine and the display during the run, not as an improvement: the figures are measured and unexplained, and the maintainer should repeat them on a normal desktop session. The three connections are the M2 ones (`SQLite` `:memory:`, `PostgreSQL`, `MySQL`), configured and not opened, for the same reason as in M2. The test count rose from 814 to 1595 run (1595 passed, 2 skipped). **Editor keystroke latency** is defined here as the headless work per keystroke: the `editor_intellisense` worst p95, plus `bracket_build_p95_ns` of `editor_buffer` when the text changes next to a bracket. At `e98f4c8` the worst of three runs gives 282,042 ns + 1,349,541 ns = 1.63 ms (the bracket build was 0.49 to 0.51 ms in the other two runs, so 0.74 to 0.79 ms), against the 16 ms frame budget. The input-to-paint latency of the widget (key press to the pixels of the popup, typing and scrolling speed on the 10,000-line script) is not measured: it needs a probe in a real window and is a maintainer check. Coverage is not measured because `cargo llvm-cov` is not installed. Linux and Windows desktop numbers are not measured (no GUI runner in `ci.yml`).

Caveat for idle RSS: an agent cannot double-click, so the three connections are listed in the sidebar but their sessions were not opened. Three `SQLite` `:memory:` sessions add little, but the figure is not the full "three connections open" metric until the maintainer repeats it with all three opened. The M1 rows are macOS only; Linux and Windows desktop numbers are not measured because `ci.yml` has no GUI runner. The spike table below has CI cold-start and idle RSS for those OSes on the spike build, not on `sqler`.

## Results grid model

Viewport fetch from `sqler_ui::grid::GridModel` holding 1M synthetic rows (6 columns: integer, text, float, boolean, integer, text). Each sample is one `viewport()` call plus a walk over every cell of the 50 visible rows.

| Date | Commit | OS | Rows | Viewport | p50 (ns) | p95 (ns) | Ingest (ms) | Heap estimate (MB) | RSS delta (MB) | Notes |
| ---- | ------ | -- | ---- | -------- | -------- | -------- | ----------- | ------------------ | -------------- | ----- |
| 2026-10-20 | 1dfec02 plus this PR | macOS 27.0.1 arm64 | 1,000,000 | 50 | 208 | 334 | 133 | 235.0 | n/a (Linux only) | `cargo bench -p sqler-ui --bench grid_viewport`, 100,000 samples, batch 1024; peak RSS of the bench process 242.7 MiB (`/usr/bin/time -l`, includes the runtime and the ingestion thread) |

Re-run at `e98f4c8` (macOS 27.0.1 arm64, Apple M4 Pro): unchanged within the gate (p50 125 ns, p95 167 ns, ingest 153 ms, heap estimate 246,433,024 bytes = 235.0 MiB; the p95 limit of the gate is 25 % plus 100 ns).

### Method

Run from the repository root on the commit being recorded:

1. `cargo bench -p sqler-ui --bench grid_viewport` (release profile). Flags: `--rows` (1000000), `--columns` (6), `--batch` (1024), `--viewport` (50), `--samples` (100000), `--reps` (3), `--save PATH`, `--compare PATH`, `--threshold-pct` (25), `--slack-ns` (100), `--budget-p95-ns` (1000000).
2. The bench ingests the rows through `spawn_ingest(SyntheticSource)` while the main thread polls, then draws 100,000 viewport positions from a fixed-seed xorshift64 generator and times each fetch with `Instant`. p50 and p95 are nearest-rank percentiles.
3. Memory: `heap_bytes` is the model's own estimate (capacity of the batches, row vectors and strings; MB = bytes / 1048576). On Linux the bench also prints `rss_delta_bytes` (`VmRSS` after ingestion minus before). On macOS, record the peak RSS with `/usr/bin/time -l <bench binary>`. A counting allocator would need `unsafe`, which the workspace forbids.
4. Each sample includes about 30 ns of `Instant` overhead.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job. The whole measurement runs `--reps 3` times and the reported p50 and p95 are the minimum over the repetitions (noise on a shared runner only adds time), on the base and on the PR alike. It fails if the PR p95 is more than 25 % plus 100 ns above the base p95, or if the p95 is above 1 ms (`--budget-p95-ns 1000000`). The 100 ns slack is about two timer ticks (one `Instant` pair costs about 41 ns); a 2x slowdown of the 300 ns p95 still fails (checked with `--viewport 200` against a baseline taken with the default 50: p95 584 ns against 292 ns, limit 465 ns). Without a baseline on the base branch only the 1 ms budget applies.

## Results grid view

Data, not pass/fail. The grid tab (T8) draws a `gpui-component` virtual table over the 1M-row model above. Two measurements: a headless frame data cost (CPU only, no GPU) and a probe in the real window.

Frame data cost: `cargo run --release -p sqler-ui --example grid_scroll`. It scrolls 1,000,000 rows from top to bottom in 96 px steps, 32 px rows and a 760 px viewport, and times the cell text of every visible cell per simulated frame.

| Date | Commit | OS | Rows | Max rows per frame (cap) | Frame data cost p50 / p95 / max (µs) | `jump_to_end` (µs) | Heap (MB) | Notes |
| ---- | ------ | -- | ---- | ------------------------ | ------------------------------------ | ------------------ | --------- | ----- |
| 2026-10-20 | 23a3f72 | macOS 27.0.1 arm64, Apple M4 Pro | 1,000,000 | 28 (29) | 9.4 / 9.8 / 40.8 (second run: 9.6 / 10.1 / 63.2) | 9.0 | 235.0 | 333,327 frames; ingest 133 to 158 ms; two runs |

In-app probe: `SQLER_GRID_PROBE=1` on `sqler`, with a `connections.toml` that holds one SQLite `:memory:` entry. It opens the synthetic grid, waits for the end of loading, scrolls to 600 evenly spaced rows (one per presented frame), then to the last row, and prints `SQLER grid_*` lines. RSS is read with `ps` (macOS) or `VmRSS` (Linux). Peak RSS comes from `/usr/bin/time -l`.

| Date | Commit | OS | Rows | Ingest (ms) | Frames | Frame interval p50 / p95 / max (ms) | Max rows drawn per frame | Last row drawn | RSS (MB) | Peak RSS (MB) |
| ---- | ------ | -- | ---- | ----------- | ------ | ----------------------------------- | ------------------------ | -------------- | -------- | ------------- |
| 2026-10-20 | 23a3f72 | macOS 27.0.1 arm64, Apple M4 Pro | 1,000,000 | 290.6 | 601 | 8.3 / 8.3 / 9.3 | 24 | yes | 346.5 | 346.6 |
| 2026-10-20 | 23a3f72 | macOS 27.0.1 arm64, Apple M4 Pro | 1,000,000 | 293.0 | 601 | 8.3 / 8.9 / 12.0 | 24 | yes | 346.5 | 346.6 |
| Linux X11, Wayland | | | | | | not measured: no GUI runner in `ci.yml` | | | | |
| Windows | | | | | | not measured: no GUI runner in `ci.yml` | | | | |

The frame interval is bound by the display (8.3 ms is a 120 Hz screen), so it shows responsiveness, not data cost. RSS with 1,000,000 rows open is above the 150 MB idle target, which is defined for an idle app with three connections and no large result, so it is recorded and not gated. Each open synthetic grid holds about 235 MB of result data (see the model bench above). The maintainer's interactive check (scrolling by hand) is still pending.

## Catalog tree

`sqler-catalog` on a synthetic 5,000-table schema (one schema `main`, 8 columns per table). Source `fake` is an in-memory `Introspect` that returns ready futures, so it measures the catalog and not a database. Source `sqlite` builds the same 5,000 tables in a `:memory:` `SQLite` session and is a reference figure only (20 samples, no budget).

| Date | Commit | OS | Source | Schemas cold p50 / p95 (ns) | Expand 5,000 tables cold p50 / p95 (ns) | Cached read p50 / p95 (ns) | Refresh schema p50 / p95 (ns) | Refresh all (ms) | Notes |
| ---- | ------ | -- | ------ | --------------------------- | --------------------------------------- | -------------------------- | ----------------------------- | ---------------- | ----- |
| 2026-10-20 | 7d80fbf plus this PR | macOS arm64 | fake | 833 / 1,042 | 203,125 / 351,167 | 41 / 42 | 441,167 / 443,959 | 4.44 (5,002 nodes) | 200 samples, 100,000 cached reads; budgets: cached read p95 10 us, expand p95 16 ms |
| 2026-10-20 | 7d80fbf plus this PR | macOS arm64 | sqlite (`:memory:`) | 8,375 / 14,250 | 2,100,500 / 2,260,834 | 41 / 42 | 2,453,750 / 2,623,625 | 552.1 (5,002 nodes) | 20 samples; reference, includes the driver and `SQLite` |

Re-run at `e98f4c8` (macOS arm64, Apple M4 Pro, source `fake`): the gated figure is unchanged within the gate (cached read p50 / p95 42 / 84 ns, one timer tick of difference, inside the 25 % + 150 ns rule). Not gated: expand cold p50 / p95 243,000 / 492,083 ns (was 203,125 / 351,167 ns at `7d80fbf`, about 40 % more at p95, budget 16 ms), schemas cold 2,375 / 6,084 ns, refresh schema 218,959 / 223,125 ns, refresh all 4.30 ms; one run. Linux CI, PR #29: cached read p95 91 ns on the base and 131 ns on the PR.

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

Budgets (fake source): snapshot p95 10 us, prefix p95 1 ms, fuzzy p95 5 ms, index size 4 MiB. Prefix p95 is about 140 us and fuzzy p95 about 157 us, so both are far inside their budgets.

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

Expand p95 is about 0.7 ms against the 16 ms budget (one frame at 60 Hz), and select p95 is 2 to 3 us against 1 ms. The rendered sidebar is a virtualized list, so the cost of drawing does not grow with the 5,000 rows; that part is checked by eye (see the PR checklist), not by this bench.

### Method

1. `cargo bench -p sqler-ui --bench browser_tree` (release profile). Flags: `--tables` (5000), `--samples` (200), `--select-samples` (20000), `--reps` (3), `--save PATH`, `--compare PATH`, `--threshold-pct` (25), `--slack-ns` (250000), `--budget-expand-p95-ns` (16000000), `--budget-select-p95-ns` (1000000).
2. `expand_*`: expand the schema, load the 5,000-table listing, rebuild the rows. `folder_p95_ns`: expand the `Tables` folder under it. `select_*`: change the selection. p50 and p95 are nearest-rank percentiles.
3. Exit code 0 passes, 1 is a regression or budget failure, 2 is a usage error.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job, and fails if `expand_p95_ns` is more than 25 % plus 250 us above the base, or if a budget is exceeded. Every metric is the minimum over `--reps 3` repetitions of the whole measurement, on the base and on the PR alike. The 250 us slack is an absolute allowance for runner noise on a millisecond-scale metric (about 0.7 ms on the reference machine, so the limit is about 1.1 ms); a 2x slowdown (about 1.35 ms) is above it and fails (checked by halving the saved baseline: expand p95 790 us against a baseline of 336 us, limit 670 us). Only `expand_p95_ns` is compared; the select p95 is guarded by its 1 ms budget.

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

## SQL editor buffer

`sqler_ui::editor` headless, on the 10,000-line `synthetic` script (541,580 bytes) that `dev:open-large-sql-script` opens ([ADR-0025](adr/0025-sql-editor-component.md)): the bracket index of the whole buffer, a bracket lookup at the cursor, the SQL formatter over the whole buffer and the search for the next occurrence of a word. It stops where GPUI starts: typing, scrolling, highlighting and the paint of the bracket frame are not measured and are a maintainer check on a display.

| Date | Commit | OS | Bracket build p95 (ns) | Bracket lookup p95 (ns) | Format p95 (ns) | Next occurrence p95 (ns) | Notes |
| ---- | ------ | -- | ---------------------- | ----------------------- | --------------- | ------------------------ | ----- |
| 2026-10-20 | d577740 (PR #36) | macOS arm64 | about 1,000,000 | not quoted | about 10,700,000 | about 52,000 | quoted from the PR body, not re-measured; 10,000 lines; budget 16 ms |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 1,349,541 | 250 | 11,016,542 | 53,791 | fresh, run 1 of 3 (bracket build p50 907,417, format p50 8,968,042); 30 samples |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 487,750 | 167 | 9,138,291 | 51,417 | fresh, run 2 of 3 (bracket build p50 478,583, format p50 8,913,875) |
| 2026-10-20 | e98f4c8 | macOS 27.0.1 arm64, Apple M4 Pro | 512,000 | 167 | 9,071,458 | 53,417 | fresh, run 3 of 3 (bracket build p50 483,416, format p50 8,858,292) |

Bracket build and format are the two budgeted figures (16 ms, one frame at 60 Hz): the worst format p95 is 11.0 ms and the worst bracket build p95 is 1.35 ms, so both are met. Format is the closest to its budget (about 31 % to spare in the worst run); it runs only on the explicit `editor:format-sql` command, not per keystroke. The bracket lookup is at timer resolution (one `Instant` pair is about 41 ns, so 167 to 250 ns is a few ticks). The first run after a build was the slow one for the bracket build (1.35 ms against 0.49 to 0.51 ms).

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

### Method

1. `cargo bench -p sqler-ui --bench editor_intellisense` (release profile). Flags: `--tables` (5000), `--columns` (8), `--samples` (2000 per scenario), `--budget-p95-ns` (16000000). Not part of `bench.yml`.
2. Six scenarios as listed above. The run fails if the qualified scenario does not list the loaded columns or the hover finds nothing.
3. Output is `key=value` lines: `<scenario>_p50_ns` and `<scenario>_p95_ns`, and `worst_p95_ns`, the largest p95. p50 and p95 are nearest-rank percentiles. Exit code 0 passes, 1 when the worst p95 exceeds the budget or the setup fails, 2 is a usage error.

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
