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

## Method

Run from the repository root on the commit being recorded:

1. `cargo build --release -p sqler-cli`
2. Binary size: `stat -f %z target/release/sqler-cli` (bytes), MB = bytes / 1048576.
3. Cold start: 3 warm-ups, then 20 runs of `target/release/sqler-cli run :memory: "select 1"`; report the mean. Use `hyperfine` when installed; otherwise time each run with a wall clock (`/usr/bin/time -p` is too coarse, it has 10 ms resolution).
4. CLI peak RSS: `/usr/bin/time -l target/release/sqler-cli run :memory: "select 1"`, "maximum resident set size" (bytes on macOS).
5. Test count: `cargo nextest run --workspace --no-tests=pass`.

Idle RSS with three connections needs the UI and is recorded from M1.

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
