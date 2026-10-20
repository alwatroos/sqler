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

Caveat for idle RSS: an agent cannot double-click, so the three connections are listed in the sidebar but their sessions were not opened. Three `SQLite` `:memory:` sessions add little, but the figure is not the full "three connections open" metric until the maintainer repeats it with all three opened. The M1 rows are macOS only; Linux and Windows desktop numbers are not measured because `ci.yml` has no GUI runner. The spike table below has CI cold-start and idle RSS for those OSes on the spike build, not on `sqler`.

## Results grid model

Viewport fetch from `sqler_ui::grid::GridModel` holding 1M synthetic rows (6 columns: integer, text, float, boolean, integer, text). Each sample is one `viewport()` call plus a walk over every cell of the 50 visible rows.

| Date | Commit | OS | Rows | Viewport | p50 (ns) | p95 (ns) | Ingest (ms) | Heap estimate (MB) | RSS delta (MB) | Notes |
| ---- | ------ | -- | ---- | -------- | -------- | -------- | ----------- | ------------------ | -------------- | ----- |
| 2026-10-20 | 1dfec02 plus this PR | macOS 27.0.1 arm64 | 1,000,000 | 50 | 208 | 334 | 133 | 235.0 | n/a (Linux only) | `cargo bench -p sqler-ui --bench grid_viewport`, 100,000 samples, batch 1024; peak RSS of the bench process 242.7 MiB (`/usr/bin/time -l`, includes the runtime and the ingestion thread) |

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

| 2026-10-20 | T7 branch (hover and diagnostics) | macOS arm64, Apple M4 Pro | 56,125 / 386,917 | 2,065,041 (`diagnose_big_script`) | best of 3 repetitions; `hover_p95_ns` 112,083, `diagnose_p95_ns` 2,058,917; the completion scenarios are unchanged |

Per scenario p95 in run 2 (ns): `from_empty` 211,541; `from_prefix` 404,209; `from_fuzzy` 487,500; `select_list_join5` 51,958; `alias_dot` 2,292; `where_prefix` 12,583; `insert_columns` 1,958; `big_script` (2,000 statements, cursor mid-file) 121,541.

Hover and diagnose p95 on the same schema (T7 run, ns): `hover_column` (`a3.col_0` in a 5-table join) 2,625; `hover_big_script` (table name in the middle of a 2,000-statement script) 113,000; `diagnose_join5` (5-table join, one misspelt column) 139,917; `diagnose_big_script` (the 2,000-statement script plus one statement with an unknown column) 2,065,041. Diagnosing the whole script takes about 2 ms, 8 times under the budget; most of it is scanning 2,000 statements, because the relation lookups go through a per-call name index.

The worst completion p95 is about 0.5 to 1.0 ms against the 16 ms budget (one frame at 60 Hz), roughly 16 to 32 times under it. The cost is dominated by scanning the 5,000 relation names (`FROM` with a prefix or a fuzzy match); in-scope column completion, which is what the editor asks for most, takes microseconds. The Linux CI figures are recorded by the `Bench` workflow summary (`completion-pr.txt`) on the pull request and are not part of this table until that run exists.

### Method

1. `cargo bench -p sqler-completion --bench completion` (release profile). Flags: `--tables` (5000), `--columns` (8), `--samples` (2000 per scenario), `--save PATH`, `--compare PATH`, `--reps` (3), `--threshold-pct` (25), `--slack-ns` (250000), `--budget-p95-ns` (16000000).
2. Twelve scenarios, each a fresh request per sample after 20 warm-up requests. Eight time `complete`: `from_empty` (`FROM |`), `from_prefix` (`FROM table_00|`), `from_fuzzy` (`FROM t_4|`), `select_list_join5` (select list over a 5-table join), `alias_dot` (`a.|`), `where_prefix` (`WHERE col_|` over a join), `insert_columns` (`INSERT INTO t (|`) and `big_script` (a 2,000-statement script, cursor in the middle). Two time `hover`: `hover_column` (cursor on `a3.col_0` in the 5-table join) and `hover_big_script` (cursor on the table name of the middle statement of the 2,000-statement script). Two time `diagnose`: `diagnose_join5` (the 5-table join with one misspelt column) and `diagnose_big_script` (the 2,000-statement script plus one statement with an unknown column). A scenario whose completion has no items, whose hover is `None` or whose diagnose finds nothing fails the run.
3. The measurement loop (all scenarios) runs `--reps` times in process; the repetition with the lowest `complete_p95_ns` is the one reported, saved and compared. Output is `key=value` lines: `<scenario>_p50_ns` and `<scenario>_p95_ns`, `complete_p50_ns` and `complete_p95_ns` over the samples of the eight completion scenarios only (so the baseline stays comparable), `hover_p95_ns` and `diagnose_p95_ns` over the samples of their scenarios, and `worst_p95_ns`, the largest scenario p95 of all twelve. p50 and p95 are nearest-rank percentiles.
4. Exit code 0 passes, 1 is a regression or a budget failure, 2 is a usage error or an unreadable baseline.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job, and fails if `complete_p95_ns`, taken as the minimum over 3 in-process repetitions, is more than 25 % plus 250 us (`--slack-ns 250000`, 1/64 of the budget) above the same figure on the base, or if `complete_p95_ns`, `hover_p95_ns`, `diagnose_p95_ns` or `worst_p95_ns` exceeds the 16 ms budget. `hover_p95_ns` and `diagnose_p95_ns` are compared the same way when the base's file has them (a base from before T7 does not). Without a baseline on the base branch only the budget applies. The p95 of this bench is in the hundreds of microseconds and sits in the tail of the relation-scan scenarios, so the fixed 50 ns slack of the ns-scale benches would flake on a shared runner; the minimum over repetitions drops a noisy repetition and the 250 us floor keeps a real 2x regression (about +450 us on the M4, more on a runner) failing. If the gate still flakes, report it here rather than widening the default silently.

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
