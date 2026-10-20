# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
