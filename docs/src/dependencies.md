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
| GPUI distribution | `gpui` | 0.2.2 | 2025-10-22 | Not chosen (ADR-0001). Published by Zed's `mikayla-maki`; repository `zed-industries/zed`. Cannot be unified with `gpui-component` 0.8.0. Source: API; `cargo search gpui` |
| GPUI community fork | `gpui-ce` | 0.2.2 | 2026-08-28 | Not chosen (ADR-0001). Repository `gpui-ce/gpui-ce`. Source: API |
| GPUI (chosen) | `gpui-pre` | =0.3.9 | 2026-10-12 | Chosen (ADR-0001). Snapshot of Zed's GPUI (`zed@93d7456`); required as `=0.3.9` by `gpui-component` 0.8.0. Verified 2026-10-20. Root workspace dependency (optional, through the `sqler-ui` feature `gui`, T5). Source: API; `gpui-component` 0.8.0 dependency list |
| GPUI platform (chosen) | `gpui-pre-platform` | =0.3.9 | 2026-10-12 | Chosen (ADR-0001). Zed's `gpui_platform`; creates the application on each OS, not re-exported by `gpui-component`. Root workspace dependency (optional, through `sqler-ui/gui`, T5). Verified 2026-10-20. Source: API |
| gpui-component | `gpui-component` | 0.8.0 | 2026-10-19 | Chosen (ADR-0001). Repository `longbridge/gpui-kit`, Apache-2.0. Feature `tree-sitter-sql` for SQL highlighting. Root workspace dependency (optional, through `sqler-ui/gui`, T5). Verified 2026-10-20, no newer stable version than 0.8.0. Source: API; `cargo info gpui-component@0.8.0` |
| iced | `iced` | 0.14.0 | 2025-12-07 | Not chosen: GPUI passed the spike, iced was not built (ADR-0001). Source: API |
| sqlx | `sqlx` | 0.9.0 | 2026-05-21 | PostgreSQL, MySQL, `SQLite`. Used by `sqler-driver-sqlite` with `default-features = false` and `sqlite-bundled`, `runtime-tokio`, and by `sqler-driver-postgres` with the added crate-level features `postgres` and `tls-rustls-ring-native-roots` (ADR-0009). `max_stable_version` is still 0.9.0, so the workspace line is unchanged. Checked 2026-10-20. Source: API; `cargo info sqlx` |
| rustls | `rustls` | 0.23.46 | 2026-10-14 | TLS for `sqlx` through `tls-rustls-ring-native-roots`; transitive, version from `Cargo.lock`. No OpenSSL, no `aws-lc`. Checked 2026-10-20. Source: API |
| ring | `ring` | 0.17.14 | 2025-03-11 | Crypto provider of `rustls` in that feature (needs no cmake or NASM). Transitive. Checked 2026-10-20. Source: API |
| rustls-native-certs | `rustls-native-certs` | 0.8.5 | 2026-10-13 | Loads the OS trust roots; no bundled root list, so no `webpki-roots` (CDLA) licence. Transitive. Checked 2026-10-20. Source: API |
| postgres (Docker image) | `postgres:18-alpine` | 18 | 2026-10-20 | Server for the DB tests (`tests/docker/start-postgres.sh`); newest major with an `-alpine` tag, `19*` are only `rc`/`beta`. Source: `curl -s 'https://hub.docker.com/v2/repositories/library/postgres/tags?page_size=100&name=-alpine' \| jq -r '.results[].name'` |
| tiberius | `tiberius` | 0.13.0 | 2026-09-25 | TDS (MSSQL) driver. Source: API |
| duckdb | `duckdb` | 1.10506.1 | 2026-10-16 | `duckdb-rs`; version number follows the bundled DuckDB release. Source: API |
| sqlparser | `sqlparser` | 0.63.0 | 2026-09-13 | Apache DataFusion `sqlparser-rs`. Source: API |
| tree-sitter | `tree-sitter` | 0.27.1 | 2026-10-08 | Rust bindings. `gpui-component` 0.8.0 requires `^0.26.13`, so the root `Cargo.lock` resolves 0.26.13 (through `sqler-ui/gui`). Source: API; `Cargo.lock` |
| tree-sitter-sequel | `tree-sitter-sequel` | 0.3.11 | 2025-10-01 | SQL grammar pulled in by the `gpui-component` feature `tree-sitter-sql`; now in the root `Cargo.lock` (0.3.11) through `sqler-ui/gui`. Verified 2026-10-20. Source: API; `Cargo.lock` |
| memory-stats | `memory-stats` | 1.2.0 | 2024-06-26 | Reads process RSS for the `SPIKE rss_mb` value. Spike only, not in the root workspace. Verified 2026-10-20. Source: API |
| rmcp | `rmcp` | 3.6.0 | 2026-10-20 | Official MCP Rust SDK. Source: API |
| agent-client-protocol | `agent-client-protocol` | 3.4.0 | 2026-10-16 | Repository `agentclientprotocol/rust-sdk`. Source: API |
| wasmtime | `wasmtime` | 49.0.4 | 2026-10-19 | `max_version` is the pre-release 50.0.0-rc.1, not used. Source: API (`max_stable_version`) |
| cargo-component | `cargo-component` | 0.21.1 | 2025-03-18 | No release for over a year; check the project status before relying on it. Source: API |
| keyring | `keyring` | 4.2.0 | 2026-08-29 | `MIT OR Apache-2.0`; `rust-version` 1.88. Re-verified 2026-10-20 (`max_stable_version` 4.2.0). Feature `v1` (the default, enabled explicitly with `default-features = false`) pulls the native stores `apple-native-keyring-store` 1.0.2, `windows-native-keyring-store` 1.1.0 and `zbus-secret-service-keyring-store` 1.0.1 over `keyring-core` 1.0.1, each per target. Used by `sqler-config`. Source: API; `cargo info keyring@4.2.0`; `Cargo.lock` |
| cargo-dist | `cargo-dist` | 0.32.0 | 2026-05-22 | The tool was renamed `dist`, but the `dist` crate on crates.io is an unrelated 2016 crate (0.0.0), so the crate is still `cargo-dist`. GitHub `axodotdev/cargo-dist` latest release is v0.33.0 (2026-09-10), newer than crates.io; verify the install channel in M7. Source: API for `cargo-dist` and `dist`; `cargo info cargo-dist`; GitHub releases API |
| thiserror | `thiserror` | 2.0.21 | 2026-09-23 | Derive for typed library errors; `rust-version` 1.77. Used by `sqler-core`, `sqler-driver-api` and `sqler-ui`. Checked 2026-10-20. Source: API; `cargo info thiserror` |
| tokio | `tokio` | 1.54.0 | 2026-10-19 | Async runtime for the CLI (`rt`), driver tests (`rt`), `sqler-ui` (`rt`, `sync`; dev: `rt-multi-thread`), `sqler-catalog` (`sync`; dev: `rt`) and `sqler-app` (`rt-multi-thread`); sqlx `runtime-tokio` pulls it in. Checked 2026-10-20. Source: API |
| futures-core | `futures-core` | 0.3.34 | 2026-08-11 | `Stream` trait for the `SQLite` query stream; same version as sqlx uses. Checked 2026-10-20. Source: API |
| anyhow | `anyhow` | 1.0.104 | 2026-07-18 | Error context in the `sqler-cli` binary only. Checked 2026-10-20. Source: API |
| criterion | `criterion` | 0.8.2 | 2026-02-04 | Bench harness candidate; rejected for the grid bench (ADR-0006); no harness crate added. Checked 2026-10-20. Source: API (`curl -s -A "sqler-bench-survey (github.com/sqler)" https://crates.io/api/v1/crates/criterion \| jq -r '.crate.max_stable_version, .crate.updated_at'`) |
| divan | `divan` | 0.1.21 | 2025-04-10 | Bench harness candidate; rejected for the grid bench (ADR-0006); no harness crate added. Checked 2026-10-20. Source: API (same command for `divan`) |
| serde | `serde` | 1.0.229 | 2026-07-18 | `derive` feature; config file DTOs in `sqler-config`. `MIT OR Apache-2.0`. Checked 2026-10-20. Source: API; `cargo info serde` |
| toml | `toml` | 1.1.8 | 2026-10-09 | crates.io reports `1.1.8+spec-1.1.0`; Cargo ignores the build metadata. Default features. `MIT OR Apache-2.0`. Used by `sqler-config`. Checked 2026-10-20. Source: API; `cargo info toml` |
| directories | `directories` | 6.0.0 | 2025-01-12 | Evaluated, not used: it pulls `dirs-sys` 0.5.0 (`MIT OR Apache-2.0`), which pulls `option-ext` 0.2.0 (2023-01-11), licensed `MPL-2.0`, not on the `deny.toml` allow-list. See ADR-0005. Checked 2026-10-20. Source: API for `directories`, `dirs-sys`, `option-ext` |
| insta | `insta` | 1.49.0 | 2026-10-03 | Snapshot tests for generated DDL (AGENTS.md, prompt §6). `Apache-2.0`; `rust-version` 1.66. `default-features = false`. Dev-only, `sqler-sql`. Checked 2026-10-20. Source: API (`max_stable_version`) |
| proptest | `proptest` | 1.12.0 | 2026-10-10 | Property tests for identifier quoting. `MIT OR Apache-2.0`; `rust-version` 1.88. `default-features = false`, feature `std`. Dev-only, `sqler-sql`. Checked 2026-10-20. Source: API (`max_stable_version`) |
| russh | `russh` | 0.64.1 | 2026-10-05 | Pure-Rust SSH client and server, `Apache-2.0`, `rust-version` 1.89; used by `sqler-tunnel` (ADR-0013). Declared with `default-features = false` and feature `ring`: the default `aws-lc-rs` backend is not used (it builds C with CMake/NASM on Windows and `aws-lc-sys` carries extra licence terms). `ring` 0.17.14 is `Apache-2.0 AND ISC`, accepted by `deny.toml`. `cargo tree -p sqler-tunnel -i openssl-sys` and `-i aws-lc-sys` match nothing. `cargo deny check`: licenses ok, no licence exception needed, no advisory ignore for `rsa`; the `rsa` crate is not in the tree (`cargo tree -i rsa` matches nothing). Verified 2026-10-20. Source: API; `cargo info russh@0.64.1`; docs and source of 0.64.1 |
| rand | `rand` | 0.10.3 | 2026-09-20 | Dev-dependency of `sqler-tunnel`: generates the host and client keys of the in-process test server (the version `russh` 0.64.1 uses). `MIT OR Apache-2.0`. Verified 2026-10-20. Source: API |
| tokio-stream | `tokio-stream` | 0.1.19 | 2026-07-22 | Dev-dependency of `sqler-tunnel` (feature `net`): `UnixListenerStream` for the test SSH agent. `MIT`. Verified 2026-10-20. Source: API |

## CI tools and actions

Checked on: 2026-10-20

Actions are referenced by their latest major tag; Cargo tools are pinned to an exact version.

Source commands:

- Actions: `gh api repos/<owner>/<repo>/releases/latest --jq '"\(.tag_name) \(.published_at)"'`
- Tools: `curl -s -A "sqler-ci (github.com/sqler)" https://crates.io/api/v1/crates/<name> | jq -r '.crate.max_stable_version, .crate.updated_at'`

| Component | Source | Pinned | Released | Notes |
| --------- | ------ | ------ | -------- | ----- |
| `actions/checkout` | GitHub release | `v7` (v7.0.1) | 2026-07-20 | Repository checkout; re-checked 2026-10-20, still the latest major |
| `Swatinem/rust-cache` | GitHub release | `v2` (v2.9.2) | 2026-08-06 | Cargo cache; re-checked 2026-10-20, still the latest major |
| `taiki-e/install-action` | GitHub release | `v2` (v2.87.29) | 2026-10-16 | Prebuilt tool binaries |
| `actions/upload-artifact` | GitHub release | `v7` (v7.0.2) | 2026-10-07 | Spike results artifacts (`spike.yml`) |
| `hyperfine` | crates.io, GitHub release | 2.0.0 | 2026-10-07 | Cold-start measurement in `spike.yml`, installed through `taiki-e/install-action`. 2.0 JSON: mean at `results[0].summary.time_wall_clock.mean` |
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
- `sqlx` 0.9.0: database backend of `sqler-driver-sqlite` and `sqler-driver-postgres` (AGENTS.md §6).
  The workspace line carries only `sqlite-bundled` (compiles SQLite from source, so Windows and macOS
  CI need no system library) and `runtime-tokio`; no `macros`, `migrate`, `json`, `any` or TLS. The
  plain `sqlite` feature was not used because it also enables `deserialize`, `load-extension` and
  `unlock-notify`. The first build is slower because it compiles C. `sqler-driver-postgres` adds
  `postgres` and `tls-rustls-ring-native-roots` on its own dependency line, so a build with only
  `driver-sqlite` pulls no TLS. `rustls` runs on the `ring` provider (no `aws-lc`, which needs cmake
  and NASM on Windows) and reads the OS trust roots through `rustls-native-certs` (no `webpki-roots`,
  whose CDLA licence is not on the allow-list). `openssl-sys`, `native-tls` and `aws-lc-sys` are not in
  the tree. The MySQL RSA feature (`mysql-rsa`, RUSTSEC-2023-0071) is not enabled. Versions of the TLS
  crates are those in `Cargo.lock`: `rustls` 0.23.46, `ring` 0.17.14, `rustls-native-certs` 0.8.5.
  The DB tests use the `postgres:18-alpine` image (ADR-0009).
- `tokio` 1.54.0: required by sqlx `runtime-tokio`. Declared with `default-features = false`; the
  CLI and the driver tests add only `rt`. The CLI runs a current-thread runtime. `sqler-ui` adds `rt` and
  `sync` (`oneshot`, ADR-0007); `sqler-app` adds `rt-multi-thread` for the two-worker database runtime.
- `futures-core` 0.3.34: only the `Stream` trait, to drive sqlx row streams without the full
  `futures` crate. One version in the tree.
- `anyhow` 1.0.104: error chains with context in the binary only (AGENTS.md §6); libraries keep
  `thiserror`.
- Std-only bench harness (ADR-0006): `crates/sqler-ui/benches/grid_viewport.rs` is a `harness = false` binary, so `criterion` and `divan` are not added. There is no new runtime dependency: `sqler-ui` reuses `sqler-driver-api`; `sqler-driver-sqlite` and `tokio` (`rt`) are dev-dependencies only.
- `serde` 1.0.229 (`derive`) and `toml` 1.1.8: parse and write the `sqler-config` files
  (`settings.toml`, `keybindings.toml`, `connections.toml`) into private DTOs (ADR-0005).
  `sqler-core` does not depend on them. `toml` `rust-version` is 1.85, below the workspace's 1.99.
- `keyring` 4.2.0 (feature `v1`): OS keychain access for `sqler-config` (ADR-0012). 4.x moved the backends into separate store crates that `v1` selects per target: macOS Keychain, Windows Credential Manager, and the Linux/FreeBSD Secret Service over pure-Rust `zbus` with RustCrypto (`crypto-rust`) and the `async-io` runtime. Linux CI needs no new apt packages. Not used: `dbus-secret-service` (needs `libdbus`), `linux-keyutils` (does not persist), OpenSSL crypto. The new store crates, `keyring-core`, `secret-service`, `aes`, `cbc`, `hkdf` and `sha2` are all `MIT OR Apache-2.0` (or similarly allowed); `cargo deny` needed no change.
- `directories` 6.0.0: not used. Its dependency `option-ext` is `MPL-2.0`, which `deny.toml` does not
  allow, and `deny.toml` changes need an ADR. `sqler-config` resolves the three platform paths with
  `std` only (ADR-0005).
- GPUI distribution (ADR-0001): `gpui-component` 0.8.0 depends on `gpui-pre` `=0.3.9`, so that crate
  (plus `gpui-pre-platform` for the application and window creation) is the one GPUI in the tree;
  `gpui` 0.2.2 and `gpui-ce` 0.2.2 would add a second one. Pin `gpui-component` and `gpui-pre` together
  and bump them as one unit. The check is `cargo tree -i gpui-pre` (and `gpui`/`gpui-ce` must not match).
- GPUI in the root workspace (T5, ADR-0007): `gpui-component` 0.8.0 (with `tree-sitter-sql`), `gpui` (`gpui-pre` =0.3.9) and `gpui_platform` (`gpui-pre-platform` =0.3.9) are optional dependencies of `sqler-ui` behind the feature `gui`, enabled only by `sqler-app`. `cargo deny` now sees the tree; the three MPL-2.0 exceptions and eight advisory ignores it needed are listed in ADR-0007. Versions re-checked live on 2026-10-20: no newer stable `gpui-component`, `gpui-pre` or `gpui-pre-platform` than the pinned ones, so there is no pending bump.
- `russh` 0.64.1 (T6, ADR-0013): SSH session and `direct-tcpip` forward in `sqler-tunnel`; pure Rust, so no `OpenSSH` binary and no C SSH library (`ssh2` binds libssh2). Feature `ring` only, with `default-features = false`; the default `aws-lc-rs` backend is rejected (CMake/NASM on Windows, extra licence terms in `aws-lc-sys`). `ring` needs a C compiler through `cc`, which MSVC on `windows-latest` provides. The `rsa` feature is deliberately off: it pulls in the `rsa` crate, which carries RUSTSEC-2023-0071 (Marvin timing attack, no patched version), and `deny.toml` has no ignore for it. Consequence: RSA user keys and RSA-only host keys are unsupported until the product owner decides on the advisory; `sqler-tunnel` reports an RSA private key file as `TunnelError::UnsupportedKeyType` (use an ed25519 or ECDSA key). Re-enabling is one feature flag (`rsa` on `russh`) plus the advisory ignore. No licence exception was needed (all crates in the `sqler-tunnel` tree are on the allow-list). `cargo deny check` reports duplicate-version warnings for RustCrypto crates (`multiple-versions = "warn"`). `russh` logs through the `log` crate; no logger is installed and it does not log credentials. `rand` 0.10.3 and `tokio-stream` 0.1.19 are dev-dependencies of `sqler-tunnel` only.
- Spike-only dependency `memory-stats`: lives in `spikes/ui-framework/`, outside the root workspace.
- `actions/upload-artifact` and `hyperfine`: used only by the spike workflow `spike.yml`.
- `cargo-nextest`: faster, per-test-process runner used by CI and the local commands.
- `cargo-deny`: enforces the Apache-2.0-compatible license allow-list in `deny.toml`, plus bans, sources and advisories.
- `mdbook`: builds the documentation book in CI and for GitHub Pages.
- `taiki-e/install-action`: installs the pinned tools as prebuilt binaries instead of compiling them in every run.
