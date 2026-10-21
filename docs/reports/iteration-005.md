## Iteration 005 — 2026-10-21
### Delivered
M4 (results and editing), items in merge order (the order of `git log` on `main`):
- `--open-sessions` measurement flag and the `Binary size` workflow (PR #40, `docs/src/benchmarks.md`, `docs/src/user-guide/workspace.md`)
- `sqler-export`: text writers (CSV, JSON, SQL `INSERT`, Markdown) and SQL literal helpers (PR #41, ADR-0028, `docs/src/architecture/export.md`, `docs/src/architecture/sql.md`, `docs/src/dependencies.md`)
- Bounded result model: credit gate, compact cells, fetch more (PR #42, ADR-0027, `docs/src/architecture/ui.md`, `docs/src/user-guide/results-grid.md`, `docs/src/user-guide/settings.md`, `docs/src/benchmarks.md`)
- Staged edits and bound DML: editability, apply plan, one-transaction apply (PR #43, ADR-0030, `docs/src/architecture/sql.md`, `docs/src/user-guide/results-grid.md`)
- Linux release binary: strip symbols and one codegen unit (PR #44, `docs/src/benchmarks.md`)
- `sqler-cli` export: formats, write gate, partial-file protocol (PR #45, `docs/src/user-guide/cli.md`)
- Grid columns and selection: resize, reorder, pin, cell and range selection, keyboard navigation (PR #46, `docs/src/user-guide/keybindings.md`, `docs/src/user-guide/results-grid.md`, `docs/src/benchmarks.md`)
- `sqler-export`: Parquet and XLSX writers (PR #47, ADR-0029, `docs/src/architecture/export.md`, `docs/src/dependencies.md`, `docs/src/benchmarks.md`)
- Cell detail pane: text, JSON and binary views, 1 MiB cap, Show all, copy (PR #48, `docs/src/architecture/ui.md`, `docs/src/dependencies.md`, `docs/src/user-guide/keybindings.md`, `docs/src/user-guide/results-grid.md`)
- Results grid copy and copy-as: TSV, Markdown, JSON, `INSERT` (PR #49, `docs/src/architecture/ui.md`, `docs/src/user-guide/keybindings.md`, `docs/src/user-guide/results-grid.md`)
- Client-side sort and filter for the results grid (PR #50, ADR-0031, `docs/src/user-guide/results-grid.md`, `docs/src/benchmarks.md`)
- Grid export: dialog, loaded, selection and fresh scopes, partial-file protocol, Parquet default (PR #51, ADR-0032, `docs/src/architecture/export.md`, `docs/src/user-guide/results-grid.md`, `docs/src/benchmarks.md`)
- Bench gate stability: interleaved base and PR rounds and a p50 gate for the browser tree (PR #53, `docs/src/benchmarks.md`)
- Inline editing in the results grid: cell editors, staged marks, SQL preview, apply through the production gate (PR #52, ADR-0033, `docs/src/architecture/ui.md`, `docs/src/user-guide/results-grid.md`, `docs/src/benchmarks.md`)
- Metrics and this report (this PR, `docs/src/benchmarks.md`)
### Decisions
- ADR-0027 Bounded result model: credit gate, fetch more and compact rows (accepted) — ingest pauses on a credit gate, the grid holds compact cells, and the user fetches more; amends ADR-0006.
- ADR-0028 Export writer API: push-style row writers, a format registry and SQL literals (accepted) — one writer trait for all formats; amends ADR-0020.
- ADR-0029 Parquet and XLSX writers: own streaming zip, low-level Parquet API, two cargo features (accepted) — `export-parquet` and `export-xlsx` features; touched by ADR-0032.
- ADR-0030 Staged edits and DML: bound statements, single-table editability, one-transaction apply (accepted) — edits are staged, turned into bound statements and applied in one transaction.
- ADR-0031 Client-side sort and filter: published display order, resumable job on the UI thread (accepted) — sort and filter act on the rows the grid holds and run in short slices.
- ADR-0032 Grid export: loaded rows or a fresh run of a read statement, writer on the blocking pool, hidden partial file, Parquet in the app default (accepted) — the writer never runs on the GPUI thread.
- ADR-0033 Inline editing: apply session, transaction flag, schema probe, in-place overlay and a single gate (accepted) — apply goes through the production gate once.

T14 (PR #44) needed no ADR: the Linux binary fell from 83,477,248 bytes before it to 55,798,352 bytes at `ec3057c` (fresh), under the 62,914,560 byte budget.
### Metrics
Measured on 2026-10-21, macOS 27.0.1 arm64 (Apple M4 Pro), merged `main` at commit `ec3057c`, `target/release/sqler` (default features, `--locked`), `rustc 1.99.0`. Tracked in [benchmarks](../src/benchmarks.md). Each value is labelled "fresh at `ec3057c`" (measured for this report) or "PR #N" (quoted from that PR body or its CI run, not re-measured). The load average was 1.3 to 4.5 during the runs.
- Cold start (fresh at `ec3057c`): 133.7 ms mean (σ 3.0, min 130.0, max 144.4), `hyperfine` 20 runs after 3 warm-ups of `sqler --exit-after-first-frame`, three `SQLite` `:memory:` entries. Target < 500 ms: met on macOS. M3 was 113.7 ms (+20.0 ms) and M2 155.1 ms. The rise has no known cause (see Risks).
- Idle RSS, three sessions open (fresh at `ec3057c`): `sqler --open-sessions` with three `SQLite` `:memory:` entries, every run printed `3 of 3 open, idling`. Six runs at 6 s: 101.0, 101.4, 72.5, 72.2, 72.2 and 72.3 MiB (10 s: 100.9, 101.4, 72.4, 72.1, 72.2 and 72.2 MiB). Target < 150 MB: met in all six. The two high runs (the first two) are about 29 MiB above the other four with the same binary and config; the cause is not known and was not investigated.
- Idle RSS, configured and not opened (fresh at `ec3057c`): 71.0 to 71.1 MiB (three runs, `ps` at 6 s and 10 s; `SQLite` `:memory:`, `PostgreSQL`, `MySQL`, both servers running in Docker). M3 was 71.9 to 72.1 MiB. The M3 mix with `--open-sessions` opened 1 of 3 (the test config has no stored password, so the two servers could not authenticate; keychain-backed entries are skipped by design), RSS 72.6 and 74.0 MiB in two runs; it is context, not a "three open" figure.
- Binary size (fresh at `ec3057c`): 30.46 MiB (31,939,312 bytes) on macOS, down 11.81 MiB (12,379,744 bytes) from M3 (42.27 MiB). Most likely T14 (`strip = "symbols"`, `codegen-units = 1`, PR #44), which applies to every OS; no before and after build isolates it. `Binary size` workflow run 39572444831 at `ec3057c`: Linux 55,798,352 bytes (53.2 MiB), macOS 31,922,960 bytes (30.4 MiB), Windows 40,875,520 bytes (39.0 MiB). Budget 62,914,560 bytes: met on all three, Linux with the least headroom (7,116,208 bytes). Before T14, Linux was 83,477,248 bytes (benchmarks.md).
- Grid viewport (fresh at `ec3057c`, `grid_viewport -- --staged 10000`, three runs, 1M rows): `p95_ns` 125, 125 and 125; `cells_p95_ns` 959, 958 and 1000; `ingest_ms` 201, 163 and 162; `heap_bytes` 113,900,996 (108.62 MiB, M3 246,433,024 = 235.02 MiB); `layout_p95_ns` 15,417, 15,875 and 15,750; `staged_p95_ns` 23,500, 24,125 and 23,542 with `staged_stage_ms` 7 (not gated; the 1 ms budget is on `staged_p95_ns`). `/usr/bin/time -l` on the bench binary: maximum resident set size 229,228,544 bytes (218.61 MiB), peak memory footprint 225,116,808 bytes (214.69 MiB). Reference: M3 `p95_ns` 167, PR #46 84 and `cells_p95_ns` 917.
- Grid sort and filter (fresh at `ec3057c`, `grid_sort`, three runs, 1M rows): `sort_p95_ns` 71,185,625, 71,368,417 and 70,876,042 (budget 1.5 s); `filter_p95_ns` 111.3, 110.3 and 110.9 ms (budget 500 ms); `viewport_sorted_p95_ns` 1,875, 1,917 and 1,917 ns (budget 1 ms); `slice_max_ns` 6.04, 6.04 and 6.02 ms (budget 16 ms); `sort_url_p95_ns` 2.18, 2.17 and 2.18 s (budget 15 s); `sort_multi_p95_ns` 139.8, 138.9 and 139.3 ms (PR #50: 132.4 ms, +5.6 %); `order_peak_heap_bytes` 25,195,592, 25,195,592 and 25,194,624. All three runs exit 0 and match PR #50 within 6 %.
- Grid export (fresh at `ec3057c`, `grid_export`, 1M rows, one run each): loaded `csv` 177 ms (5,627,531 rows/s, 49,777,801 bytes); loaded `json` 122 ms (8,181,286 rows/s); loaded `parquet` 273 ms (3,660,537 rows/s); loaded `xlsx` 2,606 ms (383,706 rows/s, 6 % below the PR #51 figure of 0.41 M rows/s); fresh `csv` 1,029 ms (971,118 rows/s); fresh `parquet` 1,365 ms (732,252 rows/s). Everything else is above the PR #51 figures; no regression above 25 %.
- Export writers (fresh at `ec3057c`, `export_throughput`, 1M rows, one run): `csv` 3,731,816 rows/s (225.4 MiB/s), `json` 5,479,264 (568.7), `sql` 3,780,677 (304.8), `markdown` 4,073,660 (288.8), `parquet` 6,270,505 (109.1), `xlsx` 811,937 (20.1). benchmarks.md had `parquet` 4.38 M rows/s and `xlsx` 0.55 M rows/s; both are higher now.
- Grid copy (fresh at `ec3057c`, `grid_copy`, first record, one run): `gather_slice_p95_ns` 490,250 against the 8 ms slice budget (6.1 % of it); `render_ms_tsv` 27.6 (7,291,671 bytes) and `render_ms_insert` 19.0 (9,184,672 bytes) for 100,000 rows of 10 columns.
- Completion p95 on 5,000 tables (fresh at `ec3057c`, `completion`, three runs): `complete_p95_ns` 389,125, 386,542 and 394,375 ns (M3 399,208, 392,958 and 405,042); `worst_p95_ns` 2,432,958, 2,371,125 and 2,393,250 ns (M3 2,487,958, 2,387,916 and 2,406,625). Budget 16 ms: met. `hover_p95_ns` 139,291, 137,834 and 138,042 ns against about 115,000 ns at M3: +20 %, unexplained (see Risks).
- Editor end to end (fresh at `ec3057c`, `editor_intellisense`, one run): worst p95 301,750 ns (`from_prefix`; M3 282,042, 254,125 and 269,750). Budget 16 ms: met.
- Editor buffer (fresh at `ec3057c`, `editor_buffer`, 10,000 lines, one run): bracket build p95 2,727,792 ns (M3 1,349,541, 487,750 and 512,000), bracket lookup p95 292 ns, format p95 11,759,083 ns (M3 11,016,542, 9,138,291 and 9,071,458; budget 16 ms: met), next occurrence p95 52,417 ns. The bracket build figure is the highest recorded and is a single run (see Risks).
- Editor keystroke latency (fresh at `ec3057c`): `editor_intellisense` worst p95 plus `bracket_build_p95_ns` = 301,750 + 2,727,792 ns = 3.03 ms, one run each, against the 16 ms frame budget (M3 1.63 ms at worst). The input-to-paint latency of the widget is still not measured.
- Catalog index (fresh at `ec3057c`, `catalog_index`, three runs): `snapshot_p95_ns` 125, 42 and 42; `prefix_p95_ns` 222,417, 123,459 and 121,458 (M3 241,167, 158,333 and 162,417; budget 1 ms); `fuzzy_p95_ns` 150,500, 150,083 and 152,209; `index_bytes` 220,600.
- Catalog tree and browser tree (fresh at `ec3057c`, one run each): `catalog_tree` cold expand p95 505,209 ns (M3 492,083), cached read p95 84 ns; `browser_tree` expand p95 616,417 ns (M3 648,000), select p95 2,042 ns (M3 2,500).
- Test count (fresh at `ec3057c`): 2237 tests run, 2237 passed, 2 skipped (`cargo nextest run --workspace --no-tests=pass` with `PostgreSQL` 18, `MySQL` 8.4 and the `sshd` container in Docker, `SQLER_TEST_REQUIRE_DB=postgres,mysql,ssh`), up from 1595 at the end of M3 (+642). `MariaDB` 11.8: `cargo nextest run -p sqler-driver-mysql --no-tests=fail`, 38 passed (35 in M3).
- M4 gate checks (fresh at `ec3057c`): `cargo deny check` exit 0; `deny.toml` unchanged since `e98f4c8` (no new advisory ignore); `cargo tree --workspace -i rsa` finds no package; `mdbook build docs` and `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items` exit 0; `ci.yml` run 39572444844 at `ec3057c` has `check` on Linux, macOS and Windows, `db`, `deny`, `docs` and `spdx` all green; the `Bench` workflow run at the final head of each M4 PR is green (listed in the raw measurements).
- Missed budgets: none. Every bench exited 0 and all three binary sizes are under 62,914,560 bytes.
- Coverage: not measured (`cargo llvm-cov` is not installed).
- Linux and Windows desktop numbers: not measured, because `ci.yml` has no GUI runner. Only the binary sizes of the three OSes are measured (`size.yml`).
### Bench gate changes
- PR #50: `grid_sort` added to `bench.yml`.
- PR #42: `grid_viewport` gains `cells_p95_ns` (reads the payload of each visible cell); it is gated once the baseline has the key.
- PR #52: `grid_viewport --staged N` (CI passes 10000) times the display path with N edits pending; `staged_p95_ns` is held to the 1 ms budget and compared against the baseline only when the baseline file has the key.
- PR #53: base and PR rounds are interleaved in one step with a judge-only call over the per-side minimum, and the browser tree is gated on its p50 with the p95 informational.
- `grid_export`, `export_throughput`, `grid_copy`, `editor_buffer` and `editor_intellisense` are not gated; they are held to their own budget (exit code) and a 25 % rule against the last figure in `benchmarks.md`. `bench.yml` runs on `pull_request` only, so there is no run at the `ec3057c` main sha.
### Risks & blockers
M4 is closed with these maintainer-only checks open (none can be done by an agent; the owner runs them in parallel). The verbatim per-PR lists are in `AGENTS.md` section 9.
- Unexplained cold start rise: 113.7 ms (M3) to 133.7 ms (+20.0 ms, σ 3.0) at `ec3057c`. No specific M4 change is known to add startup work, and M2 was 155.1 ms, so the M3 figure may have been the outlier. Far inside the 500 ms target, but not explained and not a verified regression; the owner should repeat it on a desktop session.
- Unexplained idle RSS spread with three sessions open: two of six runs at 101.0 and 101.4 MiB, four at 72.1 to 72.5 MiB, same binary and config. Runs 1 to 3 were taken before the database containers started and runs 4 to 6 after; run 3 was low, so the container state does not explain the gap. All are under 150 MB. Repeat on a desktop session.
- Unexplained `hover_p95_ns` rise: about 138,000 ns against about 115,000 ns at M3 (+20 %) in all three `completion` runs. Within the 25 % rule and 16 ms budget, so not flagged by any gate, and no M4 change to the completion engine is known.
- `editor_buffer` `bracket_build_p95_ns` 2,727,792 ns in a single run, the highest recorded (M3: 1,349,541, 487,750, 512,000 ns; the first run after a build was high in M3 too). Only one run was taken, so noise versus a real rise is not decided. Budget 16 ms: met.
- Unverified GUI behaviour: no headless GPUI runner exists. The cell editor key order (Enter, Tab and Escape), staged marks in both themes, dialogs (SQL preview, production confirmation, export, save), the filter bar against grid key routing, clipboard paste targets and a three-OS run of the M4 features have all been verified headless only (lists below and in `AGENTS.md` section 9).
- T4 (PR #45) was tested on macOS only: Ctrl+C handling, a second signal during cancel, and rename over an existing (read-only) target on Windows and Linux.
- Generated files not opened in real readers (PR #47, PR #51): `.xlsx` in Excel and LibreOffice, `.parquet` in pyarrow and `DuckDB`.
- Linux has the least size headroom: 55,798,352 bytes against 62,914,560 (7,116,208 bytes, 11 %). The default build includes `export-parquet` (ADR-0032).
- Stripped Linux binary (PR #44): a panic backtrace shows addresses only; the owner should confirm this policy on a desktop.
- Windows and Linux runs of `--open-sessions` with real keychain entries (PR #40) are unverified.
- The M1 to M3 UI checks that are still open (carried in `AGENTS.md` section 9).

Maintainer-only checks per M4 PR (Pending lists of the PR bodies):
- PR #42: scrolling a paused 1M-row result in the real window; the Fetch more button and `grid:fetch-more`; the paused status line; app RSS with a large result open; the in-app probe `SQLER_GRID_PROBE=1`.
- PR #41: none beyond CI. PR #43: none for the headless item (PR #52 adds the UI). PR #53: no Pending section.
- PR #47: open a generated 1M-row `.xlsx` in Excel and LibreOffice, and a generated `.parquet` in pyarrow and `DuckDB`; measure the Linux app size with `export-parquet` after T14 (closed by `size.yml` run 39572444831).
- PR #45: Windows and Linux: Ctrl+C, a second signal during cancel or close, and rename over an existing target.
- PR #46: drag resize, autofit, reorder; pinned columns while scrolling horizontally; selection painting; keyboard navigation and scrolling past the last row; Escape closing overlays with the grid focused; palette commands; trackpad horizontal scroll; Ctrl+Space in the grid versus the editor.
- PR #50: filter-bar typing against grid key routing; header indicator clicks; palette commands on the active column; the 150 ms debounce; responsiveness and scroll on 1M rows; "Sorting... %" and "N of M rows"; sort while loading and cancel by changing the sort.
- PR #48: pane layout and resize (240 to 1200 px); the pane across reruns and in the empty state; scrolling 1 MiB of text and 65,536 hex rows; Copy value and the disabled button on NULL; Show all label and discarded result; both themes.
- PR #49: Mod+C and Mod+Shift+C with the table focused and the SQL editor's copy unchanged; the right-click menu; the five palette entries; clipboard and notifications; responsiveness during a large copy; paste into a text editor, a spreadsheet and a Markdown viewer on three OSes.
- PR #51: native save dialog on three OSes; dialog layout; progress and Cancel during a 1M-row export; real mouse selection for the Selection scope; opening the files in Excel, LibreOffice and a Parquet reader; quit while exporting; the `size.yml` x86_64 figure after the Parquet default (closed by run 39572444831).
- PR #52: cell editor key order and F2 and double-click; staged marks in both themes; the SQL preview with 1,000+ statements; the production confirmation, keyless opt-in warning and close-with-pending-edits prompt; apply against `PostgreSQL` and `MySQL` with failure keeping edits pending; a run on macOS, Linux and Windows.
- PR #40: `--open-sessions` on a desktop with real keychain entries and no dialog; the three `size.yml` lines (closed by run 39572444831); `benchmarks.md` step 6 on a normal session; a changed SSH host key leaving `known_hosts` untouched.
- PR #44: start the stripped Linux binary on a desktop and accept the address-only backtrace policy; the three size lines (closed by run 39572444831).

Known limits (decided by the dev team from the PR reviews):
- Sorting a partial result sorts only the rows the grid holds ("loaded rows only" while loading or paused).
- Editing a table with no key is opt-in and warns (keyless opt-in editing).
- Export of a fresh run is guarded by a lexical read gate plus a session read-only guard; the guard is not available on `DuckDB` and SQL Server.
- Export of loaded rows follows the grid's sort and filter, while a fresh run ignores them.
- Copy over unsaved added rows copies nothing.
- `MySQL`: `USE` runs on the text protocol, and a session with autocommit off needs `SET autocommit = 1` first.
- Carried from before: bench noise on shared runners (Linux CI figures are 1.5 to 3 times the macOS ones); the vendored `sqlx-mysql` (`=0.9.0`) and `aws-lc-sys` on Windows; GPUI is a pre-release fork (`gpui-pre` =0.3.9) with advisory ignores in `deny.toml`.
### Owner decisions taken (intake)
- Maintainer checks run in parallel by the owner, which is why M4 is closed with them listed (the waiver of M3 decision 3 applies; every code-verifiable Definition of Done item was met).
### Open items carried forward
- Read-only connection mode.
- Tab restore across restarts.
- Connection pooling.
- Server-side sort and filter.
- GUI runners for Linux and Windows in CI.
- Saved-query folder sync and notebooks.
- `verify-full` through a tunnel on Windows.
- Snippets without tab stops.
- A failed intellisense load is retried only after a reconnect (PR #38).
- Input-to-paint latency probe for the editor and the grid.
- Coverage measurement (`cargo llvm-cov` not installed).
Closed in M4: the 235 MB grid model (now 108.62 MiB for 1M rows, ADR-0027), the headless session-open path (`--open-sessions`, PR #40), and the first Linux and Windows measurements (binary sizes from `size.yml`; desktop numbers are still open).
### Next iteration plan
M5 (AI), per the prompt section 11:
- Start M5 per the prompt; the Definition of Done items for M4 that need a display stay on the pending list.
- Repeat cold start, idle RSS and the editor bracket build on a desktop session, to settle the unexplained figures above.
### AGENTS.md changes
- Section 9: roadmap status reads "M4 done (code merged; maintainer-only checks run by the owner in parallel, see the list); M5 next", with the M4 delivered paragraph and the fresh numbers, a one-line M3 paragraph, the pending list (M1 to M3 kept, one bullet per M4 PR; the idle RSS with three sessions open and the CI-only first runs closed by evidence) and the known limits. The 235 MB grid gap and the headless session-open carried item were removed.
- Section 10: three gotchas (`GridControls` take `&dyn RowOrder` and display rows differ from model rows, the inline-editing apply session and transaction flag, and the cell-detail chord and Mod+C scope).
### Raw measurements
Commands run at `ec3057c` (merged `main`) on macOS 27.0.1 arm64 (Apple M4 Pro), from the worktree root; `HOME` pointed at a scratch directory whose `Library/Application Support/SQLer/connections.toml` held three `SQLite` `:memory:` entries (cold start, `--open-sessions`) or the M3 mix (`SQLite` `:memory:`, `PostgreSQL` `127.0.0.1:54329`, `MySQL` `127.0.0.1:33069`) for the configured idle RSS. The figures labelled "PR #N" were not re-measured. Scratch and containers were removed afterwards.
- Build: `cargo build --release -p sqler-app --locked`, exit 0.
- Binary size: `stat -f %z target/release/sqler` printed `31939312` (31939312 / 1048576 = 30.46 MiB).
- Per OS: `gh run view 39572444831 --log | grep 'release binary on'` (`size.yml` at `ec3057c`): Linux 55798352, macOS 31922960, Windows 40875520 bytes. Earlier run 39570169454 at `b95354a`: Linux 55409232, Windows 40381952 bytes.
- Cold start: `hyperfine -w 3 -r 20 'target/release/sqler --exit-after-first-frame'` reported mean 133.7 ms, σ 3.0 ms, min 130.0 ms, max 144.4 ms (peak memory per run 67.7 to 68.1 MiB).
- Idle RSS, configured: `ps -o rss= -p <pid>` (KiB) at 6 s and 10 s, three runs: 72768 and 72720, 72816 and 72736, 72848 and 72768 (71.0 to 71.1 MiB).
- Idle RSS, open: `target/release/sqler --open-sessions 2> open.log`, wait for `sqler: open-sessions: 3 of 3 open, idling`, `ps` at 6 s and 10 s (KiB), six runs: 103456 and 103360, 103808 and 103808, 74208 and 74112, 73888 and 73840, 73984 and 73936, 74016 and 73968. The M3 mix printed `skipped "PostgreSQL": the database connection failed`, `skipped "MySQL": the database connection failed` and `1 of 3 open, idling` (RSS 74368 and 74368 KiB before the containers started; 75776 and 75728 KiB with them running).
- Benches, each with default flags unless shown, `cargo bench -p <crate> --bench <name>`, all exit 0:
  - `sqler-ui grid_viewport -- --staged 10000`, three runs: `p95_ns` 125, 125, 125; `cells_p95_ns` 959, 958, 1000; `ingest_ms` 201, 163, 162; `heap_bytes` 113900996; `layout_p95_ns` 15417, 15875, 15750; `staged_p95_ns` 23500, 24125, 23542; `staged_stage_ms` 7. `/usr/bin/time -l` on the bench binary: `229228544  maximum resident set size`, `225116808  peak memory footprint`.
  - `sqler-ui grid_sort`, three runs: `sort_p95_ns` 71185625, 71368417, 70876042; `sort_int_p95_ns` 32252417, 32168791, 32349833; `sort_multi_p95_ns` 139765375, 138928292, 139291167; `filter_p95_ns` 111307042, 110306875, 110890833; `filter_cond_p95_ns` 9910833, 10321750, 10338792; `viewport_sorted_p95_ns` 1875, 1917, 1917; `slice_max_ns` 6035917, 6037125, 6018500; `order_heap_bytes` 17000000; `order_peak_heap_bytes` 25195592, 25195592, 25194624; `sort_dec_p95_ns` 114755875, 114352250, 114631042; `sort_url_p95_ns` 2180933666, 2174247250, 2176007125; `sort_mixed_p95_ns` 109416166, 109322458, 109158167.
  - `sqler-ui grid_export` (loaded csv): `elapsed_ms` 177, `loaded_csv_rows_per_sec` 5627531, `bytes` 49777801. Extra runs with `--source` and `--format`: loaded json 122 ms and 8181286 rows/s, loaded parquet 273 ms and 3660537, loaded xlsx 2606 ms and 383706, fresh csv 1029 ms and 971118, fresh parquet 1365 ms and 732252.
  - `sqler-ui grid_copy`: `gather_slice_p50_ns` 397500, `gather_slice_p95_ns` 490250, `render_ms_tsv` 27.6, `render_bytes_tsv` 7291671, `render_ms_insert` 19.0, `render_bytes_insert` 9184672.
  - `sqler-export export_throughput`: `csv_rows_per_sec` 3731816, `json_rows_per_sec` 5479264, `sql_rows_per_sec` 3780677, `markdown_rows_per_sec` 4073660, `parquet_rows_per_sec` 6270505, `xlsx_rows_per_sec` 811937 (MiB/s 225.4, 568.7, 304.8, 288.8, 109.1, 20.1).
  - `completion`, three runs: `complete_p50_ns` / `complete_p95_ns` 60875 / 389125, 65541 / 386542, 46708 / 394375; `hover_p95_ns` 139291, 137834, 138042; `diagnose_p95_ns` 2356500, 2351584, 2369333; `worst_p95_ns` 2432958, 2371125, 2393250; `from_fuzzy_p95_ns` 417000, 397625, 409042; `from_prefix_p95_ns` 372042, 367959, 384125; `from_empty_p95_ns` 254167, 219917, 220209.
  - `editor_intellisense`: `worst_p95_ns` 301750; `statement_start` 8917 / 10917, `from_prefix` 253959 / 301750, `qualified_column` 4958 / 5042, `alias_join` 5750 / 5875, `hover` 4209 / 4333, `diagnose` 127042 / 130333 (p50 / p95 ns).
  - `editor_buffer`: `bracket_build_p50_ns` 1129166, `bracket_build_p95_ns` 2727792, `bracket_lookup_p95_ns` 292, `format_p50_ns` 8593750, `format_p95_ns` 11759083, `occurrence_p95_ns` 52417; `bytes=541580`, `samples=30`.
  - `catalog_index`, three runs: `snapshot_p95_ns` 125, 42, 42; `prefix_p95_ns` 222417, 123459, 121458; `fuzzy_p95_ns` 150500, 150083, 152209; `load_tables_ms` 0.654, 0.106, 0.117; `publish_ms` 0.431, 0.437, 0.422; `index_bytes` 220600.
  - `catalog_tree` (fake source): `expand_cold_p50_ns` 246625, `expand_cold_p95_ns` 505209, `hit_p50_ns` 42, `hit_p95_ns` 84, `schemas_cold_p95_ns` 5125, `refresh_schema_p95_ns` 238792, `refresh_all_ms` 3.981.
  - `browser_tree`: `expand_p50_ns` 609917, `expand_p95_ns` 616417, `folder_p95_ns` 121041, `select_p50_ns` 1000, `select_p95_ns` 2042.
- Test count: `PostgreSQL` 18 (`start-postgres.sh`), `MySQL` 8.4 (`start-mysql.sh mysql`) and `start-sshd.sh` in Docker, `SQLER_TEST_REQUIRE_DB=postgres,mysql,ssh`, `INSTA_UPDATE=no`, `cargo nextest run --workspace --no-tests=pass`: 2237 run, 2237 passed, 2 skipped. Then `MariaDB` 11.8 (`start-mysql.sh mariadb`), `SQLER_TEST_REQUIRE_DB=mysql`: `cargo nextest run -p sqler-driver-mysql --no-tests=fail`: 38 run, 38 passed, 0 skipped.
- Checks: `cargo deny check` exit 0 (`advisories ok, bans ok, licenses ok, sources ok`); `git diff e98f4c8..HEAD -- deny.toml` empty; `cargo tree --workspace -i rsa` printed `error: package ID specification `rsa` did not match any packages`; `mdbook build docs` exit 0; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items` exit 0; `ci.yml` run 39572444844 at `ec3057c` all jobs success.
- `Bench` workflow runs at the final head of each M4 PR, all success: PR #42 39514446344, #41 39511007499, #40 39508707458, #43 39517058811, #44 39516457224, #45 39517969629, #46 39522092691, #47 39520414293, #50 39529727634, #48 39555341031, #49 39552987230, #51 39560078015, #52 39570763155, #53 39568266014.
- Coverage: `cargo llvm-cov --version` reported that the subcommand does not exist; not installed, not installed by the agent.
- Not measured: Linux and Windows desktop cold start and idle RSS (no GUI runner), input-to-paint latency of the editor and the grid, the three-OS GUI behaviour of the M4 PRs, code coverage.
