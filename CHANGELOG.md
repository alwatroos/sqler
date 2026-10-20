# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- DDL and script generation for `PostgreSQL`, `MySQL`/`MariaDB` and `SQLite` (`sqler-sql`). ADR-0015 and the user guide page "DDL and scripts".
- `sqler-config` secrets API: `SecretStore` with `KeychainStore` (macOS Keychain, Windows Credential Manager, Linux Secret Service through `keyring` 4.2.0) and `MemoryStore`, `SecretRef`, `SecretKind`, a redacting `Secret` and a typed `SecretError`. ADR-0012 and the user guide page "Secrets".
- Cargo workspace skeleton: 24 `crates/sqler-*` stubs plus `xtask`, with shared lints and metadata.
- `AGENTS.md` v1 (project handbook) and `CLAUDE.md` pointer.
- Dependency version survey and version policy in `docs/src/dependencies.md`.
- mdBook documentation skeleton (`docs/`) with user guide, architecture, ADR, plugin development, AI, benchmarks and contributing sections.
- ADR-0000 (ADR process), ADR-0001 (UI framework spike plan), ADR-0002 (plugin ABI: WASM Component Model), ADR-0003 (AI tool contract).
- `sqler-core` domain types: `DriverId`, `ConnectionConfig`, `Query`, `Value`, `DataType`, `Column`, `Row`, `ResultSet`, minimal catalog and `CoreError`.
- `sqler-driver-api`: `Driver`, `Session`, `QueryStream` and introspection API with cancellation, plus `DriverRegistry`.
- ADR-0004 (driver API) and the core and driver API architecture page.
- `thiserror` dependency for typed library errors.
- GitHub Actions CI on Linux, macOS and Windows: `cargo fmt`, `cargo clippy -D warnings` and `cargo nextest`.
- `cargo-deny` license policy (`deny.toml`, Apache-2.0-compatible allow-list), checked in CI.
- CI docs build (`cargo doc` with `-D warnings`, `mdbook build docs`) and an SPDX header check for Rust files.
- GitHub Pages workflow that publishes the mdBook from `main`.
- `sqler-driver-sqlite`: `SQLite` driver on sqlx (`Driver`, `Session`, streaming `QueryStream`, cooperative cancellation).
- `sqler-cli run`: runs one SQL statement through a registered driver and prints TSV; the `driver-sqlite` feature (default) registers the `SQLite` driver.
- User guide page for the command-line interface.
- `sqlx` 0.9.0, `tokio` 1.54.0, `futures-core` 0.3.34 and `anyhow` 1.0.104 dependencies, with rationale in `docs/src/dependencies.md`.
- Iteration 001 report (`docs/reports/iteration-001.md`) and the first benchmark numbers for `sqler-cli`.
- `sqler-ui` command registry: validated `category:action` command IDs, `CommandRegistry` with lookup, and a fuzzy palette query ranking commands by title and ID.
- Results grid data model (`sqler_ui::grid`): channel-fed, chunked, non-blocking `GridModel` with viewport reads, cancellable ingestion from a `QueryStream` or a synthetic source.
- 1M-row viewport benchmark (`cargo bench -p sqler-ui --bench grid_viewport`), a std-only harness with p50/p95, memory and baseline comparison.
- `.github/workflows/bench.yml`: Linux benchmark on pull requests that fails on a p95 regression against the base branch or above the 1 ms budget.
- ADR-0006 (grid data model and bench harness).
- `sqler-config`: settings (theme, UI font size), rebindable keybindings with per-platform defaults and conflict detection, and a read-only saved connection list, all from TOML files in the platform config directory.
- `sqler-core`: optional `environment` (dev, staging, prod) and `color` fields on `ConnectionConfig`.
- ADR-0005 (config files and keybindings) and the Settings user guide page.
- `serde` 1.0.229 and `toml` 1.1.8 dependencies; `directories` evaluated and rejected (MPL-2.0 transitive license), see `docs/src/dependencies.md`.
- `sqler-app`: the desktop binary `sqler` (layer L7). It opens a GPUI window with a connection sidebar and a tab strip; each tab is bound to one connection and several tabs per connection are allowed. The tab header shows name, colour and environment, and `prod` tabs use the danger colour.
- `sqler-ui` workspace state (`sidebar`, `tabs`, `workspace`) with unit tests that need no window, a `SessionBridge` that opens driver sessions on tokio without blocking the UI thread, and the `gui` feature for the GPUI views.
- `sqler --exit-after-first-frame` to measure cold start.
- ADR-0007 (desktop binary and UI-thread/tokio bridge), the Workspace user guide page and the UI architecture page.
- `gpui-component` 0.8.0, `gpui-pre` =0.3.9 and `gpui-pre-platform` =0.3.9 as root workspace dependencies (feature `gui`); `deny.toml` gains three per-crate `MPL-2.0` exceptions and eight advisory ignores for the GPUI tree. CI installs the GPUI build packages on Linux and guards the Metal toolchain on macOS.
- Settings tab (`settings:open`) with theme (light, dark, follow system) and UI font size, saved through `sqler-config`; `theme:toggle-mode` command; user guide section "Settings tab and themes".
- Command palette (`workspace:open-command-palette`, Cmd+Shift+P on macOS, Ctrl+Shift+P elsewhere) and a rebindable keybinding system: `sqler_ui::keymap` and `sqler_ui::palette`, default keymap plus `keybindings.toml` applied at startup, conflicts reported in the log and as a notification, and the Keybindings user guide page.
- Results grid tab: a virtual table over the grid model with typed column headers, a status line and Top/End buttons (`grid:scroll-to-top`, `grid:scroll-to-end`); `dev:open-synthetic-grid` opens a 1,000,000-row synthetic grid loaded on its own thread, and closing the tab cancels the loading. Headless `grid_scroll` example and an opt-in in-app probe (`SQLER_GRID_PROBE=1`); user guide page "Results grid" and benchmark rows.
- Iteration 002 report (`docs/reports/iteration-002.md`) and the first desktop-app numbers (cold start, idle RSS, binary size) in the benchmarks page; roadmap status set to M1 done with maintainer checks pending.
- M2 foundation (T1): object model in `sqler-core` (`ObjectKind`, `DbObject`, `Table`, `ColumnInfo`, keys, `Listing`) replacing `TableKind`/`Schema`/`Catalog`; lazy `Introspect` trait (`supports`, `list`, `table`, `definition`) that reports unsupported kinds as `Listing::Unsupported`; typed `Value` variants `Decimal`, `Date`, `Time`, `Timestamp`, `TimestampTz` and `Json`; `TlsMode`/`TlsConfig` on `ConnectionConfig`; `SQLite` introspection and typed date decoding. `SQLite` text in `DATE`, `TIME`, `DATETIME` and `TIMESTAMP` columns now prints in normalised ISO form, for example `2024-01-02T03:04:05` as `2024-01-02 03:04:05`. ADR-0008 and the Databases user guide pages.
