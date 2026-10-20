## Iteration 004 — 2026-10-20
### Delivered
M3 (editor and intellisense), items in merge order (the order of `git log` on `main`):
- Verify the TLS server name through an SSH tunnel (PR #28, `docs/src/user-guide/ssh-tunnels.md`, `docs/src/user-guide/connections.md`)
- Catalog snapshot, name index and on-demand column loading (PR #29, `docs/src/architecture/catalog.md`)
- Query history store and saved queries (PR #27, `docs/src/user-guide/settings.md`)
- Robust gates for the grid viewport and browser tree benches (PR #33, `docs/src/benchmarks.md`)
- Completion engine: lexer, scope analysis, ranking, schema source trait (PR #30, `docs/src/architecture/completion.md`)
- SQL text services: split, classify, format, placeholders (PR #32, `docs/src/architecture/sql.md`)
- RSA without the `rsa` crate: `ring` SSH signer and vendored `sqlx-mysql` on `aws-lc-rs` (PR #31, ADR-0018, `docs/src/user-guide/ssh-tunnels.md`, `docs/src/user-guide/databases/`)
- Query execution: run modes, parameters, prod write gate, cancel (PR #34, `docs/src/user-guide/running-queries.md`)
- SQL editor tab: highlighting, multi-cursor, bracket matching, format, run integration (PR #36, `docs/src/user-guide/sql-editor.md`)
- Completion: hover and diagnostics (PR #35, `docs/src/architecture/completion.md` "Hover and diagnostics")
- History tab and saved queries (PR #37, `docs/src/user-guide/query-history.md`)
- Editor intellisense: completion, hover and diagnostics (PR #38, `docs/src/user-guide/sql-editor.md#intellisense`)
- Metrics and this report (PR [12/12], `docs/src/benchmarks.md`)
### Decisions
- ADR-0018 RSA without the `rsa` crate: a `ring` SSH signer and a vendored `sqlx-mysql` patch on `aws-lc-rs` (accepted) — RSA SSH user keys sign through `ring`; the `MySQL` plaintext RSA exchange encrypts through `aws-lc-rs` and is allowed only with an explicit `ssl-mode=disabled`.
- ADR-0019 TLS server name through a tunnel: `TlsConfig::server_name`, local Unix-socket bridge (accepted) — the certificate is checked against the original host while the driver dials the tunnel; Windows still refuses it.
- ADR-0020 SQL text services: own lexer, fail-safe classification (accepted) — one lexer that never fails; `Read` only when no write syntax is found, everything uncertain is `Unknown`.
- ADR-0021 Query history and saved queries: JSON Lines history, versioned TOML saved queries (accepted) — append-only `history.jsonl` with always-on redaction, `queries.toml` with folders and tags.
- ADR-0022 Catalog snapshot, name index and on-demand column loading (accepted) — a lock-free immutable snapshot with a prefix and fuzzy name index; columns load on demand.
- ADR-0023 Completion engine: own lexer, schema source trait, ranking (accepted) — works on incomplete SQL, reports unloaded data as `SchemaRequest`s, and also drives hover and diagnostics.
- ADR-0024 Query execution: headless pipeline in `sqler-ui`, bound parameters, per-statement results, prod gate (accepted) — statements run in order on a lent session, parameters are bound and never spliced, every non-`Read` statement asks on `prod`; no read-only transaction.
- ADR-0025 SQL editor component: `gpui-component` editor with headless logic (accepted) — the widget is `gpui-component`; selection, brackets, formatting and run integration are headless in `sqler_ui::editor`.
- ADR-0026 Editor intellisense: completion, hover and diagnostics in the SQL editor (accepted) — GPUI-free logic, `lsp-types` 0.97 only for the provider traits, debounced diagnostics, detached catalog loads.

T1 outcome: both RSA parts are enabled without the `rsa` crate and with no advisory ignore. SSH goes through a `ring` signer in `sqler-tunnel`; `MySQL` goes through the vendored `sqlx-mysql` `=0.9.0` on `aws-lc-rs` (56 changed lines in 4 files against upstream). `deny.toml` bans `rsa`, and `cargo tree -i rsa` is empty.
### Metrics
Measured on 2026-10-20, macOS 27.0.1 arm64 (Apple M4 Pro), merged `main` at commit `e98f4c8`, `target/release/sqler` (default features, `--locked`). Tracked in [benchmarks](../src/benchmarks.md). Each value is labelled "fresh at `e98f4c8`" (measured for this report) or "PR #N" (quoted from that PR body or its CI run, not re-measured).
- Cold start (fresh at `e98f4c8`): 113.7 ms mean (σ 3.2, min 109.7, max 117.7), `hyperfine` 20 runs after 3 warm-ups of `sqler --exit-after-first-frame`. Target < 500 ms: met on macOS. M2 was 155.1 ms.
- Idle RSS (fresh at `e98f4c8`): 71.9 to 72.1 MiB (three runs, `ps` at 6 s and 10 s) with three saved connections (`SQLite` `:memory:`, `PostgreSQL`, `MySQL`; both servers running in Docker), configured and not opened: the app still has no headless way to open a session. Target < 150 MB: met for the configured-not-opened case. M2 was 100.0 MiB. The drop of 28 MiB, and of 41 ms in cold start, has no explanation in the code merged since `ba24c9a`, so it is reported as measured and not as an improvement; it probably reflects the machine and display state of the run, and the maintainer should repeat both on a desktop session.
- Binary size (fresh at `e98f4c8`): 42.3 MiB (44,319,056 bytes, 44.3 MB decimal). Target < 60 MB: met on macOS (+4.0 MiB since M2, 38.3 MiB). Linux and Windows binaries are not measured; the M1 Linux spike build was 57.7 MB and the app has grown by 12.8 MiB on macOS since M1, so the 60 MB budget on Linux is at risk and needs a real measurement.
- Completion p95 on 5,000 tables (fresh at `e98f4c8`, engine only, three runs): `complete_p95_ns` 399,208, 392,958 and 405,042 ns; worst completion scenario 426,791 ns (`from_fuzzy`); worst of all twelve scenarios 2,487,958, 2,387,916 and 2,406,625 ns (`diagnose_big_script`). Budget 16 ms: met. Linux CI: PR #30 `complete_p95_ns` 464,994 ns; PR #35 690,583 ns.
- Editor end to end (fresh at `e98f4c8`, `editor_intellisense`, three runs): worst p95 282,042, 254,125 and 269,750 ns (`from_prefix`). Budget 16 ms: met, about 57 times under.
- Hover and diagnose p95 (fresh at `e98f4c8`, engine, three runs): hover 115,292, 114,000 and 115,000 ns; diagnose on a 2,000-statement script 2,356,875, 2,374,334 and 2,397,459 ns. Editor path: hover 4,250 ns, diagnose on five statements 129,083 ns (run 1). Linux CI, PR #35: hover 257,328 ns, diagnose 5,038,306 ns.
- Catalog index (fresh at `e98f4c8`, three runs): snapshot p95 42 to 84 ns; prefix lookup p95 241,167, 158,333 and 162,417 ns (budget 1 ms; 138,042 to 145,917 ns at `7a7dd54`, so about 10 to 65 % higher, the first run after a build being the high one); fuzzy p95 151,583 to 155,750 ns (budget 5 ms); index 220,600 bytes (budget 4 MiB). Linux CI, PR #29: prefix p95 322,381 ns.
- Editor buffer (fresh at `e98f4c8`, 10,000 lines, three runs): bracket build p95 1,349,541, 487,750 and 512,000 ns; bracket lookup p95 250 and 167 ns; format p95 11,016,542, 9,138,291 and 9,071,458 ns (budget 16 ms: met, the closest figure to its budget); next occurrence p95 about 52 us. PR #36 quoted about 1.0 ms, 10.7 ms and 52 us.
- Editor keystroke latency: headless work per keystroke = `editor_intellisense` worst p95 plus `bracket_build_p95_ns` when the text changes next to a bracket; worst of three runs 282,042 + 1,349,541 ns = 1.63 ms (0.74 to 0.79 ms with the other two runs). The input-to-paint latency of the widget is not measured (needs a display probe); it is a maintainer check.
- M3 DoD: completion p95 < 16 ms on 5,000 tables: met (engine worst p95 2.49 ms over all scenarios, 0.43 ms over the completion scenarios; editor end-to-end worst p95 0.28 ms; both headless, so the paint of the popup is not included).
- Catalog tree, browser tree and grid (fresh at `e98f4c8`): gated figures unchanged within their gates (cached read p95 84 ns, browser expand p95 648,000 ns, grid viewport p95 167 ns). Not gated: catalog tree cold expand p95 492,083 ns, up from 351,167 ns at `7d80fbf` (one run, budget 16 ms).
- Test count (fresh at `e98f4c8`): 1595 tests run, 1595 passed, 2 skipped (`cargo nextest run --workspace --no-tests=pass` with `PostgreSQL` 18, `MySQL` 8.4 and the `sshd` container in Docker, `SQLER_TEST_REQUIRE_DB=postgres,mysql,ssh`), up from 814 at the end of M2 (+781). `MariaDB` 11.8: `cargo nextest run -p sqler-driver-mysql --no-tests=fail`, 35 passed (26 in M2).
- Coverage: not measured (`cargo llvm-cov` is not installed).
- Linux and Windows desktop numbers: not measured, because `ci.yml` has no GUI runner.
### Bench gate changes
- PR #33: `grid_viewport` and `browser_tree` take the minimum over `--reps 3` repetitions (base and PR alike) and compare with an absolute slack (grid 100 ns, browser tree 250 us) on top of 25 %; a 2x slowdown still fails (checked in the PR).
- PR #30 and PR #35: the completion gate is 25 % + 250 us over the minimum of 3 in-process repetitions; `hover_p95_ns` and `diagnose_p95_ns` are gated the same way once the base has a baseline with those keys, and until then only the 16 ms budget applies to them.
- PR #29: `catalog_index` added to `bench.yml`; the `catalog_tree` slack was raised from 50 to 150 ns.
- `editor_buffer` and `editor_intellisense` are not gated and not in `bench.yml`; their figures go into the PR body and `benchmarks.md`.
### Risks & blockers
M3 is closed with these maintainer-only checks open (none can be done by an agent; the owner runs them in parallel):
- Editor, on a display (PR #36): highlighting in both themes, tab header, Alt-click, add cursor above and below, add next occurrence, bracket frame, `editor:format-sql` as one undo step, `editor:new`, "Open in SQL Editor", typing and scrolling speed on the 10,000-line script.
- Query execution (PR #34): run modes by key and palette, per-statement result tabs, "Cancelled", cancel of `pg_sleep` and `SLEEP`, the parameter dialog, the prod confirmation, cancel on tab close and quit, a responsive window during a sleep.
- History and saved queries (PR #37): newest-first rows, live search and the "All connections" toggle, Open and Run again (prod confirmation, truncated and redacted entries), Clear history, Save query and its errors, SAVED QUERIES, `query:open-saved`, rename and delete, persistence after a restart, a large history file, `<redacted>`, focus after dialogs.
- Intellisense (PR #38): the popup for tables and for late columns of `u.`, hover, the underline after 300 ms, pick-up of a catalog refresh, no popup without a catalog, views in the popup, smooth typing.
- Ctrl+Space on macOS: the input-source shortcut may take the chord; rebind `editor:trigger-completion` or use the palette.
- Keychain (PR #16, still open): `cargo test -p sqler-config -- --ignored keychain_round_trip` on macOS, Windows and Linux desktop sessions.
- SSH real bastion (PR #19, still open): `SQLER_TEST_BASTION_*` and `cargo test -p sqler-tunnel --test real_bastion -- --ignored`; plus RSA keys (file and agent) against a real server and the "use Ed25519/ECDSA" error for a SHA-1-only server (PR #31), and a plaintext `MySQL` RSA exchange against a real server (PR #31).
- `verify-full` through a tunnel against a real server on macOS and Linux (PR #28).
- `aws-lc-sys` (`prebuilt-nasm`) build on Windows and macOS CI (PR #31).
- Idle RSS with three sessions actually open, and a repeat of cold start and idle RSS on a desktop session, because this run's figures are lower than M2 for no known reason.
- The M1 and M2 UI checks that are still open: shell, settings and themes, palette and keybindings, grid scrolling (M1); connection manager and object browser (PR #24, #25).

Known gaps:
- Diagnostics (PR #35, `docs/src/architecture/completion.md` "Limits"): silent for non-DML statements, unloaded relations, relations created in the text, uncertain levels and routine bodies; a typo in the select list of a derived table or CTE body, an ambiguous name inside a right-side `SET` expression and hover inside `INSERT ... SELECT` are missed.
- Classifier (ADR-0020): a function or view can write behind `Read`; `standard_conforming_strings=off`, `ANSI_QUOTES` and `NO_BACKSLASH_ESCAPES` give `Unknown`, and `Unknown` always confirms on `prod`.
- `MySQL` DDL commits implicitly and statements run in no read-only transaction (ADR-0024); the `MySQL` `KILL QUERY` race (ADR-0010); the "Dropping..." modal blocks the window (ADR-0017).
- Bench noise on shared runners: Linux CI figures are 1.5 to 3 times the macOS ones (PR #35: diagnose 5.04 ms), and CI gates compare base and PR in the same job for that reason.
- The 1M-row grid still holds about 235 MB; M4 needs a bounded result model before real queries feed it.
- Linux and Windows desktop numbers are unmeasured; the Linux binary may be near the 60 MB target.
- Maintenance of the vendored `sqlx-mysql` (`=0.9.0`), and `aws-lc-sys` with `prebuilt-nasm` on Windows.
- GPUI is a pre-release fork (`gpui-pre` =0.3.9) with MPL-2.0 exceptions and advisory ignores in `deny.toml`.
### Owner decisions taken (intake)
- (1) RSA alternatives with no advisory ignore and no `rsa` crate: both implemented, with limits (no SHA-1 `ssh-rsa`, no RSA-only host keys, legacy PEM refused, plaintext exchange only with an explicit `ssl-mode=disabled` and still open to a man in the middle).
- (2) `verify-full` through a tunnel: implemented on Unix through a local socket bridge; Windows still refuses it (ADR-0019).
- (3) Maintainer checks run in parallel by the owner, which is why M3 is closed with them listed.
### Open items carried forward
- Read-only connection mode, tab restore across restarts, connection pooling, saved-query folder sync and notebooks (out of M3, `plan.md` Assumptions).
- `verify-full` through a tunnel on Windows.
- Snippets without tab stops.
- A failed intellisense load is retried only after a reconnect (PR #38).
- A headless session-open path, so idle RSS with open sessions can be scripted.
- GUI runners for Linux and Windows in CI.
### Next iteration plan
M4 (results and editing), per the prompt section 11:
- Sort, filter and pin in the grid, cell detail, inline editing with staged changes and a SQL preview, all export formats and copy-as.
- The bounded result model (the 235 MB grid model) before real query results grow.
- The carried items that M4 needs: the headless session-open path, and the first Linux and Windows measurements.
### AGENTS.md changes
- Section 9: roadmap status reads "M3 done (code merged; maintainer-only checks run by the owner in parallel); M4 next", with the M3 delivered paragraph, the fresh numbers, a one-line M2 paragraph, the consolidated list of pending maintainer checks (M1, M2 and M3) and the known limits. The stale lines about pending product-owner decisions and RSA were removed; the outcomes are in this report.
- Section 10: three gotchas (newline loss after a rebase conflict on append-only files, the PR #33 rule for new gated benches, and the headless `editor_*` benches that stop at the GPUI providers).
### Raw measurements
Commands run at `e98f4c8` (merged `main`) on macOS 27.0.1 arm64 (Apple M4 Pro), from the repository root; `HOME` pointed at a scratch directory whose `Library/Application Support/SQLer/connections.toml` held a `SQLite` `:memory:`, a `PostgreSQL` (`127.0.0.1:54329`) and a `MySQL` (`127.0.0.1:33069`) entry. The quoted figures (PR #27 to #38, and the Linux CI runs of PR #29, #30, #33 and #35 from `gh run view --log`) come from the PR bodies and the `Bench` workflow logs and were not re-measured.
- Build: `cargo build --release -p sqler-app --locked`, exit 0.
- Binary size: `stat -f %z target/release/sqler` printed `44319056` (44319056 / 1048576 = 42.3 MiB).
- Cold start: `hyperfine -w 3 -r 20 'target/release/sqler --exit-after-first-frame'` reported mean 113.7 ms, σ 3.2 ms, min 109.7 ms, max 117.7 ms (peak memory per run 68.4 to 68.6 MiB).
- Idle RSS: `ps -o rss= -p <pid>` (KiB) at 6 s and 10 s, three runs: 73840 and 73824, 73712 and 73664, 73840 and 73792 (71.9 to 72.1 MiB).
- Benches, each with default flags, `cargo bench -p <crate> --bench <name>`, all exit 0:
  - `completion`, three runs: `complete_p50_ns` / `complete_p95_ns` 45791 / 399208, 48292 / 392958, 57958 / 405042; `hover_p95_ns` 115292, 114000, 115000; `diagnose_p95_ns` 2356875, 2374334, 2397459; `worst_p95_ns` 2487958, 2387916, 2406625; `from_fuzzy_p95_ns` 408667, 407500, 426791; `from_prefix_p95_ns` 367708, 361667, 383000; `from_empty_p95_ns` 217125, 209000, 215917.
  - `editor_intellisense`, three runs: `worst_p95_ns` 282042, 254125, 269750; run 1: `statement_start` 9083 / 10916, `from_prefix` 236417 / 282042, `qualified_column` 4875 / 5250, `alias_join` 5584 / 5708, `hover` 4167 / 4250, `diagnose` 125958 / 129083 (p50 / p95 ns).
  - `editor_buffer`, three runs: `bracket_build_p95_ns` 1349541, 487750, 512000; `bracket_lookup_p95_ns` 250, 167, 167; `format_p95_ns` 11016542, 9138291, 9071458; `occurrence_p95_ns` 53791, 51417, 53417; `bytes=541580`, `samples=30`.
  - `catalog_index`, three runs: `snapshot_p95_ns` 84, 42, 42; `prefix_p95_ns` 241167, 158333, 162417; `fuzzy_p95_ns` 151583, 153625, 155750; `load_tables_ms` 0.401, 0.101, 0.102; `publish_ms` 0.537, 0.526, 0.515; `index_bytes` 220600.
  - `catalog_tree` (fake source): `expand_cold_p50_ns` 243000, `expand_cold_p95_ns` 492083, `hit_p50_ns` 42, `hit_p95_ns` 84, `schemas_cold_p95_ns` 6084, `refresh_schema_p95_ns` 223125, `refresh_all_ms` 4.296.
  - `browser_tree`: `expand_p50_ns` 639084, `expand_p95_ns` 648000, `folder_p95_ns` 119917, `select_p50_ns` 1250, `select_p95_ns` 2500.
  - `grid_viewport`: `p50_ns` 125, `p95_ns` 167, `ingest_ms` 153, `heap_bytes` 246433024.
- Test count: `PostgreSQL` 18 (`start-postgres.sh`), `MySQL` 8.4 (`start-mysql.sh mysql`) and `start-sshd.sh` in Docker, `SQLER_TEST_REQUIRE_DB=postgres,mysql,ssh`, `INSTA_UPDATE=no`, `cargo nextest run --workspace --no-tests=pass`: 1595 run, 1595 passed, 2 skipped. Then `MariaDB` 11.8 (`start-mysql.sh mariadb`), `SQLER_TEST_REQUIRE_DB=mysql`: `cargo nextest run -p sqler-driver-mysql --no-tests=fail`: 35 run, 35 passed, 0 skipped. All containers were removed afterwards.
- Coverage: `cargo llvm-cov --version` reported that the subcommand does not exist; not installed, not installed by the agent.
- Not measured: Linux and Windows desktop numbers (no GUI runner), input-to-paint latency of the editor (no probe), idle RSS with open sessions (no headless path).
