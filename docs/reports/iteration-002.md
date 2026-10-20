## Iteration 002 — 2026-10-20
### Delivered
M1 (UI foundation), items in dependency order:
- Command registry with validated `category:action` IDs and fuzzy palette query (PR #7, `docs/src/architecture/ui.md`)
- `sqler-config`: settings, keybindings and saved connections files (PR #9, `docs/src/user-guide/settings.md`)
- Results grid data model, 1M-row viewport benchmark and `bench.yml` (PR #8, `docs/src/benchmarks.md`)
- GPUI framework spike, ADR-0001 accepted (PR #10, `docs/src/benchmarks.md` spike table)
- Desktop shell: `sqler-app` binary, connection sidebar, per-connection tabs, tokio bridge (PR #11, `docs/src/user-guide/workspace.md`, `docs/src/architecture/ui.md`)
- Settings tab and themes: light, dark, follow system, UI font size (PR #12, `docs/src/user-guide/settings.md`)
- Command palette and configurable keybindings (PR #13, `docs/src/user-guide/keybindings.md`)
- Results grid view: virtual table over the 1M-row model (PR #14, `docs/src/user-guide/results-grid.md`)
- Metrics and this report (PR [9/9], `docs/src/benchmarks.md`)
### Decisions
- ADR-0001 UI framework spike plan (accepted) — GPUI with `gpui-component` is the UI framework; the resolved crate is `gpui-pre` =0.3.9. The spike met the budgets on macOS, and on Linux and Windows CI runners (cold start 182 to 206 ms, first frame 185 to 2285 ms on software renderers).
- ADR-0005 Config files and keybindings (accepted) — TOML files, chord grammar, std-only directory lookup (`directories` rejected: MPL-2.0 transitive licence).
- ADR-0006 Grid data model and bench harness (accepted) — channel-fed chunked `GridModel`, std-only bench with p50/p95 and a regression rule in `bench.yml`.
- ADR-0007 Desktop binary and UI-thread/tokio bridge (accepted) — `sqler-app` at layer L7 is the only crate that depends on `sqler-ui`; `SessionBridge` runs driver work on tokio without touching the UI thread.
### Metrics
Measured on 2026-10-20, macOS 27.0.1 arm64 (Apple M4 Pro), commit `1f206e6`, `target/release/sqler` (default features, `--locked`). Tracked in [benchmarks](../src/benchmarks.md).
- Cold start: 140.6 ms mean (σ 12.8, min 132.7, max 191.1), `hyperfine` 20 runs after 3 warm-ups of `sqler --exit-after-first-frame`. Target < 500 ms: met on macOS.
- Idle RSS: 98.0 to 98.3 MiB (three runs, `ps` at 6 s and 10 s) with three `SQLite` `:memory:` connections in the sidebar. The three sessions were not opened, because an agent cannot double-click; the full "three connections open" figure is pending the maintainer. Target < 150 MB.
- Binary size: 29.5 MB (30,928,816 bytes) for the `sqler` release binary. Target < 60 MB: met. For reference the CI spike build was 57.7 MB on Linux, unstripped; the Linux `sqler` binary is not measured.
- Results grid, 1M rows (cited from `benchmarks.md`, run at `1dfec02` and `23a3f72`): viewport fetch p50 208 ns, p95 334 ns; frame data cost p50 9.4 µs, p95 9.8 µs; in-app probe 601 frames, frame interval p95 8.3 to 8.9 ms (120 Hz display), last row drawn; RSS 346.5 MB with the grid open (about 235 MB is result data), above the idle target by design and not gated.
- Completion p95: n/a until M3.
- Test count: 299 tests run, 299 passed (`cargo nextest run --workspace --no-tests=pass`), up from 71 at the end of M0.
- Coverage: not measured (`cargo llvm-cov` is not installed).
- Linux and Windows desktop numbers: not measured, because `ci.yml` has no GUI runner (assumed reason: the build-only CI installs no runtime display libraries).
### Risks & blockers
- M1 Definition of Done is not fully verified. Pending maintainer checks (interactive macOS, none can be done by an agent):
  - Workspace shell (PR #11): click and select in the sidebar, double-click opens a tab, two tabs on one connection, switch and close tabs, the prod connection look, plus the ADR-0001 open items (trackpad scroll, typing SQL, resizing the split).
  - Settings and themes (PR #12): toggle theme, follow system, change font size, confirm settings are restored after restart.
  - Palette and keybindings (PR #13): open the palette with the chord, filter, Up/Down, Enter runs the command, Esc closes, the conflict notification appears, rebind via `keybindings.toml` and restart.
  - Results grid (PR #14): smooth mouse and trackpad scrolling of the 1M-row grid, Top and End buttons, closing the tab.
  - Idle RSS with three sessions actually opened.
- The results grid with 1M rows holds about 235 MB and RSS reaches 346.5 MB. This is a memory risk for large results; M4 needs a bounded or spilled result model before real queries feed the grid.
- Linux and Windows `sqler` numbers are unmeasured; the Linux spike binary was 57.7 MB, close to the 60 MB budget.
- On quit, open sessions are dropped, not closed; M2 must close them.
- GPUI is a pre-release fork (`gpui-pre` =0.3.9) with MPL-2.0 exceptions and eight advisory ignores in `deny.toml`; upgrades need a spike-style check.
- GitHub Pages still needs enabling by hand.
### Next iteration plan
M2 (connections and drivers), after the maintainer verification above is recorded:
- `PostgreSQL` and `MySQL` drivers next to `SQLite`, behind `driver-*` features, with testcontainers tests.
- Connection manager: groups, colours, environments, keychain, SSH tunnel, TLS; editing connections in the app (M1 only reads `connections.toml`).
- `sqler-catalog` with lazy tree and background refresh; object browser with DDL generation.
- Close sessions on quit; feed the grid from real query streams; measure idle RSS with three real connections.
- Add Linux and Windows desktop measurements.
### AGENTS.md changes
- Section 9: roadmap status now "M1 done in code, interactive checks pending", M2 next, pointer to this report.
- Section 10 (Gotchas): macOS config isolation for measurements via `HOME`.
