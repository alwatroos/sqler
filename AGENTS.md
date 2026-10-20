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
| `sqler-sql` | L1 | Parsing (`sqlparser`), formatting, statement splitting, dialect handling |
| `sqler-plugin-api` | L1 | WIT definitions and host bindings (wasmtime component model) |
| `sqler-config` | L1 | Settings, keybindings, connection store, keychain |
| `sqler-driver-postgres` | L2 | `PostgreSQL` driver (sqlx / tokio-postgres) |
| `sqler-driver-mysql` | L2 | `MySQL` driver |
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
| `xtask` | tooling | Repository automation (generate WIT bindings, release, bench) |

Layering: a crate may depend only on crates in lower layers.

```
L0 core
L1 driver-api, ai-api, sql, plugin-api, config
L2 driver-*, ai-ollama/anthropic/openai/mistral, catalog, completion, export
L3 ai-tools, plugin-host
L4 mcp
L5 acp
L6 ui, cli
```

Rules:

- No workspace crate depends on `sqler-ui`; only a future desktop binary may. Check:
  `cargo tree --workspace -i sqler-ui` must show `sqler-ui` alone.
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
cargo run -p sqler-cli
cargo run -p xtask
```

- CI (`.github/workflows/ci.yml`) runs this subset of the commands above: fmt, clippy, nextest on Linux/macOS/Windows; deny, docs (`cargo doc` and `mdbook build docs`), spdx on Linux.
- SPDX check (`sh .github/scripts/check-spdx.sh`) prints the offending paths and exits 1 when a tracked `*.rs` file lacks the header on line 1; it prints nothing otherwise.
- CI tools (pinned, see `docs/src/dependencies.md`): `cargo install --locked cargo-nextest@0.9.148 cargo-deny@0.20.2 mdbook@0.5.4`.
- `.github/workflows/pages.yml` publishes the book (`target/book/`) to GitHub Pages on push to `main`.
- Bench: none yet.

## 4. Coding standards

- `#![forbid(unsafe_code)]` in every crate except where FFI is unavoidable (drivers, wasmtime glue); document each exception.
- Workspace-level lints: `clippy::pedantic` as warn, selected lints as deny; `missing_docs` warn on public crates.
- No `unwrap`/`expect` outside tests and `main`; use typed errors.
- Async: never hold a lock across an `.await`; never block the UI executor; every spawned task has a cancellation path.
- UI: every user action is a `Command` with an ID; no business logic in views; views render state, actions mutate state.
- Tests: unit tests next to code; integration tests under `tests/`; DB tests use testcontainers and are tagged so they can be skipped locally.
- Performance: any PR touching the grid, catalog or completion must include a benchmark result in its description.
- Naming: crates `sqler-*`, features `driver-*`, `ai-*`; commands `category:verb-object` (`query:run-selection`).

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
| [0001](docs/src/adr/0001-ui-framework-spike-plan.md) | UI framework spike plan | proposed |
| [0002](docs/src/adr/0002-plugin-abi-wasm-component-model.md) | Plugin ABI: WASM Component Model | accepted |
| [0003](docs/src/adr/0003-ai-tool-contract.md) | AI tool contract | accepted |

## 9. Roadmap status

Current: **M0 active** (bootstrap). Next: M1 (UI foundation).

M0 covers the workspace skeleton, this handbook, the mdBook skeleton, CI on three OSes, `cargo-deny`,
license headers, ADR-0000 to ADR-0003, and a `sqler-cli` that runs a query on `SQLite`.
Later milestones: M1 UI foundation, M2 connections and drivers, M3 editor and intellisense,
M4 results and editing, M5 AI, M6 plugins, M7 release 1.0. Do not start a milestone before the
previous one's Definition of Done is met.

M0 CI is in place (`ci.yml`, `pages.yml`, `deny.toml`) except the plugin-build and benchmark-regression jobs, which are deferred (see Gotchas).

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
- The plugin-build and benchmark-regression CI jobs are deferred until `plugins/` (M6) and benchmarks exist.
- CI installs the toolchain with a bare `rustup toolchain install`, which reads `rust-toolchain.toml` (rustup 1.28 or newer), so the Rust version is pinned in one place only.
