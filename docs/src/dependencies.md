# Dependencies

Checked on: 2026-10-20

Latest stable versions of the candidate components, read from crates.io (`max_stable_version`)
and not from memory. A listed version is a survey result, not a decision: the ADR or the PR that
introduces a dependency records the final version and the reason. Rust stable toolchain at the
time of the survey: 1.99.1 (2026-10-13), from `static.rust-lang.org`.

Source commands (all rows):

- API: `curl -s -A "sqler-bootstrap (github.com/sqler)" https://crates.io/api/v1/crates/<name> | jq -r '.crate.max_stable_version, .crate.updated_at'`
- Cross-check: `cargo info <name>` and `cargo search <name>`
- Toolchain: `curl -s https://static.rust-lang.org/dist/channel-rust-stable.toml | grep -m1 -A1 '^\[pkg.rust\]'`

| Component | crates.io crate | Latest stable | Released/updated | Notes |
| --------- | --------------- | ------------- | ---------------- | ----- |
| GPUI distribution | `gpui` | 0.2.2 | 2025-10-22 | Published by Zed's `mikayla-maki`; repository `zed-industries/zed`. Community fork `gpui-ce` 0.2.2 (2026-08-28, `gpui-ce/gpui-ce`) also exists. ADR-0001 decides. Source: API for `gpui`, `gpui-ce`; `cargo search gpui` |
| gpui-component | `gpui-component` | 0.8.0 | 2026-10-19 | Repository `longbridge/gpui-kit`. Source: API |
| iced | `iced` | 0.14.0 | 2025-12-07 | UI alternative for the ADR-0001 spike. Source: API |
| sqlx | `sqlx` | 0.9.0 | 2026-05-21 | PostgreSQL, MySQL, `SQLite`. Used by `sqler-driver-sqlite` with `default-features = false` and `sqlite-bundled`, `runtime-tokio`. Checked 2026-10-20. Source: API; `cargo info sqlx` |
| tiberius | `tiberius` | 0.13.0 | 2026-09-25 | TDS (MSSQL) driver. Source: API |
| duckdb | `duckdb` | 1.10506.1 | 2026-10-16 | `duckdb-rs`; version number follows the bundled DuckDB release. Source: API |
| sqlparser | `sqlparser` | 0.63.0 | 2026-09-13 | Apache DataFusion `sqlparser-rs`. Source: API |
| tree-sitter | `tree-sitter` | 0.27.1 | 2026-10-08 | Rust bindings. Source: API |
| rmcp | `rmcp` | 3.6.0 | 2026-10-20 | Official MCP Rust SDK. Source: API |
| agent-client-protocol | `agent-client-protocol` | 3.4.0 | 2026-10-16 | Repository `agentclientprotocol/rust-sdk`. Source: API |
| wasmtime | `wasmtime` | 49.0.4 | 2026-10-19 | `max_version` is the pre-release 50.0.0-rc.1, not used. Source: API (`max_stable_version`) |
| cargo-component | `cargo-component` | 0.21.1 | 2025-03-18 | No release for over a year; check the project status before relying on it. Source: API |
| keyring | `keyring` | 4.2.0 | 2026-08-29 | Source: API |
| cargo-dist | `cargo-dist` | 0.32.0 | 2026-05-22 | The tool was renamed `dist`, but the `dist` crate on crates.io is an unrelated 2016 crate (0.0.0), so the crate is still `cargo-dist`. GitHub `axodotdev/cargo-dist` latest release is v0.33.0 (2026-09-10), newer than crates.io; verify the install channel in M7. Source: API for `cargo-dist` and `dist`; `cargo info cargo-dist`; GitHub releases API |
| thiserror | `thiserror` | 2.0.21 | 2026-09-23 | Derive for typed library errors; `rust-version` 1.77. Used by `sqler-core` and `sqler-driver-api`. Checked 2026-10-20. Source: API; `cargo info thiserror` |
| tokio | `tokio` | 1.54.0 | 2026-10-19 | Async runtime for the CLI (`rt`) and driver tests (`rt`); sqlx `runtime-tokio` pulls it in. Checked 2026-10-20. Source: API |
| futures-core | `futures-core` | 0.3.34 | 2026-08-11 | `Stream` trait for the `SQLite` query stream; same version as sqlx uses. Checked 2026-10-20. Source: API |
| anyhow | `anyhow` | 1.0.104 | 2026-07-18 | Error context in the `sqler-cli` binary only. Checked 2026-10-20. Source: API |

## CI tools and actions

Checked on: 2026-10-20

Actions are referenced by their latest major tag; Cargo tools are pinned to an exact version.

Source commands:

- Actions: `gh api repos/<owner>/<repo>/releases/latest --jq '"\(.tag_name) \(.published_at)"'`
- Tools: `curl -s -A "sqler-ci (github.com/sqler)" https://crates.io/api/v1/crates/<name> | jq -r '.crate.max_stable_version, .crate.updated_at'`

| Component | Source | Pinned | Released | Notes |
| --------- | ------ | ------ | -------- | ----- |
| `actions/checkout` | GitHub release | `v7` (v7.0.1) | 2026-07-20 | Repository checkout |
| `Swatinem/rust-cache` | GitHub release | `v2` (v2.9.2) | 2026-08-06 | Cargo cache |
| `taiki-e/install-action` | GitHub release | `v2` (v2.87.29) | 2026-10-16 | Prebuilt tool binaries |
| `actions/configure-pages` | GitHub release | `v6` (v6.0.0) | 2026-03-25 | Pages setup |
| `actions/upload-pages-artifact` | GitHub release | `v5` (v5.0.0) | 2026-04-10 | Book artifact |
| `actions/deploy-pages` | GitHub release | `v5` (v5.0.1) | 2026-09-01 | Pages deploy |
| `cargo-nextest` | crates.io | 0.9.148 | 2026-10-08 | Test runner |
| `cargo-deny` | crates.io | 0.20.2 | 2026-07-09 | License, ban, source and advisory checks |
| `mdbook` | crates.io | 0.5.4 | 2026-07-06 | Book builder |

## Version policy

- Verify the latest stable version on crates.io (and read the release notes) before adding or bumping
  any dependency. Never pin a version from memory.
- Stable releases only; no pre-release versions.
- Declare each dependency once in the root `[workspace.dependencies]`; crates use `dep.workspace = true`.
- Commit `Cargo.lock`.
- Every add or bump updates this page or an ADR with the version and the reason.
- Send a User-Agent header with crates.io API requests; the API rejects requests without one.

## Rationale

Per-dependency rationale is added by the PR that introduces it.

- `thiserror` 2.0.21: typed library errors without hand-written `Display`/`Error` impls (AGENTS.md
  §6). Used by `sqler-core` (`CoreError`) and `sqler-driver-api` (`DriverError`). Its `rust-version`
  (1.77) is below the workspace's 1.99.
- `sqlx` 0.9.0: SQLite backend of `sqler-driver-sqlite` (AGENTS.md §6). Features are limited to
  `sqlite-bundled` (compiles SQLite from source, so Windows and macOS CI need no system library) and
  `runtime-tokio`; no `macros`, `migrate`, `json`, `any` or TLS. The plain `sqlite` feature was not
  used because it also enables `deserialize`, `load-extension` and `unlock-notify`. The first build is
  slower because it compiles C.
- `tokio` 1.54.0: required by sqlx `runtime-tokio`. Declared with `default-features = false`; the
  CLI and the driver tests add only `rt`. The CLI runs a current-thread runtime.
- `futures-core` 0.3.34: only the `Stream` trait, to drive sqlx row streams without the full
  `futures` crate. One version in the tree.
- `anyhow` 1.0.104: error chains with context in the binary only (AGENTS.md §6); libraries keep
  `thiserror`.
- `cargo-nextest`: faster, per-test-process runner used by CI and the local commands.
- `cargo-deny`: enforces the Apache-2.0-compatible license allow-list in `deny.toml`, plus bans, sources and advisories.
- `mdbook`: builds the documentation book in CI and for GitHub Pages.
- `taiki-e/install-action`: installs the pinned tools as prebuilt binaries instead of compiling them in every run.
