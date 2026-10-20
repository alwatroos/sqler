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
