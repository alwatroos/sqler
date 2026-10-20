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
| sqlx | `sqlx` | 0.9.0 | 2026-05-21 | PostgreSQL, MySQL, `SQLite`. Source: API |
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
