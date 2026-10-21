# 0031 Client-side sort and filter: published display order, resumable job on the UI thread

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

The results grid (T5, T8) shows rows in arrival order. The user needs to sort by a column and filter rows, on results of up to millions of rows, without freezing the window and without copying the result. The model stores cells compactly (ADR-0027: 16-byte cells, per-chunk arenas) and only appends rows. This ADR adds sort and filter on the rows the model already holds. It adds public API to `sqler_ui::grid` and changes the `GridControls` signatures, so AGENTS.md section 8 requires an ADR.

## Decision drivers

- Sorting 1,000,000 rows of text must finish in 1.5 s, a quick filter in 500 ms, and a viewport of the sorted view must cost no more than 1 ms.
- The UI thread should not be held longer than a frame (16 ms) by one step of this work.
- No copy of the result and no change to the storage of ADR-0027.
- A total order over every cell type, so that sort results are deterministic and the standard sorts cannot panic.
- Selection, scrolling and rendering must follow rows through one abstraction (`RowOrder`, T5).
- Testable headless.

## Considered options

1. Loaded rows, client-side, in slices on the UI thread (chosen): a published display order of model-row indices, built by a resumable job of about 6 ms per step.
2. A worker thread that sorts a copy of the keys. It needs owned keys (a copy of every text value), which costs memory comparable to the result itself.
3. A worker thread over an `Arc` snapshot of the chunks. It changes the storage of ADR-0027 (chunks would have to be shared and immutable).
4. Server re-query (`ORDER BY` / `WHERE` added to the statement). It is not in M4: it needs dialect-specific SQL rewriting of arbitrary statements, a new round trip, and loses the paged result and its "fetch more" state. It is planned for a later milestone and does not exclude this option, which would still be needed for results that are not complete.

## Decision outcome

Chosen: option 1. Options 2 and 3 trade a copy or a storage change for a thread; option 1 needs neither and stays within the budgets (see below). Option 4 changes what the result is.

**Display order.** `GridOrder`, owned by `GridTab`, holds a published `order: Vec<u32>` (model-row indices in display order) and an inverse `Vec<u32>` (`u32::MAX` = hidden). `DisplayRows` implements `RowOrder`, so `GridControls`, `Selection` and rendering map display to model rows and back through it. Selection is stored in model rows. When no sort and no filter is set, `rows()` is the identity and the caches are freed. The model only appends, so published indices stay valid across fetch-more. `GridTab::cell_text(row, col)` stays model-indexed.

**Resumable job.** `GridOrder::step(&model, Some(budget))` does at most about `budget` of work and says where it is (`OrderProgress`); `step(None)` runs to the end. The view's pump calls it with 6 ms and yields 1 ms between calls. Phases: scan rows in sequence through `model.viewport` (filter, then extend the key caches), sort runs of kept rows with `sort_unstable_by`, merge the runs bottom-up (and with the published order, for extend jobs), build the inverse, publish. The clock is checked after every unit (1,024 rows scanned, one sorted run, 1,024 elements merged or 4,096 inverse entries), and at least one unit is always done. A run is not interruptible, so its size follows the measured speed: the first run has 1,024 rows, the next ones are sized to take about 2 ms at the speed just measured (a run of n rows costs about n log2 n comparisons), at most double the last one, between 256 and 16,384 rows. That bounds a step on the slow path too (cells compared through the model), where a fixed 16,384-row run could take several frames. The merge passes join runs of any lengths. Each `set_sort`/`set_filter`/`set_collation` drops the running job (that is the cancel) and starts a full one; the previous order stays on screen until the new one is published. An equal spec is a no-op. Filter-only jobs append in model order.

**Rows that arrive.** With no job and more rows in the model than `covered`, `step` starts an extend job when the new rows number at least max(4,096, covered / 8), or the model state is not `Streaming`, or the order was last published at least 500 ms ago (so a slow trickle does not look frozen): the new rows are scanned, sorted and merged into the published order. The coalescing keeps the total work O(n log n) over a whole load. Content under a sorted viewport moves while rows load; this is accepted and documented. While the first job runs nothing is published and rows show in model order. A changed column set (`columns_changed`) resets the order.

**Total order (`compare_cells`).** Ascending order of non-NULL values; NULL is greater than every value, and the key comparator applies the direction only to non-NULL pairs, so NULLs come last in both directions.

- Class ranks: Bool < Number (Int, Float, Decimal) < Date/Timestamp < TimestampTz < Time < Text/Json < Bytes < any other `Value`; the last class compares by `Display` text.
- Numbers compare by their displayed decimal value, exactly: Int digits, Decimal text, Float shortest round-trip. -0 equals 0; -inf is first, then finite values, +inf, and NaN last. Equal numbers (`1`, `1.0`, `1.00`) tie.
- Dates are timestamps at 00:00; on an equal instant Date sorts before Timestamp. TimestampTz compares by UTC instant, then by offset.
- Text: with `CodePoint`, `str::cmp`. With `CaseInsensitive` (the default, "Match case" off), the `char::to_lowercase` sequences are compared first, then `str::cmp` as the tie-break.
- The comparator ends with a model-index tie-break, so the sort is stable and deterministic.

**Key cache.** Each sort key has a dense per-row cache of a `u8` tag (`class << 3 | kind`, with kinds Int, Float, Bool, TextPrefix, Instant, Slow, Approx; NULL has its own tag) and `u64` bits. Floats, and ints up to 2^53 (exact as a float), share one key space: the sign-flipped bits with -0 and NaN normalised (kind Float, exact). A decimal has the same bits of its nearest `f64` (kind Approx: the rounding is monotone, but different decimals can share bits). Ints beyond 2^53 keep their own sign-flipped key (kind Int). Text keys hold the first 8 bytes of the folded or raw UTF-8, instants the nanoseconds since 1970 (clamped to the `i64` range with kind Approx outside it). Two keys of one class whose kinds are Float, Instant or Approx are decided from the cache when their bits differ; equal Int, Float and Bool keys are equal; everything else (TextPrefix, Instant and Approx with equal bits, Slow, an Int beyond 2^53 against a float) falls back to `compare_cells` on the model rows (two `model.row` lookups per compare; a mixed int/float comparison formats into a stack buffer and allocates nothing). Text that shares its first 8 bytes (URLs, paths, ids with a prefix) is the case that stays on the fallback. A triple-transitivity test over about 40 cells, a randomised test that a decided key never contradicts `compare_cells` on close numbers, and an independent reference in `tests/grid_order_props.rs` guard the total order, since the standard sorts may panic on a comparator that is not one.

**Filter.** `FilterSpec` is a quick filter (text, all columns or one) AND every condition (`Equals`, `NotEquals`, `Contains`, `Greater`, `Less`, `IsNull`, `NotNull`). NULL fails every op except `IsNull`; `NotEquals` excludes NULL (SQL semantics). Contains is a case-folding (per collation) substring match on the full `Display` text (the `&str` of Text/Json cells, a formatted value in a scratch buffer otherwise); an empty needle matches. For `Equals`, `NotEquals`, `Greater` and `Less` the trimmed operand is read in the cell's type (a number for numeric cells, `true/false/t/f/1/0` for Bool, `FromStr` for temporal cells); when it does not parse, the display text is compared with the operand under the collation. Operands are compiled once.

**Match case.** One per-grid setting (not persisted), off by default; it covers sort and filter.

**Commands.** `grid:sort-ascending`, `grid:sort-descending` (on the active cell's column; they replace the sort and do nothing without an active cell), `grid:clear-sort`, `grid:clear-filter`, `grid:toggle-match-case`. No default key bindings. The header shows a sort indicator at its right edge; a click cycles ascending, descending, none, and Shift adds a key.

**Budgets and figures.** Budgets at 1M x 6 rows: text sort p95 1.5 s, quick filter p95 500 ms, 50-row window of the sorted order p95 1 ms, longest 6 ms slice 16 ms. Measured on macOS 27.0.1 arm64 (Apple M4 Pro, other builds running on the machine), minimum over 3 repetitions (`docs/src/benchmarks.md`, "Results grid sort and filter"): text sort 68 ms, integer sort 32 ms, two-key sort 132 ms, quick filter "99" 112 ms, two-condition filter 10 ms, sorted viewport 1.9 us, longest slice 6.03 ms. Three more 1M-row columns cover the sorts the key cache does not always decide: decimals 111 ms (longest slice 6.2 ms), ints mixed with floats 105 ms (6.2 ms) and URL-like text sharing a 37-byte prefix 2.2 s (longest slice 8.0 ms; every comparison reads two cells, so this case is slow but each step stays bounded). Budgets: decimals and mixed 500 ms, URL text 15 s (a guard against a catastrophe on a slow runner; a regression is caught by the relative comparison), every longest slice 16 ms. All are met; on columns decided by the key cache the longest slice is the 6 ms budget, on the cell-reading path 8 ms was measured, and a step is not guaranteed to stay under a frame on a slower machine. `bench.yml` gates `sort_p95_ns`, `filter_p95_ns`, `viewport_sorted_p95_ns` and `sort_url_p95_ns` with the rule of PR #33 (minimum over 3 repetitions, 25 % plus an absolute slack, keys the baseline lacks are skipped).

**Memory.** Published: 4 bytes per displayed row (the order) plus 4 bytes per covered model row (the inverse, filtered or not) plus 9 bytes per covered row for each sort key (tag and bits): 17,000,000 bytes for a text sort of 1M rows. While a job runs there is more: the kept rows, the two merge buffers (4 bytes each) and the new inverse, next to the published order of the previous spec. Measured peak while one text sort replaces another at 1M rows: 25,194,632 bytes (about 25 bytes per row). Freed when the order becomes inactive.

## Consequences

- Good: sort and filter work on the compact storage with no copy of cell data and no new thread; selection, scrolling and rendering share one `RowOrder`.
- Good: cancel is dropping the job, and the previous order stays visible; a step is bounded by the slice budget plus one unit of work: 6.0 to 6.4 ms on the columns of the bench, 8.0 ms on text that shares a long prefix (see the figures above).
- Good: a documented total order; the sort is stable and deterministic.
- Bad: sort and filter see only the rows loaded so far (the view says "loaded rows only" while the state is not `Done`); a paged result is not sorted on the server.
- Bad: rows that load while a sort is active are merged in and move the content under the viewport.
- Bad: a text column with long shared prefixes (URLs) falls back to `compare_cells` often (two `model.row` binary searches per compare), so the sort may approach its budget there; it is recorded, and `model.rs` is not changed.
- Bad: a few transitions are O(n) and run as one unit each, not in slices: allocating the inverse (`vec![HIDDEN; to]`), copying the published order when an extend job starts its merge (`published.to_vec()`, the `MergeOld` buffers) and the final move of the result. Each is a memory-bound pass of 4 bytes per row; they stay far under a frame at 1M rows and approach one (about 16 ms) only at tens of millions of rows, a size the grid does not reach with its default row limit. Slicing them is not planned.
- Bad: sorted runs are sized from the speed of the previous run, so a column that is cheap in its first rows and expensive later can overrun the step budget once; after an overrun (a run taking more than twice the target) the next run is halved at least and the size grows by a quarter at a time, so later steps stay near the budget.
- Bad: a cell edited in a sorted view (T11) does not re-sort until the spec is re-applied.
- Bad: `GridControls::{motion_moves, apply, click_cell, click_header, is_selected}` take `&dyn RowOrder` in place of a row count; callers (T7, T8, T11) pass `&tab.rows()`.
- Neutral: server-side sort and filter remain a later option for results that are not fully loaded.
