# 0006 Grid data model and bench harness

- Status: accepted
- Date: 2026-10-20
- Note: storage and backpressure are superseded by [ADR-0027](0027-bounded-result-model.md) (bounded model, compact cells); the rest stands.

## Context and problem statement

The results grid must hold a result of 1M rows without blocking the UI thread and must serve only the visible window (AGENTS.md §2, §4). The model has to be independent of the UI framework (ADR-0001 is still open), fed by `sqler_driver_api::QueryStream` or by a synthetic source, cancellable, and benchmarked. The project also needs its first benchmark harness and a CI gate against regressions (AGENTS.md §4 Performance rule).

## Decision drivers

- The UI thread never waits on ingestion or on a lock held by a writer.
- Growth of a 1M-row result must not copy the whole result.
- Cancellation reuses `CancelFlag` from `sqler-driver-api`.
- No UI framework and no driver dependency in non-test code; no new runtime dependency.
- A benchmark that reports p50/p95, can compare itself with a baseline and fails with an exit code.

## Considered options

Data model:

1. `Arc<RwLock<Vec<Row>>>` shared between ingestion and UI.
2. One flat `Vec<Row>` fed by a bounded channel.
3. A channel-fed model made of chunks, polled with `try_recv`, with an unbounded channel.

Bench harness:

1. `criterion`.
2. `divan`.
3. A std-only `harness = false` binary.

## Decision outcome

Chosen: data model option 3 and harness option 3.

Model (`sqler_ui::grid`):

- `grid_channel()` returns a `GridModel` owned by the UI thread and a cloneable `GridSink`. Ingestion (`ingest`) validates the `QueryStream` protocol like `collect` and sends batches through an unbounded `std::sync::mpsc` channel.
- The UI calls `GridModel::poll(max)`, which only does `try_recv`, so it never waits. Batches are kept as chunks with start offsets; a viewport is a binary search plus a slice walk, and growth never reallocates the whole result.
- Cancellation: model and sink share one `CancelFlag`. `GridModel::cancel` sets it and switches the state to `Cancelled` at once; `ingest` checks the flag before each `next_event`. Dropping the model also stops ingestion, because the send fails. With a driver stream the caller also calls the session's `CancelHandle`.
- `spawn_ingest` runs `ingest` on a named thread with a park-based `block_on` (no unsafe). Streams that need a Tokio context, such as the sqlx drivers, run `ingest` on the driver's runtime instead.
- `GridModel::heap_bytes` estimates memory from capacities; it is O(rows) and not meant for per-frame use. A counting allocator would need `unsafe`, which is forbidden.

Harness: a std-only bench binary (`crates/sqler-ui/benches/grid_viewport.rs`, `harness = false`) prints p50/p95, ingestion time and memory as `key=value` lines. `--save` writes them to a file; `--compare` checks them against a baseline file. Exit code 1 means a regression or a blown budget.

CI gate (`.github/workflows/bench.yml`): on pull requests the same Linux job benchmarks the base commit and the PR commit and compares them. The rule is: p95 more than 25 % + 50 ns above the base, or p95 above 1 ms, fails. Running both in one job limits runner-to-runner noise; the 25 % margin and 50 ns slack absorb the remaining jitter of a shared runner, and the threshold is a flag, changed only through the documented rule in `docs/src/benchmarks.md`. Linux only, because AGENTS.md §5.3 asks for Linux benchmarks and shared macOS and Windows runners are noisier. When the base has no bench (the first PR), only the absolute budget applies.

## Consequences

- Good: the UI never blocks; no lock is shared between threads; no copy of the full result on growth.
- Good: no new dependency; the bench adds no transitive crates.
- Good: synthetic and driver rows take the same code path.
- Bad: the unbounded channel can hold a whole result in flight; accepted at the time, replaced by the bounded credit gate in [ADR-0027](0027-bounded-result-model.md).
- Bad: a hand-written harness has no statistics beyond p50/p95 and no outlier analysis.
- Bad: `bench.yml` is not a required check, so a flaky run is a signal, not a block.
- Neutral: sort, filter, pin and edit (M4) will need their own index structures on top of the chunks.

## Pros and cons of the options

- `Arc<RwLock<Vec<Row>>>`: simple, but the UI can wait on a writer, and a lock across an `.await` breaks AGENTS.md §4. Rejected.
- One flat `Vec` with a bounded channel: a growing `Vec` copies the whole result on reallocation (about 24 MB of row headers at 1M rows), and a bounded `send` blocks inside async code. Rejected.
- Chunked model with an unbounded channel: chosen.
- `criterion` 0.8.2 (2026-02-04): rich statistics, but about 30 transitive crates, no p95 and no failing exit code on regression. Rejected.
- `divan` 0.1.21 (2025-04-10): light, but no baseline comparison or p95. Rejected.
- Std-only binary: chosen, see above.
