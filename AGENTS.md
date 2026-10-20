# AGENTS.md

The SQLer project handbook (v1). This file is the single source of truth for contributors and agents.
`CLAUDE.md` only points here. Per-crate `AGENTS.md` files are allowed; they must link back to this one.

## 1. Purpose

SQLer is a lightweight, native, open-source (Apache-2.0) SQL IDE written in Rust. It is not a
general code editor with a database plugin: the database is the centre of the UI. Connections and
schema objects are first-class, and SQL editing is one activity among several.

It takes the best of three tools and leaves their weaknesses behind:

| Steal from | What | Avoid |
| ---------- | ---- | ----- |
| **Beekeeper Studio** | Clean, fast, uncluttered UX; tabs; quick connection switching; inline cell editing; schema-aware SQL autocompletion; "AI Shell" that learns the schema and can run queries to verify its own answers | Electron weight; JS runtime; closed Pro features |
| **Azure Data Studio** | Notebook-style results, excellent results grid (sort/filter/export/copy-as), connection groups, extension ecosystem, keyboard-first workflow, dashboards per connection | Being a fork of a general-purpose editor; retired by Microsoft in 2026 |
| **pgAdmin** | Deep object browser (schemas, tables, views, functions, triggers, roles, extensions), DDL generation for every object, EXPLAIN visualiser, query history, server-side object properties | Slow web UI; cluttered dialogs; Python/Flask stack |

Non-negotiables: Rust everywhere (edition 2024, stable toolchain pinned in `rust-toolchain.toml`);
lightweight (cold start < 500 ms, idle RSS < 150 MB with three connections, release binary < 60 MB);
fully open source (Apache-2.0, SPDX header in every source file); modular; newest libraries,
verified on crates.io; documentation is part of the deliverable; cross-platform (Linux, macOS,
Windows); no AI feature executes a write statement without explicit user confirmation.

## 2. Architecture

Cargo workspace. Layered; dependencies flow downward only. Members are `crates/*` and `xtask`.

| Crate | Layer | Responsibility |
| ----- | ----- | -------------- |
| `sqler-core` | L0 | Domain types: Connection, Catalog, Query, ResultSet, Value, DataType; no I/O |
| `sqler-driver-api` | L1 | `Driver` trait, streaming result protocol, cancellation, introspection trait |
| `sqler-ai-api` | L1 | `AiProvider` trait, message and tool types, streaming, token accounting |
| `sqler-sql` | L1 | SQL text services on an own lexer (statement splitting, cursor lookup, placeholders, fail-safe classification, formatting), dialect handling, DDL generation; `sqlparser` only for later AST needs (ADR-0020) |
| `sqler-plugin-api` | L1 | WIT definitions and host bindings (wasmtime component model) |
| `sqler-config` | L1 | Settings, keybindings, connection store, keychain |
| `sqler-tunnel` | L1 | SSH session and local port forward (russh); drivers connect to a local address |
| `sqler-driver-postgres` | L2 | `PostgreSQL` driver (sqlx / tokio-postgres) |
| `sqler-driver-mysql` | L2 | `MySQL` and `MariaDB` driver (sqlx) |
| `sqler-driver-sqlite` | L2 | `SQLite` driver |
| `sqler-driver-mssql` | L2 | SQL Server driver (tiberius) |
| `sqler-driver-duckdb` | L2 | `DuckDB` driver |
| `sqler-ai-ollama` | L2 | Ollama provider |
| `sqler-ai-anthropic` | L2 | Anthropic provider |
| `sqler-ai-openai` | L2 | OpenAI and any OpenAI-compatible endpoint |
| `sqler-ai-mistral` | L2 | Mistral provider |
| `sqler-catalog` | L2 | Schema cache, incremental refresh, search index, dependency graph |
| `sqler-completion` | L2 | Intellisense engine: scope analysis, alias resolution, ranking; pure functions, heavily tested |
| `sqler-export` | L2 | CSV, JSON, SQL, Parquet and XLSX writers |
| `sqler-ai-tools` | L3 | Tool set exposed to models: list_schemas, describe_table, run_query (guarded), search_data, explain |
| `sqler-plugin-host` | L3 | Plugin loading, sandboxing, capability grants, lifecycle |
| `sqler-mcp` | L4 | MCP server (rmcp) exposing `sqler-ai-tools` to external agents |
| `sqler-acp` | L5 | ACP client: spawn and host Claude Code, Codex CLI, Gemini CLI and others inside SQLer |
| `sqler-ui` | L6 | GPUI app: workspace, tabs, panels, grid, editor, palette, themes |
| `sqler-cli` | L6 | Headless CLI (run query, export, start MCP server); proves the core is UI-independent |
| `sqler-app` | L7 | Desktop binary `sqler`: tokio runtime, driver registry, opens the `sqler-ui` window (ADR-0007) |
| `xtask` | tooling | Repository automation (generate WIT bindings, release, bench) |

Layering: a crate may depend only on crates in lower layers.

```
L0 core
L1 driver-api, ai-api, sql, plugin-api, config, tunnel
L2 driver-*, ai-ollama/anthropic/openai/mistral, catalog, completion, export
L3 ai-tools, plugin-host
L4 mcp
L5 acp
L6 ui, cli
L7 app
```

Rules:

- Only `sqler-app` depends on `sqler-ui`; driver crates are wired only by binaries (`sqler-app`, `sqler-cli`). Check:
  `cargo tree --workspace -i sqler-ui` must show `sqler-ui` and `sqler-app` only (CI "Dependency rules" step).
- `sqler-core` and `sqler-completion` have no async, no I/O, no UI: pure and exhaustively unit-tested.
- Drivers communicate through `sqler-driver-api` only. The UI never imports a driver crate.
- Every long-running operation is cancellable and reports progress through a channel; the UI thread never blocks.
- The public API of every crate is documented (`missing_docs` is a workspace lint).
- Drivers, AI providers and plugins are behind traits and can be compiled out with feature flags.

## 3. Commands

```sh
cargo build --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
cargo test --workspace
cargo nextest run --workspace --no-tests=pass
cargo deny check
mdbook build docs
sh .github/scripts/check-spdx.sh
cargo tree --workspace -i sqler-ui
cargo run -p sqler-cli -- run :memory: "select 1"
cargo run -p sqler-cli -- run --driver postgres "$SQLER_TEST_POSTGRES_URL" "select 1"
cargo nextest run --workspace -E 'binary(/^db_/)' --no-tests=pass
cargo run -p sqler-app
cargo tree --workspace -i gpui-pre
cargo run -p xtask
cargo bench -p sqler-ui --bench grid_viewport
cargo bench -p sqler-catalog --bench catalog_tree
cargo bench -p sqler-catalog --bench catalog_index
cargo bench -p sqler-ui --bench browser_tree
```

- CI (`.github/workflows/ci.yml`) runs this subset of the commands above: fmt, clippy, nextest on Linux/macOS/Windows; deny, docs (`cargo doc` and `mdbook build docs`), spdx on Linux. Since T5 the Linux `check` and `docs` jobs install the GPUI build packages with apt, and the macOS `check` job guards the Metal toolchain; the `deny` job also runs the "Dependency rules" step.
- DB tests (ADR-0009): `crates/sqler-driver-postgres/tests/docker/start-postgres.sh <outdir>` starts `PostgreSQL` with TLS in Docker (container `sqler-test-postgres`, `127.0.0.1:54329`) and prints `KEY=value` lines; locally use `eval "$(sh crates/sqler-driver-postgres/tests/docker/start-postgres.sh /tmp/pg | sed 's/^/export /')"`. The tests read `SQLER_TEST_POSTGRES_URL` and `SQLER_TEST_POSTGRES_CA` (the pattern is `SQLER_TEST_<DB>_URL`/`_CA`); when the URL is unset they print `skipped: <VAR> is not set (see AGENTS.md §3)` and pass. `SQLER_TEST_REQUIRE_DB=postgres` (a comma list of ids) turns a missing variable into a panic. Test files are `tests/db_<db>*.rs`, so the filter above selects them. Remove the local container with `docker rm -f sqler-test-postgres`. `MySQL` and `MariaDB` (ADR-0010): `sh crates/sqler-driver-mysql/tests/docker/start-mysql.sh <outdir> [mysql|mariadb]` (container `sqler-test-mysql`, `127.0.0.1:33069`, images `mysql:8.4` and `mariadb:11.8`) prints `SQLER_TEST_MYSQL_URL` and `SQLER_TEST_MYSQL_CA`; run the tests once per variant; remove with `docker rm -f sqler-test-mysql`.
- CI has a Linux `db` job (no GPUI apt packages) that starts the server with the script, sets `SQLER_TEST_REQUIRE_DB=postgres,mysql`, runs `cargo nextest run -p sqler-driver-postgres --no-tests=fail`, then the same for `sqler-driver-mysql` against `MySQL` 8.4 and `MariaDB` 11.8, and CLI smoke tests with `--driver postgres` and, for each of `MySQL` and `MariaDB`, `--driver mysql`.
- SPDX check (`sh .github/scripts/check-spdx.sh`) prints the offending paths and exits 1 when a tracked `*.rs` file lacks the header on line 1; it prints nothing otherwise.
- CI tools (pinned, see `docs/src/dependencies.md`): `cargo install --locked cargo-nextest@0.9.148 cargo-deny@0.20.2 mdbook@0.5.4`.
- `.github/workflows/bench.yml` runs the grid viewport, catalog tree and catalog index benchmarks on Linux for pull requests (base vs PR in one job).
- `.github/workflows/pages.yml` publishes the book (`target/book/`) to GitHub Pages on push to `main`.
- Bench: `cargo bench -p sqler-ui --bench grid_viewport -- [--rows N --columns N --batch N --viewport N --samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-p95-ns B]`. Rule: p95 more than 25 % + 50 ns above the base branch, or p95 above 1 ms, fails `bench.yml`. Method and results: `docs/src/benchmarks.md`. The release CLI is still measured by hand as described there.
- Catalog bench: `cargo bench -p sqler-catalog --bench catalog_tree -- [--tables N --columns N --samples N --hit-samples N --source fake|sqlite --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-hit-p95-ns B --budget-expand-p95-ns B]`. Defaults: 5000 tables, 8 columns, `fake`. Budgets (`fake` only): cached-read p95 10 us, cold expand of 5,000 tables p95 16 ms. `--compare` checks `hit_p95_ns` (25 % + 50 ns). Exit 0 pass, 1 regression or budget, 2 usage error. Wired into `bench.yml`.
- Catalog index bench: `cargo bench -p sqler-catalog --bench catalog_index -- [--tables N --columns N --loaded N --samples N --lookup-samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-snapshot-p95-ns B --budget-prefix-p95-ns B --budget-fuzzy-p95-ns B --budget-index-bytes B]`. Defaults: 5000 tables (plus N/10 views and functions), 8 columns, 50 loaded tables. Budgets: snapshot p95 10 us, prefix lookup p95 1 ms, fuzzy lookup p95 5 ms, index 4 MiB. `--compare` checks `prefix_p95_ns` (25 % + 50 ns). Exit 0 pass, 1 regression or budget, 2 usage error. Wired into `bench.yml`.
- Browser tree bench: `cargo bench -p sqler-ui --bench browser_tree -- [--tables N --samples N --select-samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-expand-p95-ns B --budget-select-p95-ns B]`. Defaults: 5000 tables. Budgets: expand of a 5,000-table schema p95 16 ms (`expand_p95_ns`), selection change p95 1 ms (`select_p95_ns`). `--compare` checks `expand_p95_ns` (25 % + 50 ns). Exit 0 pass, 1 regression or budget, 2 usage error. Wired into `bench.yml`.

## 4. Coding standards

- `#![forbid(unsafe_code)]` in every crate except where FFI is unavoidable (drivers, wasmtime glue); document each exception.
- Workspace-level lints: `clippy::pedantic` as warn, selected lints as deny; `missing_docs` warn on public crates.
- No `unwrap`/`expect` outside tests and `main`; use typed errors.
- Async: never hold a lock across an `.await`; never block the UI executor; every spawned task has a cancellation path.
- UI: every user action is a `Command` with an ID; no business logic in views; views render state, actions mutate state.
- Tests: unit tests next to code; integration tests under `tests/`; DB tests (`tests/db_<db>*.rs`) start no containers themselves: a script per database starts one, the tests read `SQLER_TEST_<DB>_URL` and skip when it is unset (see §3).
- Performance: any PR touching the grid, catalog or completion must include a benchmark result in its description.
- Naming: crates `sqler-*`, features `driver-*`, `ai-*`; commands `category:action`, where action is one or more hyphenated lowercase segments (`app:quit`, `workspace:open-command-palette`).

Workspace lints (root `Cargo.toml`, inherited with `[lints] workspace = true`):

| Lint | Level |
| ---- | ----- |
| `unsafe_code` (rust) | forbid |
| `missing_docs` (rust) | warn |
| `clippy::pedantic` (priority -1) | warn |
| `clippy::unwrap_used`, `clippy::expect_used` | deny (allowed in tests via `clippy.toml`) |
| `clippy::dbg_macro`, `clippy::todo` | deny |

`unwrap`/`expect` are denied workspace-wide and relaxed only in tests (`clippy.toml`); `main` must use an explicit `#[allow(clippy::unwrap_used, clippy::expect_used)]` if it needs them.

Every source file starts with the SPDX header:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors
```

## 5. Definition of Done

A task is done only when:

- code is merged to `main`;
- tests exist and pass on all targets;
- public items are documented;
- user-facing behaviour is documented in the book;
- a CHANGELOG entry is written;
- benchmarks are within thresholds;
- no new `unsafe` without a `// SAFETY:` comment and a reviewer sign-off;
- no new dependency without a recorded version and rationale.

## 6. Iteration workflow

1. **Plan**: read this file and the roadmap; pick the next milestone items; write them as GitHub issues or tasks with acceptance criteria.
2. **Decide**: a new dependency, crate, public API or cross-cutting change needs an ADR first.
3. **Build**: small PRs; tests alongside code; docs alongside tests.
4. **Verify**: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --workspace`, `cargo deny check`, warning-free `cargo doc`, plugin build, benchmarks not regressed. Run on all three OSes when the change touches platform code.
5. **Document**: user guide, architecture page, CHANGELOG, this file (commands, gotchas, roadmap status).
6. **Report** (template below).
7. **Reflect**: anything that cost time twice goes into the Gotchas section.

Git: Conventional Commits; small PRs; English only; no AI attribution in commits or PRs.

End-of-iteration report, posted as `docs/reports/iteration-NNN.md`:

```markdown
## Iteration NNN — <date>
### Delivered
- <item> (PR #, docs page)
### Decisions
- ADR-00xx <title> — <one-line summary>
### Metrics
- cold start / idle RSS / binary size / completion p95 / test count / coverage
### Risks & blockers
### Next iteration plan
### AGENTS.md changes
```

## 7. Docs

- Documentation lives under `docs/` as an mdBook (`docs/src/`). The book is configured in `docs/book.toml`, with the table of contents in `docs/src/SUMMARY.md`. Build it with `mdbook build docs`; the output goes to `target/book/` (the Pages job deploys that directory). mdBook 0.5.4 (latest stable on crates.io, checked 2026-10-20). A new page needs a SUMMARY line.
- `docs/src/dependencies.md` is the dated dependency version survey and the version policy. Update it (or an ADR) with every dependency add or bump.
- `CHANGELOG.md` follows Keep a Changelog. Every user-visible change adds an entry under `## [Unreleased]`.
- Iteration reports go to `docs/reports/iteration-NNN.md`; benchmarks go to `docs/src/benchmarks.md`.
- A feature without docs is not done.

## 8. ADR process

- Format: MADR. Files: `docs/src/adr/NNNN-title.md`. The full process is in [ADR-0000](docs/src/adr/0000-adr-process.md).
- ADRs are never deleted. A changed decision is recorded by a new ADR that supersedes the old one.
- Write the ADR before introducing a new dependency, crate, public API or cross-cutting change.

| ADR | Title | Status |
| --- | ----- | ------ |
| [0000](docs/src/adr/0000-adr-process.md) | ADR process | accepted |
| [0001](docs/src/adr/0001-ui-framework-spike-plan.md) | UI framework spike plan | accepted |
| [0002](docs/src/adr/0002-plugin-abi-wasm-component-model.md) | Plugin ABI: WASM Component Model | accepted |
| [0003](docs/src/adr/0003-ai-tool-contract.md) | AI tool contract | accepted |
| [0004](docs/src/adr/0004-driver-api.md) | Driver API: boxed futures, event stream, session cancellation | accepted |
| [0005](docs/src/adr/0005-config-files-and-keybindings.md) | Config files and keybindings: TOML files, chord grammar, std-only directory lookup | accepted |
| [0006](docs/src/adr/0006-grid-data-model-and-bench-harness.md) | Grid data model and bench harness | accepted |
| [0007](docs/src/adr/0007-desktop-binary-and-ui-tokio-bridge.md) | Desktop binary and UI-thread/tokio bridge | accepted |
| [0008](docs/src/adr/0008-object-model-lazy-introspection-value-types-tls.md) | Object model, lazy introspection, typed values and TLS options | accepted |
| [0009](docs/src/adr/0009-postgres-driver-tls-db-tests.md) | `PostgreSQL` driver: sqlx, server-side cancel, rustls TLS, DB test convention | accepted |
| [0010](docs/src/adr/0010-mysql-driver.md) | `MySQL` and `MariaDB` driver: sqlx, `KILL QUERY` cancel, no RSA password exchange | accepted |
| [0011](docs/src/adr/0011-connection-store-format.md) | Connection store format: versioned TOML, groups, derived secret references | accepted |
| [0012](docs/src/adr/0012-keychain-secrets.md) | Keychain secrets: `keyring` 4.x, reference scheme, failure policy | accepted |
| [0013](docs/src/adr/0013-ssh-tunnel-crate.md) | SSH tunnel crate: russh, strict known_hosts, loopback-only forward | accepted |
| [0014](docs/src/adr/0014-catalog-cache-and-background-refresh.md) | Catalog cache, worker concurrency and background refresh | accepted |
| [0015](docs/src/adr/0015-ddl-generation-dialects-and-quoting.md) | DDL generation: dialects and quoting | accepted |
| [0016](docs/src/adr/0016-connect-pipeline.md) | Connect pipeline: `Connector` trait in `sqler-ui`, pipeline in `sqler-app` | accepted |
| [0017](docs/src/adr/0017-object-browser-sessions-and-drop-confirmation.md) | Object browser: browser session per connection, short-lived action sessions, DROP confirmation | accepted |
| [0019](docs/src/adr/0019-tls-server-name-through-tunnel.md) | TLS server name through a tunnel: `TlsConfig::server_name`, local Unix-socket bridge | accepted |
| [0020](docs/src/adr/0020-sql-text-services.md) | SQL text services: own lexer, fail-safe classification | accepted |
| [0022](docs/src/adr/0022-catalog-snapshot-name-index.md) | Catalog snapshot, name index and on-demand column loading | accepted |

## 9. Roadmap status

Current: **M2 code complete; Definition of Done pending maintainer verification and product-owner decisions (see lists); M3 next** (editor and intellisense). See `docs/reports/iteration-003.md`. Per the rule below, M3 starts only after the lists are cleared or the owner explicitly waives them.

M2 delivered the keychain secret store, the editable connection store, the object model with lazy introspection and typed values, the SSH tunnel crate, DDL generation, the catalog cache, the `PostgreSQL` and `MySQL`/`MariaDB` drivers, the connection manager with the connect pipeline, and the object browser (ADR-0008 to ADR-0017). Measured at `ba24c9a` on macOS: cold start 155.1 ms, idle RSS 100.0 MiB with three connections configured, binary 38.3 MiB, 814 tests.

Pending maintainer-only checks (need a display, an OS desktop session or a real server; none can be done by an agent):

- M1 (iteration-002 report): interactive checks of the workspace shell (PR #11), settings and themes (#12), palette and keybindings (#13) and grid scrolling (#14).
- Idle RSS with three sessions actually opened (the app has no headless way to open sessions; 100.0 MiB was measured with three configured).
- Keychain (PR #16): `cargo test -p sqler-config -- --ignored keychain_round_trip` on a desktop session of macOS, Windows and Linux, and a look at the OS keychain UI.
- SSH bastion (PR #19): `SQLER_TEST_BASTION_*` set and `cargo test -p sqler-tunnel --test real_bastion -- --ignored`; the Windows named-pipe agent code is compiled by CI only.
- Connection manager (PR #24): masked inputs and focus when a dialog opens, Escape with an input focused, drag preview, keychain prompts on the three OSes, the SSH host-key prompt with a real bastion.
- Object browser (PR #25): context menus per node kind, the DROP dialog and the second `prod` step, read-only DDL and Properties tabs with Copy, scrolling a 5,000-table schema, keyboard navigation, copy to the clipboard.

Pending product-owner decisions (details in the iteration-003 report): RSA SSH keys (RUSTSEC-2023-0071, `russh` `rsa` feature), plaintext RSA password exchange for `MySQL` (`mysql-rsa` feature, same advisory), `verify-full` TLS through an SSH tunnel (needs a server-name field on the core `TlsConfig`). Known limits: the `MySQL` cancel race window and the "Dropping..." step that blocks the window.

M0 and M1 are described in `docs/reports/iteration-001.md` and `docs/reports/iteration-002.md`.

M0 covers the workspace skeleton, this handbook, the mdBook skeleton, CI on three OSes, `cargo-deny`,
license headers, ADR-0000 to ADR-0004, and a `sqler-cli` that runs a query on `SQLite`.
Later milestones: M1 UI foundation, M2 connections and drivers, M3 editor and intellisense,
M4 results and editing, M5 AI, M6 plugins, M7 release 1.0. Do not start a milestone before the
previous one's Definition of Done is met.

M0 CI is in place (`ci.yml`, `pages.yml`, `deny.toml`) except the plugin-build job, which is deferred (see Gotchas); `bench.yml` covers benchmark regressions.

## 10. Gotchas

- `clippy::doc_markdown` (pedantic) flags product names such as `SQLite`, `DuckDB` and `PostgreSQL` in docs; put them in backticks.
- The `pedantic` group needs `priority = -1`, or `clippy::lint_groups_priority` fires.
- A crate with `[lints] workspace = true` cannot override single lints. An FFI exception to `unsafe_code` needs its own `[lints]` table, documented.
- `cargo nextest` fails when there are no tests unless you pass `--no-tests=pass`.
- The crates.io API rejects requests without a User-Agent header.
- Never pin a version from memory; check crates.io. Check `max_stable_version`, not `max_version` (it can be a pre-release).
- The `cargo-dist` tool was renamed `dist`, but the `dist` crate on crates.io is unrelated; the crate is still `cargo-dist`.
- `clippy::unwrap_used`/`clippy::expect_used` are denied everywhere except tests; there is no exemption for `main`, so add an `allow` attribute there if needed.
- GitHub Pages must be enabled once by hand (Settings > Pages > Source: GitHub Actions) before `pages.yml` can deploy.
- The plugin-build CI job is deferred until `plugins/` (M6).
- `cargo bench` passes `--bench` to `harness = false` binaries; their argument parser must ignore it.
- CI installs the toolchain with a bare `rustup toolchain install`, which reads `rust-toolchain.toml` (rustup 1.28 or newer), so the Rust version is pinned in one place only.
- UI framework spike (ADR-0001): GPUI is accepted. The resolved GPUI crate is `gpui-pre` (=0.3.9, pulled by `gpui-component` 0.8.0), not `gpui` or `gpui-ce`; the check is `cargo tree -i gpui-pre`, since `cargo tree -i gpui` can never match. Application creation needs `gpui-pre-platform` (features `font-kit`, `x11`, `wayland`); `gpui-component` does not re-export it.
- Spike Linux apt packages: `libfontconfig-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev libvulkan1 mesa-vulkan-drivers vulkan-tools fonts-dejavu-core xvfb xauth cage pkg-config`. Software Vulkan is lavapipe (`mesa-vulkan-drivers`); the renderer shows as `llvmpipe` and GPUI runs on it with no extra setup.
- Headless Wayland in CI needs a `wl_seat`: `weston --backend=headless` has none and GPUI panics (`Wayland compositor does not provide wl_seat`). Use `cage` with `WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 WLR_HEADLESS_OUTPUTS=1`, unset `DISPLAY`, and check that the report says `display=wayland`.
- macOS: the GPUI build needs the Metal toolchain (`xcodebuild -downloadComponent MetalToolchain`, about 840 MB; the `macos-latest` image already has it). Windows CI renders with WARP (software D3D). On shared CI VMs first-frame times vary up to about 3.6x between runs; cold start is stable.
- macOS: a GPUI window may not present frames while the display is idle or the window is hidden, so smoke runs can stall (two manual runs ran 84 s and 120 s). Keep a watchdog and treat a one-off watchdog failure as environment, not as a GPUI failure.
- `hyperfine` 2.0 JSON has the mean at `results[0].summary.time_wall_clock.mean`. On Windows run it with `-N` and a quoted path, or it fails.
- Open follow-up from the first UI PR (T5): the 2-minute interactive macOS check has not been done. It covers the spike items (trackpad scroll, typing SQL, resizing the split) and the workspace shell (click and double-click a connection, two tabs on one connection, switch, close, the prod look). An agent cannot do it; the maintainer does it and records the result.
- The apt packages in `ci.yml` are the build-only set (`pkg-config libfontconfig-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev`); no runtime libraries are installed because no test opens a window.
- `cargo test -p sqler-ui` is GPUI-free (the views need the `gui` feature, which only `sqler-app` enables). `cargo clippy --workspace --all-features` and `--workspace` builds do compile GPUI.
- Never call `tokio::spawn` on the GPUI thread: there is no runtime context. Use `SessionBridge` (`Handle::spawn` plus a `oneshot` awaited with `cx.spawn`), see ADR-0007.
- `sqler --exit-after-first-frame` exits with code 0 after the first frame (cold-start measurement). On quit, open sessions are closed (ADR-0016): `app:quit` closes them under a 2 s bound; the window-close hook only gets about 150 ms.
- Measuring the desktop app on macOS: the config directory is `$HOME/Library/Application Support/SQLer`, so run `sqler` with `HOME` pointed at a scratch directory to get a known `connections.toml` and leave the real config alone. `hyperfine` is at `/opt/homebrew/bin`, `mdbook` and `cargo-nextest` are in `~/.cargo/bin` and may be off `PATH`.
- Secrets (T5, ADR-0012): headless Linux CI has no Secret Service, so tests use `MemoryStore`; `KeychainStore` is only checked by hand with `cargo test -p sqler-config -- --ignored keychain_round_trip` on a desktop session. `keyring` 4.x has no native-backend features of its own: enable `v1`, which turns on the Keychain, Credential Manager and `zbus` Secret Service (`crypto-rust`, `async-io`) stores per target. Never use keyutils, `vendored` `libdbus`, OpenSSL crypto, a `tokio` runtime feature or a mock store. `SecretStore` calls block; async callers use `spawn_blocking`.
- `PostgreSQL` test server (ADR-0009): the server rejects a TLS key it does not own or that is group/world readable, so the script copies the key inside the container, runs `chown postgres` and `chmod 600`, then starts the server. GitHub `services:` cannot pass server arguments (`-c ssl=on`), hence the `docker run` script. The image's init server listens on the socket only; wait with `pg_isready -h 127.0.0.1`, not the socket.
- `PgConnectOptions` has a `Debug` that prints the password: never derive `Debug` on a struct that holds it and never log it (`PgCanceller` has a hand-written `Debug`). Driver errors must not echo the target.
- `MySQL` test server (ADR-0010): `sqlx` is built without `mysql-rsa`, so a plaintext login to an account that is not cached by `caching_sha2_password` fails; the tests connect over TLS first. Text in a `_bin` collation and `MariaDB` `JSON` arrive as `Bytes`; `sqlx` reports a zero date as NULL, so the driver reads date columns as raw bytes. `KILL QUERY` needs a side connection; a killed `SLEEP()` returns 1, not an error.
- A DB test that skips is green: without `SQLER_TEST_REQUIRE_DB` a missing variable only prints a skip line. CI sets it so the `db` job cannot pass without running. An unreachable server always fails.
- `sqler-tunnel` (ADR-0013): `russh` is built with `ring` only. RSA private keys are unsupported (`TunnelError::UnsupportedKeyType`) until the owner decides on RUSTSEC-2023-0071; re-enabling is the `rsa` feature on `russh` plus an advisory ignore in `deny.toml`. Host keys are never accepted silently: use `trust_host_key` after the user confirms the fingerprint. Tests use an in-process `russh` server, no Docker.
- Connect pipeline (ADR-0016): `verify-full` TLS through an SSH tunnel is refused (`TlsHostnameThroughTunnel`; an explicit TLS mode overrides the URL `sslmode`/`ssl-mode`, and a URL verify-full counts only without an explicit mode) because the driver would verify `127.0.0.1`; lifting it needs a server-name field on core `TlsConfig` plus driver support. A `Tunnel` must not be dropped outside a tokio runtime: `TunnelledSession` closes it on the runtime handle. A stored target never has user info; the database user is `?user=` and the password is injected at connect time (a password with a target that has no user is refused). `sqler-ui` talks to the pipeline only through the `Connector` trait.
- Object browser (ADR-0017): `ObjectTree` never calls the catalog; `expand` and `*_loaded` return `CacheKey`s and the workspace turns them into `Effect::LoadCatalog`. Tokio work still goes only through `SessionBridge`. DROP SQL comes only from `DropFlow::confirm` (once, twice on `prod`); nothing else may build a `RunJob(Drop)`. `gpui-component` 0.8.0 `ContextMenuExt` cannot open a menu from code, so `object:show-menu` (Shift+F10, Menu) opens the palette filtered to `Object: `. The sidebar is a `gpui::list` with a `ListState` that is diffed in `settle`; do not rebuild rows per frame. `Up`/`Down` walk groups, connections and tree rows through `Workspace::move_selection`.
