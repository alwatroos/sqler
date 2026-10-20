# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `sqler-completion`: SQL completion engine for `PostgreSQL`, `MySQL`/`MariaDB` and `SQLite`. `complete(&CompletionRequest, &dyn SchemaSource)` is pure and synchronous, works on incomplete SQL (own total lexer, no `sqlparser`), detects the context (relation, select list, `WHERE`, `ON`, `GROUP BY`, `ORDER BY`, `schema.`, `alias.`, `INSERT` columns), resolves aliases, CTEs and subquery aliases with dialect-specific quoting and case rules, offers per-dialect keywords, built-in functions and five snippets, and ranks in-scope columns and prefix matches first. The schema is read through the `SchemaSource` trait over `sqler-core` types (`MemorySchema` for tests); columns that are not loaded produce a `SchemaRequest`, not an error. Proptest and snapshot tests, a 5,000-table `completion` benchmark (p95 budget 16 ms, also run by `bench.yml`), ADR-0023 (including the hover and diagnostics shape) and the architecture page "Completion engine".
- `verify-full` TLS through an SSH tunnel on macOS and Linux: `sqler-core` `TlsConfig` gains `server_name`, the connect pipeline sets it to the connection's original host, and the `PostgreSQL` and `MySQL`/`MariaDB` drivers verify the certificate against it through a private local socket bridge. Windows still refuses it. ADR-0019.
- Object browser in the desktop app: a lazy tree under each open connection (schemas, object folders, columns) with a loading state, a virtualized sidebar list, a context menu per node kind (open data, generate CREATE, SELECT, INSERT and DROP into read-only DDL tabs, copy name, refresh with progress and cancel, properties), DROP only after a dialog that shows the exact SQL and a second confirmation on `prod` connections, 19 `object:*` commands and arrow keys and Enter in the focused sidebar. Shift+F10 and Menu open the command palette filtered to `Object:` (no programmatic context menu in `gpui-component` 0.8.0). New public module `sqler_ui::browser`, ADR-0017, the user guide page "Object browser" and a `browser_tree` benchmark (also run by `bench.yml`).
- DDL and script generation for `PostgreSQL`, `MySQL`/`MariaDB` and `SQLite` (`sqler-sql`). ADR-0015 and the user guide page "DDL and scripts".
- Connection manager in the desktop app: a form for new and edited connections (driver, target, colour, environment, group, TLS, SSH tunnel), passwords stored in the keychain, a cancellable "Test connection", nested groups with create, rename, delete and drag or command based move, a confirmed delete that also removes the keychain entries, and `prod` connections that stand out. One connect pipeline (secret, SSH tunnel, TLS, driver) in `sqler-app` behind the `Connector` trait of `sqler-ui`; a host-key trust dialog; open sessions and tunnels are closed on quit. `verify-full` TLS through an SSH tunnel is refused for now. ADR-0016 and the user guide section "In the app".
- `sqler-driver-postgres`: `PostgreSQL` driver on sqlx (streaming, server-side cancel, TLS modes with CA and client certificate through rustls, typed values, introspection of all 13 object kinds); the `driver-postgres` feature (default) in `sqler-cli` and `sqler-app`.
- `sqler-driver-mysql`: `MySQL` and `MariaDB` driver on sqlx (streaming, cancel with `KILL QUERY` on a side connection, TLS modes through rustls, typed values, introspection of schemas, tables, views, functions, procedures, triggers, indexes and roles); the `driver-mysql` feature (default) in `sqler-cli` and `sqler-app`. `sqlx` is built without `mysql-rsa`, so the `rsa` crate (RUSTSEC-2023-0071) is not in the tree and the RSA password exchange needs TLS.
- CI `db` job: `MySQL` 8.4 and `MariaDB` 11.8 in Docker with TLS (`start-mysql.sh`), their DB tests and a CLI smoke test.
- ADR-0010 (`MySQL` and `MariaDB` driver) and the user guide page "MySQL and MariaDB".
- CI `db` job: `PostgreSQL` in Docker with TLS, DB integration tests (`SQLER_TEST_<DB>_URL`, `SQLER_TEST_REQUIRE_DB`) and a CLI smoke test.
- ADR-0009 (`PostgreSQL` driver, TLS backend, DB test convention) and the user guide page "PostgreSQL".
- `sqler-catalog`: per-connection schema cache with lazy per-node loading through `Introspect`, one worker that serialises introspection calls, deduplicated concurrent loads, per-node errors, and background refresh of a node, subtree or the whole connection with progress events, cooperative cancel and atomic apply. ADR-0014, the architecture page "Schema catalog", a 5,000-table benchmark (`catalog_tree`, also run by `bench.yml`) and its results in `docs/src/benchmarks.md`.
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
- Iteration 003 report (`docs/reports/iteration-003.md`) and the M2 desktop-app numbers (cold start, idle RSS, binary size, test count) in the benchmarks page; roadmap status set to M2 code complete with maintainer checks and owner decisions pending.
- `sqler-tunnel` (layer L1): SSH session and loopback-only local port forward on `russh` 0.64.1 (pure Rust, `ring` backend). Password, private key file and SSH agent authentication, strict `known_hosts` host key checking with typed errors and an explicit `trust_host_key`, cancellable open. RSA private keys are unsupported until the owner decides on RUSTSEC-2023-0071.
- ADR-0013 (SSH tunnel crate) and the SSH tunnels user guide page.
- `russh` 0.64.1 dependency (`rand` 0.10.3 and `tokio-stream` 0.1.19 as test dependencies), with rationale in `docs/src/dependencies.md`.
- `sqler-config`: editable connection store (`ConnectionStore`) with nested groups, colour, environment, TLS and SSH tunnel profiles, saved atomically as versioned TOML (`version = 1`); M1 files still load and are migrated on the next save. Passwords and passphrases are never written, only keychain flags; the keychain reference is derived from the connection ID and converts to a `SecretRef` (`<id>:<kind>`). `write_atomic` now flushes the temporary file to disk before the rename and, on Unix, the directory afterwards (best effort; Windows skips the directory flush). ADR-0011 (amends ADR-0005) and the Connections user guide page.
- M2 foundation (T1): object model in `sqler-core` (`ObjectKind`, `DbObject`, `Table`, `ColumnInfo`, keys, `Listing`) replacing `TableKind`/`Schema`/`Catalog`; lazy `Introspect` trait (`supports`, `list`, `table`, `definition`) that reports unsupported kinds as `Listing::Unsupported`; typed `Value` variants `Decimal`, `Date`, `Time`, `Timestamp`, `TimestampTz` and `Json`; `TlsMode`/`TlsConfig` on `ConnectionConfig`; `SQLite` introspection and typed date decoding. `SQLite` text in `DATE`, `TIME`, `DATETIME` and `TIMESTAMP` columns now prints in normalised ISO form, for example `2024-01-02T03:04:05` as `2024-01-02 03:04:05`. ADR-0008 and the Databases user guide pages.
