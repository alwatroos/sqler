# 0027 Bounded result model: credit gate, fetch more and compact rows

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

ADR-0006 gave the results grid an unbounded `std::sync::mpsc` channel and `Vec<Row>` chunks. A query that returns tens of millions of rows then fills memory before the user looks at the first page, and the synthetic 1M x 6 result costs about 246 bytes per row. The result model must be bounded, the user must be able to ask for more rows, and the per-row cost must drop. This changes the public API of `sqler_ui::grid` (AGENTS.md §8 requires an ADR) and replaces the storage and backpressure parts of ADR-0006.

## Decision drivers

- A hard bound on rows held, with an explicit "fetch more" step.
- Cancel and close must always end ingestion, also when it waits for the user.
- `ingest` stays runtime-agnostic (std `Waker` only); the UI thread never waits and `poll` stays a push of ready data.
- About 123 bytes per row or less for 1M x 6 (measured figures are on the [Benchmarks](../benchmarks.md) page).

## Considered options

1. A credit gate: ingestion sends rows only against credit granted by the model; storage in compact per-batch chunks.
2. A bounded `tokio::sync::mpsc` channel, bounded by message count.
3. Keep the unbounded channel and cap the rows in the model by dropping the rest.

## Decision outcome

Chosen: option 1. Option 2 bounds batches, not rows or bytes, and "fetch more" would need the model to stop receiving (a peek and a stash). Option 3 still reads and discards the whole result.

**Bound and defaults.** A page is `page_rows` rows or `page_bytes` bytes, whichever comes first. `page_rows` is the `result_page_rows` setting in `settings.toml` (default 100,000, range 1,000 to 10,000,000, checked on load and save); `page_bytes` is fixed at 64 MiB. `ingest` applies the byte cap inside a batch: it sends rows until the packed size reaches the cap, always at least one row, and keeps the rest of the batch as the pending partial batch for the next page. The cap is therefore overshot by at most one row, whatever the batch size. There is no Settings tab control.

**Gate.** `grid_channel_with(GridLimits)` grants one page up front. The gate (`granted` and `sent` counters for rows and bytes, a `closed` flag and a `Waker`) is shared by the model, the sink and `GridCancel`, behind a mutex held only for O(1) updates, never across an await and never while a chunk is built. When credit runs out `ingest` sends `Msg::Paused` and awaits a `poll_fn` future, so the driver stream is not polled again (all three drivers are pull-based). The model shows `IngestState::Paused`, a non-terminal state meaning "page full, more rows are known to exist". An exactly full page followed by the end of the result is `Done`, never `Paused`.

**One cancel object per run.** A paused statement waits on the gate, so every cancel path must wake it. `QueryJob::stop` is a `RunStop`: one flag plus the `GridCancel` of every sink of the run, so setting `stop` alone also wakes a paused job (a plain `CancelFlag` cannot wake anything). The workspace keeps `GridCancel` (never the bare flag) for Open Data and synthetic tabs, and `Quit` cancels them all.

**Lookahead of one batch.** `ingest` may hold the one batch it already pulled when the credit ends. It splits that batch at the remaining credit and keeps the rest until the next grant. What sits in the channel is therefore at most one page plus one batch.

**Fetch more.** `GridModel::fetch_more` works only in `Paused`: it grants another page (saturating), sets `Streaming` and wakes ingestion. The command is `grid:fetch-more` ("Fetch More Rows"); the buttons are "Fetch more" in the grid toolbar and next to Cancel in the query results strip.

**Wake paths.** Only these end a paused ingest: `GridCancel::cancel` (from `GridSink::canceller`), `GridModel::cancel` and dropping the `GridModel`. Setting only the `CancelFlag` (`GridSink::cancel_flag`) does not wake it. The query workspace keeps `GridCancel` handles, so Cancel and quit end a paused job.

**Compact cells.** Each batch becomes a `Chunk` on the ingestion thread (so `poll` stays a push): one row-major `Vec<Cell>` of 16-byte cells, with text, JSON and bytes in per-chunk arenas addressed by `u32` offsets and lengths. Rare variants (decimal, dates and times, unknown variants) live in a side `Vec<Value>`. A value whose arena offset would pass `u32::MAX` also goes to that side vector, so a huge chunk degrades to the old cost per value instead of failing. Rows are read as `RowRef` and `CellRef`; nothing allocates per cell, and `CellRef` displays exactly like `Value`. `heap_bytes` stays a capacity estimate.

**Script consequence.** A paused statement holds the tab's session, so in a script the run waits there. Later statements stay `Pending` until the user fetches the rest of that result or cancels. Cancel ends the run; closing the tab drops the model, which returns the session, and the later statements are not run. This follows from one session per tab (ADR-0024).

**Unbounded users.** The synthetic dev grid (the dev probe waits for `Done`), the `grid_viewport` bench and the `grid_scroll` example use `GridLimits::UNBOUNDED`, so they still measure 1M rows. Open Data keeps its `LIMIT 1000` and pauses only if the page setting is below it.

**Gate on 6 columns.** The memory baseline (246,433,024 bytes) was measured at the bench default of 6 columns, so the memory gate compares at `--columns 6`. A run with `--columns 8` is reported for information only.

## Consequences

- Good: memory is bounded by the page setting; the user decides whether to read on.
- Good: a smaller per-row cost for the same 1M x 6 result; figures on the [Benchmarks](../benchmarks.md) page.
- Good: no change to the driver traits; cancel and close always end ingestion.
- Bad: a paused statement blocks later statements of a script and keeps the session busy until it is fetched, cancelled or closed.
- Bad: `ingest_ms` rises, because chunk building moved to the ingestion thread; it is recorded, not gated.
- Bad: the public read API changes from `&Row` to `RowRef` and `CellRef`.
- Bad: a caller that sets only the `CancelFlag` leaves a paused ingest waiting (documented in AGENTS.md §10).
