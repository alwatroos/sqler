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

Caveat for idle RSS: an agent cannot double-click, so the three connections are listed in the sidebar but their sessions were not opened. Three `SQLite` `:memory:` sessions add little, but the figure is not the full "three connections open" metric until the maintainer repeats it with all three opened. The M1 rows are macOS only; Linux and Windows desktop numbers are not measured because `ci.yml` has no GUI runner. The spike table below has CI cold-start and idle RSS for those OSes on the spike build, not on `sqler`.

## Results grid model

Viewport fetch from `sqler_ui::grid::GridModel` holding 1M synthetic rows (6 columns: integer, text, float, boolean, integer, text). Each sample is one `viewport()` call plus a walk over every cell of the 50 visible rows.

| Date | Commit | OS | Rows | Viewport | p50 (ns) | p95 (ns) | Ingest (ms) | Heap estimate (MB) | RSS delta (MB) | Notes |
| ---- | ------ | -- | ---- | -------- | -------- | -------- | ----------- | ------------------ | -------------- | ----- |
| 2026-10-20 | 1dfec02 plus this PR | macOS 27.0.1 arm64 | 1,000,000 | 50 | 208 | 334 | 133 | 235.0 | n/a (Linux only) | `cargo bench -p sqler-ui --bench grid_viewport`, 100,000 samples, batch 1024; peak RSS of the bench process 242.7 MiB (`/usr/bin/time -l`, includes the runtime and the ingestion thread) |

### Method

Run from the repository root on the commit being recorded:

1. `cargo bench -p sqler-ui --bench grid_viewport` (release profile). Flags: `--rows` (1000000), `--columns` (6), `--batch` (1024), `--viewport` (50), `--samples` (100000), `--save PATH`, `--compare PATH`, `--threshold-pct` (25), `--slack-ns` (50), `--budget-p95-ns` (1000000).
2. The bench ingests the rows through `spawn_ingest(SyntheticSource)` while the main thread polls, then draws 100,000 viewport positions from a fixed-seed xorshift64 generator and times each fetch with `Instant`. p50 and p95 are nearest-rank percentiles.
3. Memory: `heap_bytes` is the model's own estimate (capacity of the batches, row vectors and strings; MB = bytes / 1048576). On Linux the bench also prints `rss_delta_bytes` (`VmRSS` after ingestion minus before). On macOS, record the peak RSS with `/usr/bin/time -l <bench binary>`. A counting allocator would need `unsafe`, which the workspace forbids.
4. Each sample includes about 30 ns of `Instant` overhead.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job. It fails if the PR p95 is more than 25 % plus 50 ns above the base p95, or if the p95 is above 1 ms (`--budget-p95-ns 1000000`). Without a baseline on the base branch only the 1 ms budget applies.

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

- The cached read is at timer resolution. One `Instant` pair costs about 41 ns on this machine, so 41 / 42 ns says the read is below one tick, not that it costs 42 ns. Its p95 is not meaningful beyond "below one tick". The 10 us budget and the `--compare` rule (25 % + 50 ns slack) still catch a real regression, such as a read that reaches the worker.
- The bench-regression guard is weak for the cached read: its p95 sits at timer resolution, so the `--compare` rule can only catch a large jump (the absolute 10 us budget is the real guard), not a small slowdown.
- Cold figures vary between runs (expand p50 was 137 to 203 us over several runs on the same machine).
- Refresh all re-fetches 5,002 cached nodes (the schema list, the 5,000-table listing and every `table()`), then applies them in one write.

### Method

Run from the repository root on the commit being recorded:

1. `cargo bench -p sqler-catalog --bench catalog_tree` (release profile). Flags: `--tables` (5000), `--columns` (8), `--samples` (200), `--hit-samples` (100000), `--source fake|sqlite` (fake), `--save PATH`, `--compare PATH`, `--threshold-pct` (25), `--slack-ns` (50), `--budget-hit-p95-ns` (10000), `--budget-expand-p95-ns` (16000000).
2. `schemas_cold_*`: a new catalog per sample, then `children` for the schema list. `expand_cold_*`: `children` for the N tables of `main`, cold. `hit_*`: `cached()` on that listing. `refresh_schema_*`: `refresh(Subtree(main))` with the list cached. `refresh_all_ms`: `refresh(All)` after `table()` was loaded for every table. p50 and p95 are nearest-rank percentiles.
3. Exit code 0 passes, 1 is a regression or budget failure, 2 is a usage error. Budgets apply to `fake` only.

### Regression rule

`bench.yml` runs the bench on Linux for each pull request, on the base commit and on the PR in the same job, and fails if `hit_p95_ns` is more than 25 % plus 50 ns above the base, or if a budget is exceeded. Without a baseline on the base branch only the budgets apply.

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
