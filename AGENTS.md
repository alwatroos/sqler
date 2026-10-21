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
| `sqler-export` | L2 | CSV, JSON, SQL `INSERT`, Markdown, Parquet and XLSX writers behind a push-style `RowWriter` API and a format registry (ADR-0028); Parquet and XLSX sit behind the cargo features `parquet` and `xlsx` (ADR-0029) |
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
cargo build --release -p sqler-app --locked
target/release/sqler --open-sessions
cargo tree --workspace -i gpui-pre
cargo run -p xtask
cargo bench -p sqler-ui --bench grid_viewport
cargo bench -p sqler-ui --bench grid_sort
cargo bench -p sqler-catalog --bench catalog_tree
cargo bench -p sqler-catalog --bench catalog_index
cargo bench -p sqler-ui --bench browser_tree
cargo bench -p sqler-completion --bench completion
cargo bench -p sqler-ui --bench editor_buffer
cargo bench -p sqler-ui --bench editor_intellisense
cargo bench -p sqler-export --bench export_throughput
```

- CI (`.github/workflows/ci.yml`) runs this subset of the commands above: fmt, clippy, nextest on Linux/macOS/Windows; deny, docs (`cargo doc` and `mdbook build docs`), spdx on Linux. Since T5 the Linux `check` and `docs` jobs install the GPUI build packages with apt, and the macOS `check` job guards the Metal toolchain; the `deny` job also runs the "Dependency rules" step.
- DB tests (ADR-0009): `crates/sqler-driver-postgres/tests/docker/start-postgres.sh <outdir>` starts `PostgreSQL` with TLS in Docker (container `sqler-test-postgres`, `127.0.0.1:54329`) and prints `KEY=value` lines; locally use `eval "$(sh crates/sqler-driver-postgres/tests/docker/start-postgres.sh /tmp/pg | sed 's/^/export /')"`. The tests read `SQLER_TEST_POSTGRES_URL` and `SQLER_TEST_POSTGRES_CA` (the pattern is `SQLER_TEST_<DB>_URL`/`_CA`); when the URL is unset they print `skipped: <VAR> is not set (see AGENTS.md §3)` and pass. `SQLER_TEST_REQUIRE_DB=postgres` (a comma list of ids) turns a missing variable into a panic. Test files are `tests/db_<db>*.rs`, so the filter above selects them. Remove the local container with `docker rm -f sqler-test-postgres`. `MySQL` and `MariaDB` (ADR-0010): `sh crates/sqler-driver-mysql/tests/docker/start-mysql.sh <outdir> [mysql|mariadb]` (container `sqler-test-mysql`, `127.0.0.1:33069`, images `mysql:8.4` and `mariadb:11.8`) prints `SQLER_TEST_MYSQL_URL` and `SQLER_TEST_MYSQL_CA`; run the tests once per variant; remove with `docker rm -f sqler-test-mysql`. SSH tunnel RSA tests (ADR-0018, gate id `ssh`): `sh crates/sqler-tunnel/tests/docker/start-sshd.sh <outdir>` (container `sqler-test-sshd`, Alpine OpenSSH; sshd on `127.0.0.1:33256` limited to `rsa-sha2-256` and `33512` to `rsa-sha2-512`) generates RSA test keys with `ssh-keygen` and prints `SQLER_TEST_SSH_HOST`, `SQLER_TEST_SSH_PORT_SHA256`, `SQLER_TEST_SSH_PORT_SHA512`, `SQLER_TEST_SSH_USER`, `SQLER_TEST_SSH_KNOWN_HOSTS`, `SQLER_TEST_SSH_KEY_RSA`, `SQLER_TEST_SSH_KEY_RSA_PEM`, `SQLER_TEST_SSH_KEY_RSA_PKCS8`, `SQLER_TEST_SSH_KEY_RSA_ENC` and `SQLER_TEST_SSH_KEY_PASSPHRASE`; `tests/ssh_rsa.rs` skips without them; remove with `docker rm -f sqler-test-sshd`.
- CI has a Linux `db` job (no GPUI apt packages) that starts the server with the script, sets `SQLER_TEST_REQUIRE_DB=postgres,mysql,ssh`, runs `cargo nextest run -p sqler-driver-postgres --no-tests=fail`, then the same for `sqler-driver-mysql` against `MySQL` 8.4 and `MariaDB` 11.8, a step that starts the SSH server and runs `cargo nextest run -p sqler-tunnel --no-tests=fail`, and CLI smoke tests with `--driver postgres` and, for each of `MySQL` and `MariaDB`, `--driver mysql`.
- SPDX check (`sh .github/scripts/check-spdx.sh`) prints the offending paths and exits 1 when a tracked `*.rs` file lacks the header on line 1; it prints nothing otherwise. It skips `vendor/` (third-party code that keeps its upstream licence files, ADR-0018).
- `vendor/` holds third-party source copied in for a `[patch.crates-io]` entry, today only `vendor/sqlx-mysql` (sqlx-mysql 0.9.0 plus an `aws-lc-rs` backend, ADR-0018). It is not a workspace member, so fmt and clippy skip it, and it gets no SPDX headers. Its README states the maintenance procedure; no other change belongs there.
- CI tools (pinned, see `docs/src/dependencies.md`): `cargo install --locked cargo-nextest@0.9.148 cargo-deny@0.20.2 mdbook@0.5.4`.
- `.github/workflows/bench.yml` runs the grid viewport, grid sort, catalog tree, catalog index, browser tree and completion benchmarks on Linux for pull requests (base vs PR in one job).
- `.github/workflows/size.yml` reports the release `sqler` size on Linux/macOS/Windows (main, `workflow_dispatch`, PRs touching `crates/**`, `Cargo.toml`, `Cargo.lock`, `vendor/**`); report only, budget 60 MB.
- `.github/workflows/pages.yml` publishes the book (`target/book/`) to GitHub Pages on push to `main`.
- Bench: `cargo bench -p sqler-ui --bench grid_viewport -- [--rows N --columns N --batch N --viewport N --samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-p95-ns B]`. Rule: p95 more than 25 % + 50 ns above the base branch, or p95 above 1 ms, fails `bench.yml`. Method and results: `docs/src/benchmarks.md`. The release CLI is still measured by hand as described there.
- Grid sort bench: `cargo bench -p sqler-ui --bench grid_sort -- [--rows N --columns N --samples N --window-samples N --reps N --save PATH --compare PATH --threshold-pct P --sort-slack-ns S --filter-slack-ns S --viewport-slack-ns S --budget-sort-ns B --budget-filter-ns B --budget-viewport-ns B --budget-slice-ns B --budget-sort-slow-ns B --budget-sort-url-ns B]`. Defaults: 1M x 6 synthetic rows, 5 full-job samples, 3 repetitions (minimum). Budgets: text sort p95 1.5 s, quick filter p95 500 ms, 50-row window of the sorted order p95 1 ms, longest 6 ms step 16 ms; plus decimal, mixed int/float and URL-text columns (sort 500 ms, 500 ms, 5 s; longest step 16 ms each). `--compare` checks `sort_p95_ns` (25 % + 20 ms), `filter_p95_ns` (25 % + 10 ms) and `viewport_sorted_p95_ns` (25 % + 500 ns) only for keys the baseline file has. Exit 0 pass, 1 regression or budget, 2 usage error. Wired into `bench.yml`.
- Catalog bench: `cargo bench -p sqler-catalog --bench catalog_tree -- [--tables N --columns N --samples N --hit-samples N --source fake|sqlite --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-hit-p95-ns B --budget-expand-p95-ns B]`. Defaults: 5000 tables, 8 columns, `fake`. Budgets (`fake` only): cached-read p95 10 us, cold expand of 5,000 tables p95 16 ms. `--compare` checks `hit_p95_ns` (25 % + 50 ns). Exit 0 pass, 1 regression or budget, 2 usage error. Wired into `bench.yml`.
- Catalog index bench: `cargo bench -p sqler-catalog --bench catalog_index -- [--tables N --columns N --loaded N --samples N --lookup-samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-snapshot-p95-ns B --budget-prefix-p95-ns B --budget-fuzzy-p95-ns B --budget-index-bytes B]`. Defaults: 5000 tables (plus N/10 views and functions), 8 columns, 50 loaded tables. Budgets: snapshot p95 10 us, prefix lookup p95 1 ms, fuzzy lookup p95 5 ms, index 4 MiB. `--compare` checks `prefix_p95_ns` (25 % + 50 ns). Exit 0 pass, 1 regression or budget, 2 usage error. Wired into `bench.yml`.
- Completion bench: `cargo bench -p sqler-completion --bench completion -- [--tables N --columns N --samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-p95-ns B]`. Defaults: 5000 tables, 8 columns, 2000 samples per scenario, threshold 25 %, slack 250 us (`--slack-ns 250000`; the p95 is in the hundreds of microseconds), 3 in-process repetitions (`--reps`), budget 16 ms. Prints `key=value` lines (`<scenario>_p95_ns`, `complete_p95_ns`, `hover_p95_ns`, `diagnose_p95_ns`, `worst_p95_ns`). Exit 1 when `complete_p95_ns`, `hover_p95_ns`, `diagnose_p95_ns` or `worst_p95_ns` exceeds the budget, or `complete_p95_ns` (the minimum over the 3 repetitions), and `hover_p95_ns` and `diagnose_p95_ns` if the baseline has them, regress against `--compare` (25 % + 250 us); exit 2 on a usage error or unreadable baseline. Wired into `bench.yml`.
- Editor bench: `cargo bench -p sqler-ui --bench editor_buffer -- [--lines N --samples N --budget-p95-ns B]`. Defaults: 10000 lines, budget 16 ms. Prints `bracket_build_p95_ns`, `bracket_lookup_p95_ns`, `format_p95_ns`, `occurrence_p95_ns`. Exit 0 pass, 1 when the bracket build or format p95 exceeds the budget, 2 usage error. Not in `bench.yml`; put the numbers in the PR body.
- Intellisense bench: `cargo bench -p sqler-ui --bench editor_intellisense -- [--tables N --columns N --samples N --budget-p95-ns B]`. Defaults: 5000 tables, 8 columns, 2000 samples, budget 16 ms. Prints `statement_start_p95_ns`, `from_prefix_p95_ns`, `qualified_column_p95_ns`, `alias_join_p95_ns`, `hover_p95_ns`, `diagnose_p95_ns`, `worst_p95_ns` (headless, from a fresh snapshot to items ready). Exit 0 pass, 1 when `worst_p95_ns` exceeds the budget, 2 usage error. Not in `bench.yml`; put the numbers in the PR body.
- Browser tree bench: `cargo bench -p sqler-ui --bench browser_tree -- [--tables N --samples N --select-samples N --save PATH --compare PATH --threshold-pct P --slack-ns S --budget-expand-p95-ns B --budget-select-p95-ns B]`. Defaults: 5000 tables. Budgets: expand of a 5,000-table schema p95 16 ms (`expand_p95_ns`), selection change p95 1 ms (`select_p95_ns`). `--compare` checks `expand_p95_ns` (25 % + 50 ns). Exit 0 pass, 1 regression or budget, 2 usage error. Wired into `bench.yml`.
- Export bench: `cargo bench -p sqler-export --bench export_throughput -- [--rows N --batch N --format ID]`. Defaults: 1,000,000 rows of five columns, batches of 1,000. Prints `<id>_rows_per_sec=` and `<id>_mib_per_sec=` for every format the enabled features compile (`csv`, `json`, `sql`, `markdown`, `parquet`, `xlsx`); `--format ID` runs one; a `--bench` argument is ignored. Exit 0 ok, 1 writer error, 2 usage error. Not in `bench.yml`; put the numbers in the PR body.
- Grid export bench: `cargo bench -p sqler-ui --bench grid_export -- [--rows N --format ID --source loaded|fresh]` (extra `--bench` ignored). Defaults: 1,000,000 rows of six columns, `csv`, `loaded`. `loaded` copies rows out of a bounded `GridModel` with `LoadedCursor` into the writer, `fresh` streams a `SQLite` recursive CTE through `fresh_rows`. Prints `<source>_<id>_rows_per_sec=`. Not in `bench.yml`; figures in `docs/src/benchmarks.md`.

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
| [0018](docs/src/adr/0018-rsa-alternatives.md) | RSA without the `rsa` crate: a `ring` SSH signer and a vendored `sqlx-mysql` patch on `aws-lc-rs` | accepted |
| [0019](docs/src/adr/0019-tls-server-name-through-tunnel.md) | TLS server name through a tunnel: `TlsConfig::server_name`, local Unix-socket bridge | accepted |
| [0020](docs/src/adr/0020-sql-text-services.md) | SQL text services: own lexer, fail-safe classification | accepted |
| [0021](docs/src/adr/0021-query-history-and-saved-queries.md) | Query history and saved queries: JSON Lines history, versioned TOML saved queries | accepted |
| [0022](docs/src/adr/0022-catalog-snapshot-name-index.md) | Catalog snapshot, name index and on-demand column loading | accepted |
| [0023](docs/src/adr/0023-completion-engine.md) | Completion engine: own lexer, schema source trait, ranking | accepted |
| [0024](docs/src/adr/0024-query-execution.md) | Query execution: headless pipeline in `sqler-ui`, bound parameters, per-statement results, prod gate | accepted |
| [0025](docs/src/adr/0025-sql-editor-component.md) | SQL editor component: `gpui-component` editor with headless logic | accepted |
| [0026](docs/src/adr/0026-editor-intellisense.md) | Editor intellisense: completion, hover and diagnostics in the SQL editor | accepted |
| [0027](docs/src/adr/0027-bounded-result-model.md) | Bounded result model: credit gate, fetch more and compact rows | accepted |
| [0028](docs/src/adr/0028-export-writer-api.md) | Export writer API: push-style row writers, a format registry and SQL literals | accepted |
| [0029](docs/src/adr/0029-parquet-and-xlsx-writers.md) | Parquet and XLSX writers: own streaming zip, low-level Parquet API, two cargo features | accepted |
| [0030](docs/src/adr/0030-staged-edits-and-dml.md) | Staged edits and DML: bound statements, single-table editability, one-transaction apply | accepted |
| [0031](docs/src/adr/0031-client-side-sort-and-filter.md) | Client-side sort and filter: published display order, resumable job on the UI thread | accepted |
| [0032](docs/src/adr/0032-grid-export.md) | Grid export: loaded rows or a fresh run of a read statement, writer on the blocking pool, hidden partial file, Parquet in the app default | accepted |

## 9. Roadmap status

Current: **M3 done (code merged; maintainer-only checks run by the owner in parallel, see the list); M4 next** (results and editing). See `docs/reports/iteration-004.md`. The owner waived the wait for the maintainer-only display and hardware checks and closed M3 with them listed (intake decision 3 of the iteration-004 report); every code-verifiable Definition of Done item was met.

M3 delivered: SSH RSA keys and the `MySQL` RSA password exchange without the `rsa` crate (ADR-0018), `verify-full` TLS through an SSH tunnel on Unix (ADR-0019), SQL text services (ADR-0020), query history and saved queries (ADR-0021), the catalog snapshot and name index (ADR-0022), the completion engine with hover and diagnostics (ADR-0023), query execution with the prod write gate (ADR-0024), the SQL editor tab (ADR-0025), editor intellisense (ADR-0026) and the history tab. Measured fresh at `e98f4c8` on macOS arm64 (Apple M4 Pro): cold start 113.7 ms, idle RSS 72.0 MiB with three connections configured and not opened (lower than M2 for no known reason; repeat it), binary 42.3 MiB (44,319,056 bytes), 1595 tests run and passed (2 skipped), coverage not measured (`cargo llvm-cov` not installed). Completion p95 on 5,000 tables: engine 405 us (worst of all scenarios 2.49 ms, `diagnose` on a 2,000-statement script), editor end to end 282 us, against the 16 ms budget: met. Headless editor work per keystroke 1.63 ms at worst; input-to-paint latency is not measured.

M2 delivered the keychain, the connection store, the object model, the tunnel, DDL, the catalog, the `PostgreSQL` and `MySQL`/`MariaDB` drivers, the connection manager and the object browser (ADR-0008 to ADR-0017); report in `docs/reports/iteration-003.md`.

Pending maintainer-only checks (need a display, an OS desktop session or a real server; none can be done by an agent; the owner runs them in parallel with M4):

- M1 (iteration-002 report): interactive checks of the workspace shell (PR #11), settings and themes (#12), palette and keybindings (#13) and grid scrolling (#14).
- M2: idle RSS with three sessions actually opened (the app has no headless way to open sessions); keychain (PR #16: `cargo test -p sqler-config -- --ignored keychain_round_trip` on macOS, Windows and Linux desktop sessions, and a look at the OS keychain UI; still open); SSH bastion (PR #19: `SQLER_TEST_BASTION_*` and `cargo test -p sqler-tunnel --test real_bastion -- --ignored`; the Windows named-pipe agent code is compiled by CI only; still open); connection manager (PR #24: masked inputs, focus, Escape, drag preview, keychain prompts on three OSes, SSH host-key prompt with a real bastion); object browser (PR #25: context menus, DROP dialog and the `prod` second step, DDL and Properties tabs, scrolling 5,000 tables, keyboard navigation, clipboard).
- RSA and TLS (M3, PR #31): an RSA user key against a real bastion with `rsa-sha2-256`/`-512`, an agent-held RSA key, the readable "use Ed25519/ECDSA" error against a SHA-1-only `ssh-rsa` server, a plaintext `MySQL` RSA exchange against a real server with `ssl-mode=disabled` and that `prefer` with no TLS offered is refused with the readable error, the `aws-lc-sys` build on Windows and macOS CI, the first CI run of the `db` job with `ssh` in `SQLER_TEST_REQUIRE_DB`; `verify-full` through a real bastion on macOS and Linux (PR #28). CI-only first runs: the `catalog_index` (PR #29) and completion (PR #30) bench steps in `bench.yml`.
- SQL editor on a display (PR #36): highlighting in light and dark themes, tab header, Alt-click, add cursor above and below, add next occurrence, bracket frame, `editor:format-sql` as one undo step, `editor:new` by key and palette, "Open in SQL Editor" from the browser menu and the DDL tab, `dev:open-large-sql-script` responsiveness, typing and scrolling speed on the 10,000-line script, run selection, statement and all from the editor with the results display.
- Query execution (PR #34): each run mode by key and palette, per-statement result tabs, "Cancelled", cancel of `pg_sleep` and `SLEEP`, the parameter dialog, the prod confirmation, cancel on tab close and quit, a responsive window during a sleep.
- History and saved queries (PR #37): history rows, live search and the "All connections" toggle, Open and Run again (prod confirmation, truncated and redacted entries), double-click opens an entry, Clear history, the Save query dialog, the SAVED QUERIES section, `query:open-saved`, rename and delete, persistence after a restart, a large history file, `<redacted>` for a statement with a password, focus after dialogs.
- Intellisense (PR #38): popup for tables and for `u.` columns that arrive late, Ctrl+Space (also the macOS input-source conflict and the palette command), hover, the diagnostics underline after 300 ms, pick-up of a catalog refresh, no popup without a catalog, views in the popup, smooth typing on the 10,000-line script.
- Editor input-to-paint latency has no probe; the figure in `docs/src/benchmarks.md` is headless work only.

Known limits and gaps: `MySQL` cancel race window and the "Dropping..." step that blocks the window (ADR-0010, ADR-0017); diagnostics stay silent for non-DML statements, unloaded relations, relations created in the text, uncertain levels and routine bodies (`docs/src/architecture/completion.md`, "Limits"); the classifier can miss a write behind a function or view, and `standard_conforming_strings=off`, `ANSI_QUOTES` and `NO_BACKSLASH_ESCAPES` give `Unknown`, which always confirms on `prod` (ADR-0020); `MySQL` DDL commits implicitly and there is no read-only transaction (ADR-0024); a failed intellisense load is retried only after a reconnect (PR #38); bench noise on shared runners; the 1M-row grid holds about 235 MB until the bounded result model of M4; Linux and Windows desktop numbers are unmeasured; the vendored `sqlx-mysql` (`=0.9.0`) needs maintenance, and `aws-lc-sys` uses `prebuilt-nasm` on Windows. Carried to later milestones: read-only connection mode, tab restore, connection pooling, saved-query folder sync, notebooks, `verify-full` through a tunnel on Windows, snippets without tab stops, a headless session-open path for RSS, GUI runners for Linux and Windows.

M0 and M1 are described in `docs/reports/iteration-001.md` and `docs/reports/iteration-002.md`.

M0 covers the workspace skeleton, this handbook, the mdBook skeleton, CI on three OSes, `cargo-deny`,
license headers, ADR-0000 to ADR-0004, and a `sqler-cli` that runs a query on `SQLite`.
Later milestones: M1 UI foundation, M2 connections and drivers, M3 editor and intellisense,
M4 results and editing, M5 AI, M6 plugins, M7 release 1.0. Do not start a milestone before the
previous one's Definition of Done is met. The only exception is an explicit owner waiver recorded
in the iteration report: maintainer-only display and hardware checks may be carried as an explicit
pending list (as for M3, section 9 above); all code-verifiable Definition of Done items must still
be met.

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
- `sqler --exit-after-first-frame` exits with code 0 after the first frame (cold-start measurement). On quit, open sessions are closed (ADR-0016): `app:quit` closes them under a 2 s bound; the window-close hook only gets about 150 ms. `sqler --open-sessions` opens every saved connection through the same pipeline after startup and idles (idle RSS with sessions open); it skips, with a stderr line holding the connection name and a fixed reason (never error text; only the host-key reasons add the jump `host:port`), any connection with a keychain secret, an untrusted SSH host key or a failed connect, so SQLer itself never prompts (an SSH agent may).
- Measuring the desktop app on macOS: the config directory is `$HOME/Library/Application Support/SQLer`, so run `sqler` with `HOME` pointed at a scratch directory to get a known `connections.toml` and leave the real config alone. `hyperfine` is at `/opt/homebrew/bin`, `mdbook` and `cargo-nextest` are in `~/.cargo/bin` and may be off `PATH`.
- Secrets (T5, ADR-0012): headless Linux CI has no Secret Service, so tests use `MemoryStore`; `KeychainStore` is only checked by hand with `cargo test -p sqler-config -- --ignored keychain_round_trip` on a desktop session. `keyring` 4.x has no native-backend features of its own: enable `v1`, which turns on the Keychain, Credential Manager and `zbus` Secret Service (`crypto-rust`, `async-io`) stores per target. Never use keyutils, `vendored` `libdbus`, OpenSSL crypto, a `tokio` runtime feature or a mock store. `SecretStore` calls block; async callers use `spawn_blocking`.
- `PostgreSQL` test server (ADR-0009): the server rejects a TLS key it does not own or that is group/world readable, so the script copies the key inside the container, runs `chown postgres` and `chmod 600`, then starts the server. GitHub `services:` cannot pass server arguments (`-c ssl=on`), hence the `docker run` script. The image's init server listens on the socket only; wait with `pg_isready -h 127.0.0.1`, not the socket.
- `PgConnectOptions` has a `Debug` that prints the password: never derive `Debug` on a struct that holds it and never log it (`PgCanceller` has a hand-written `Debug`). Driver errors must not echo the target.
- `MySQL` test server (ADR-0010, ADR-0018): `sqlx-mysql` is the vendored `vendor/sqlx-mysql` (pinned `=0.9.0`, `aws-lc-rs` RSA backend), so a plaintext login to an account not cached by `caching_sha2_password` works only with an explicit `ssl-mode=disabled` (the patched `encrypt_rsa` refuses otherwise, so a stripped TLS offer under `prefer` cannot downgrade), and even then the in-band server key lets a man-in-the-middle read the password: only `require`, `verify-ca` and `verify-full` protect it. The test `rsa_exchange_over_plaintext_cold_cache` covers both (MySQL only; `MariaDB` has no such plugin; it uses a TLS-stripping proxy). On every `sqlx` bump re-sync the vendored copy (see its README). `aws-lc-sys` on x86_64 Windows uses the `prebuilt-nasm` feature (target-specific dependency in `sqler-driver-mysql`); if Windows CI breaks on it, part (b) reverts to NO-GO (ADR-0018). `deny.toml` bans the `rsa` crate; never add it. Text in a `_bin` collation and `MariaDB` `JSON` arrive as `Bytes`; `sqlx` reports a zero date as NULL, so the driver reads date columns as raw bytes. `KILL QUERY` needs a side connection; a killed `SLEEP()` returns 1, not an error.
- A DB test that skips is green: without `SQLER_TEST_REQUIRE_DB` a missing variable only prints a skip line. CI sets it so the `db` job cannot pass without running. An unreachable server always fails.
- `sqler-tunnel` (ADR-0013): `russh` is built with `ring` only and never with its `rsa` feature. RSA user keys work through `rsa_key.rs` (ADR-0018): OpenSSH, PKCS#1 and PKCS#8 files, 2048/3072/4096 bits, `rsa-sha2-256`/`-512` only; no SHA-1 `ssh-rsa`, no RSA-only host keys, legacy-encrypted PEM refused (errors name Ed25519/ECDSA). RSA keys in an agent need no special code. RSA tests need the `start-sshd.sh` container (gate id `ssh`); the test keys are generated, never committed. Host keys are never accepted silently: use `trust_host_key` after the user confirms the fingerprint. Tests use an in-process `russh` server, no Docker.
- Connect pipeline (ADR-0016): `verify-full` TLS through an SSH tunnel is refused (`TlsHostnameThroughTunnel`; an explicit TLS mode overrides the URL `sslmode`/`ssl-mode`, and a URL verify-full counts only without an explicit mode) because the driver would verify `127.0.0.1`; lifting it needs a server-name field on core `TlsConfig` plus driver support. A `Tunnel` must not be dropped outside a tokio runtime: `TunnelledSession` closes it on the runtime handle. A stored target never has user info; the database user is `?user=` and the password is injected at connect time (a password with a target that has no user is refused). `sqler-ui` talks to the pipeline only through the `Connector` trait.
- Object browser (ADR-0017): `ObjectTree` never calls the catalog; `expand` and `*_loaded` return `CacheKey`s and the workspace turns them into `Effect::LoadCatalog`. Tokio work still goes only through `SessionBridge`. DROP SQL comes only from `DropFlow::confirm` (once, twice on `prod`); nothing else may build a `RunJob(Drop)`. `gpui-component` 0.8.0 `ContextMenuExt` cannot open a menu from code, so `object:show-menu` (Shift+F10, Menu) opens the palette filtered to `Object: `. The sidebar is a `gpui::list` with a `ListState` that is diffed in `settle`; do not rebuild rows per frame. `Up`/`Down` walk groups, connections and tree rows through `Workspace::move_selection`.
- Query execution (ADR-0024): while a query runs, the tab's session is on loan to the `QueryJob` (`take_session`, returned in `query_finished`; closed there if the tab is gone), so a second run is refused. Cancel needs the session's `CancelHandle` and the job's `RunStop` (`stop.cancel()` sets the flag and cancels every `GridSink` through its `GridCancel`, which also wakes a paused statement); `SQLite` resets its own flag when `execute` starts, so without the flags a cancel between statements is lost. Parameters are bound through `Query::params` (`$k`, `?k`, `?` per occurrence), never spliced. `sqler-ui::query` is GPUI-free; the prod confirmation lists every non-`Read` statement once per run.
- Editor intellisense (ADR-0026): `sqler_ui::editor::intellisense` is GPUI-free; only `editor/view/provider.rs` uses `gpui-component`'s `CompletionProvider`. The direct `lsp-types` dependency must stay on the same line (0.97) as the one `gpui-component` uses (`cargo tree -i lsp-types` must show one version), or the provider traits see a different type. Catalog futures are awaited on the GPUI executor, never with `tokio::spawn`. `completion_menu_state` is `#[doc(hidden)]`; re-check it on a `gpui-component` bump. `editor:trigger-completion` is Ctrl+Space, which macOS may take for input source switching.
- Append-only files (`CHANGELOG.md`, `docs/src/SUMMARY.md`, `docs/src/adr/index.md`) conflict on every rebase of a stacked PR. After resolving one, check that no bullet or table row lost its newline (a `".- ` join happened on CHANGELOG line 24 and a blank line split a table in `benchmarks.md`), and that every table still has one cell count.
- A new gated bench follows PR #33, not a bare 25 % + ns rule: take the minimum over `--reps 3` on the base and on the PR and add an absolute slack sized to the metric (100 ns for the grid, 250 us for the browser tree), because shared runners only add time. Linux CI figures are 1.5 to 3 times the macOS ones (PR #35: `diagnose_p95_ns` 5.04 ms against 2.4 ms here).
- The headless `editor_buffer` and `editor_intellisense` benches stop at the GPUI providers (`Rope::to_string`, LSP position mapping, debounce, paint), and are not in `bench.yml`. A typing or scrolling regression shows only on a display, so check it by hand on the 10,000-line script after editor changes.
- `[profile.release]` sets `strip = "symbols"` and `codegen-units = 1` (T14, binary size). A release build has no symbol names, so a panic backtrace shows addresses only, and a profiler shows none. For debugging build with `CARGO_PROFILE_RELEASE_STRIP=none cargo build --release -p sqler-app` (and `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` for line numbers). `cargo bench` inherits `release`, so it is stripped too. LTO and `panic = "abort"` were measured and rejected, see `docs/src/benchmarks.md`.
- Result grid credit gate (ADR-0027): only `GridCancel` (`GridSink::canceller`), `GridModel::cancel` or a dropped `GridModel` wake a paused ingest; setting the `CancelFlag` alone does not. Store `GridCancel`, never `cancel_flag()`, wherever a tab or run can be cancelled (`QueryJob::stop` is a `RunStop` for this reason; `Quit` cancels every grid). A paused statement holds the tab session, so later statements of a script stay `Pending` until fetch-all, cancel or close.
- Grid export (ADR-0032, `sqler_ui::export`): the writer (`write_feed`) always runs on tokio's blocking pool (`SessionBridge::blocking`) and blocks on a bounded feed (`FEED_CAPACITY` = 4 batches); never run it on the GPUI thread and never `tokio::spawn` for it. Loaded rows are copied out of the UI-owned `GridModel` on the UI thread by `LoadedCursor`, in slices with a time budget, so a million rows do not freeze a frame; keep the budget when touching it. A fresh run (`fresh_rows`) opens its own short-lived session and is allowed only for one statement that `classify_each` calls `Read` (`fresh_gate`, then `Workspace::fresh_check`); never re-run a write, a script or an unclassified statement. A paused statement holds the tab session (credit gate above), which is why the export does not borrow it. The target is written as `.<name>.<pid>-<job>.partial` and renamed on success (`PartialFile`); `ExportFailure` texts never carry the SQL, a path or a credential: `Connect` and `Driver` hold the user text of the connect pipeline or the driver, built only through `connect_text`/`driver_text` (add a fixed text there for any new error variant that can echo a target, path or secret). A dialog pins the tab's result generation (`Workspace::result_replaced`, called wherever a tab's table is replaced); if it changed, the export is refused.
