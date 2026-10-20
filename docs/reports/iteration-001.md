## Iteration 001 — 2026-10-20
### Delivered
- T1 Cargo workspace skeleton and project handbook (PR #1, `docs/src/dependencies.md`, `AGENTS.md`)
- T2 mdBook skeleton and ADR-0000 to ADR-0003 (PR #2, `docs/src/introduction.md`, `docs/src/adr/`)
- T3 CI on three OSes, `cargo-deny`, SPDX check and Pages workflow (PR #4, `AGENTS.md` section 3, `docs/src/dependencies.md`)
- T4 Core domain types and driver API (PR #3, `docs/src/architecture/core-and-driver-api.md`)
- T5 `SQLite` driver and `sqler-cli run` (PR #5, `docs/src/user-guide/cli.md`)
### Decisions
- ADR-0000 ADR process (accepted) — MADR format, with metadata as a bullet list because mdBook does not strip YAML front matter.
- ADR-0001 UI framework spike plan (proposed) — one-week M1 spike with GPUI and gpui-component as the default candidate, covering a 1M-row virtualised grid and a highlighted SQL editor on all three OSes.
- ADR-0002 Plugin ABI: WASM Component Model (accepted) — plugins are WebAssembly components on wasmtime with WIT interfaces and capability grants.
- ADR-0003 AI tool contract (accepted) — one tool set in `sqler-ai-tools` shared by the in-app chat, the MCP server and ACP-hosted agents.
- ADR-0004 Driver API: boxed futures, event stream, session cancellation (accepted) — object-safe `Driver` and `Session` traits returning boxed futures and a `QueryStream` of events, with explicit cancel handles.
### Metrics
All values below come from commands run on 2026-10-20 on macOS 27.0.1 arm64 against `target/release/sqler-cli` (default features, default release profile). Tracked in [benchmarks](../src/benchmarks.md).
- Cold start: 2.4 ms mean (max 2.6 ms) for `sqler-cli run :memory: "select 1"`; `--version` floor 2.4 ms mean (max 2.7 ms). 20 runs after 3 warm-ups, timed with Python `time.perf_counter` around `subprocess.run` (`hyperfine` not installed). macOS 27.0.1 arm64, commit `6fc6229`. Target < 500 ms. Warm file cache, CLI only.
- Idle RSS with three connections: n/a until M1 (UI); CLI peak RSS 3.6 MB (3817472 bytes, `/usr/bin/time -l`) for reference.
- Binary size: 3.0 MB (3172736 bytes) for the `sqler-cli` release binary. Target < 60 MB. This is the CLI, not the desktop app.
- Completion p95: n/a until M3.
- Test count: 71 tests run, 71 passed (`cargo nextest run --workspace --no-tests=pass`).
- Coverage: not measured (no coverage tool in M0; `cargo llvm-cov` is not installed).
### Risks & blockers
- ADR-0001 is still `proposed`; the GPUI spike is the first job of M1.
- The plugin-build and benchmark-regression CI jobs are deferred until `plugins/` (M6) and benchmarks exist.
- GitHub Pages needs to be enabled once by hand (Settings > Pages > Source: GitHub Actions) before `pages.yml` can deploy.
- All M0 numbers are for the CLI only, so the UI budgets (cold start, idle RSS, binary size of the desktop app) are unverified.
- The cold-start figures are on one OS only (macOS arm64); Linux and Windows rows are still to be added.
### Next iteration plan
M1 (UI foundation):
- GPUI spike; finalise ADR-0001.
- Workspace shell: connection sidebar, per-connection tab strip, command palette, settings, themes, keybindings.
- Results grid virtualised to 1M rows, with a benchmark.
- First UI idle-RSS and cold-start measurement.
- Add the bench-regression CI job once a benchmark exists.
### AGENTS.md changes
- Section 9: roadmap status changed to "M0 done" with M1 next, and a pointer to this report.
- Section 3: the "Bench: none yet" line now points to the measurement method in `docs/src/benchmarks.md`.
- Section 10 (Gotchas): no change; nothing demonstrably cost time twice during this iteration.
