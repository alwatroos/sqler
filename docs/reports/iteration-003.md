## Iteration 003 — 2026-10-20
### Delivered
M2 (connections and drivers), items in merge order:
- Keychain secret store: `SecretStore`, `KeychainStore` on `keyring` 4.2.0, `MemoryStore` (PR #16, `docs/src/user-guide/secrets.md`)
- Connection store: groups, colour, environment, TLS and SSH profiles, versioned TOML (PR #17, `docs/src/user-guide/connections.md`)
- Object model, lazy introspection, typed values and TLS options in core and the driver API (PR #18, `docs/src/user-guide/databases/index.md`)
- `sqler-tunnel`: SSH local-forward tunnel on `russh` (PR #19, `docs/src/user-guide/ssh-tunnels.md`)
- DDL and script generation for `PostgreSQL`, `MySQL`/`MariaDB` and `SQLite` (PR #20, `docs/src/user-guide/ddl.md`)
- `sqler-catalog`: lazy cached schema tree with background refresh (PR #21, `docs/src/architecture/catalog.md`)
- `PostgreSQL` driver with DB tests and a CI `db` job (PR #22, `docs/src/user-guide/databases/`)
- `MySQL` and `MariaDB` driver (PR #23, `docs/src/user-guide/databases/`)
- Connection manager UI and connect pipeline (PR #24, `docs/src/user-guide/connections.md`, `docs/src/architecture/ui.md`)
- Object browser: lazy tree, menus, DDL and Properties tabs, guarded DROP (PR #25, `docs/src/user-guide/object-browser.md`)
- Metrics and this report (PR [11/11], `docs/src/benchmarks.md`)
### Decisions
- ADR-0008 Object model, lazy introspection, typed values and TLS options (accepted) — one object model for all drivers; `Introspect` loads one level at a time and reports unsupported kinds as `Listing::Unsupported`.
- ADR-0009 `PostgreSQL` driver (accepted) — sqlx, server-side cancel, rustls TLS, and the DB test convention `SQLER_TEST_<DB>_URL` with a start script per database.
- ADR-0010 `MySQL` and `MariaDB` driver (accepted) — sqlx, `KILL QUERY` on a side connection for cancel, no RSA password exchange.
- ADR-0011 Connection store format (accepted) — versioned TOML with groups and derived secret references; M1 files still load.
- ADR-0012 Keychain secrets (accepted) — `keyring` 4.x, reference scheme `<id>:<kind>`, no plain-text fallback.
- ADR-0013 SSH tunnel crate (accepted) — `russh` with `ring` only, strict `known_hosts`, loopback-only forward.
- ADR-0014 Catalog cache, worker concurrency and background refresh (accepted) — one worker serialises introspection, loads are deduplicated, refresh reports progress and can be cancelled.
- ADR-0015 DDL generation: dialects and quoting (accepted) — generation lives in `sqler-sql` per dialect with explicit identifier quoting.
- ADR-0016 Connect pipeline (accepted) — `Connector` trait in `sqler-ui`, pipeline (secrets, tunnel, target rewrite, driver registry) in `sqler-app`.
- ADR-0017 Object browser sessions and DROP confirmation (accepted) — a browser session per connection, short-lived action sessions, a verbatim statement and a second step on `prod`.
### Metrics
Measured on 2026-10-20, macOS 27.0.1 arm64 (Apple M4 Pro), commit `ba24c9a`, `target/release/sqler` (default features, `--locked`). Tracked in [benchmarks](../src/benchmarks.md).
- Cold start: 155.1 ms mean (σ 11.0, min 138.3, max 167.2), `hyperfine` 20 runs after 3 warm-ups of `sqler --exit-after-first-frame`. Target < 500 ms: met on macOS. M1 was 140.6 ms.
- Idle RSS: 100.0 to 100.2 MiB (three runs, `ps` at 6 s and 10 s) with three saved connections (`SQLite` `:memory:`, `PostgreSQL`, `MySQL`; both servers running in Docker). The sessions were not opened: the app has no headless way to open one and an agent cannot double-click, so the "three connections open" figure is still pending the maintainer. Target < 150 MB. M1 was 98.1 MiB.
- Binary size: 38.3 MiB (40,168,784 bytes, 40.2 MB decimal). Target < 60 MB: met. M1 was 29.5 MiB. The Linux and Windows `sqler` binaries are not measured; the M1 Linux spike build was 57.7 MB.
- Catalog tree (cited from `benchmarks.md`, `7d80fbf` plus the PR, fake source, 5,000 tables): cold expand p50 203 µs, p95 351 µs; cached read p50 41 ns, p95 42 ns (timer resolution, below one tick); refresh all 4.44 ms. With a `SQLite` `:memory:` source: expand p95 2.26 ms, refresh all 552.1 ms (reference, no budget).
- Object browser tree (cited, `5672fc0` plus the PR, 5,000 tables): expand p50 627 to 649 µs, p95 1.04 to 1.12 ms (budget 16 ms); selection p95 2.0 to 3.2 µs (budget 1 ms).
- Results grid (cited from M1, unchanged): viewport fetch p95 334 ns for 1M rows.
- Completion p95: n/a until M3.
- Test count: 814 tests run, 814 passed, 2 skipped (`cargo nextest run --workspace --no-tests=pass` with `PostgreSQL` 18 and `MySQL` 8.4 in Docker and `SQLER_TEST_REQUIRE_DB=postgres,mysql`), up from 299 at the end of M1. `MariaDB` 11.8: `cargo nextest run -p sqler-driver-mysql --no-tests=fail`, 26 passed.
- Coverage: not measured (`cargo llvm-cov` is not installed).
- Linux and Windows desktop numbers: not measured, because `ci.yml` has no GUI runner.
### Risks & blockers
- M2 Definition of Done is not fully verified, and three product decisions are open (next section). Pending maintainer checks, none of which an agent can do:
  - M1 checks from iteration 002 (shell, settings and themes, palette and keybindings, grid scrolling) are still open.
  - Keychain (PR #16): `cargo test -p sqler-config -- --ignored keychain_round_trip` on macOS, Windows and Linux desktop sessions, and a look at the OS keychain UI.
  - SSH (PR #19): a real bastion with `SQLER_TEST_BASTION_*` and `cargo test -p sqler-tunnel --test real_bastion -- --ignored`; the Windows named-pipe agent code is compiled by CI only.
  - Connection manager (PR #24): masked inputs and focus when a dialog opens, Escape with an input focused, drag preview, keychain prompts on the three OSes, the SSH host-key prompt with a real bastion.
  - Object browser (PR #25): context menus, the DROP dialog and its `prod` second step, DDL and Properties tabs and Copy, scrolling about 5,000 tables, keyboard navigation, clipboard.
  - Idle RSS with three sessions actually opened.
- Known limits: a `MySQL` cancel that arrives just as a statement ends can hit the next statement of the same session (ADR-0010); the "Dropping..." step of the object browser blocks the window until the server returns (ADR-0017).
- The 1M-row grid still holds about 235 MB (M1 risk); M4 needs a bounded result model before real queries feed it.
- Linux and Windows `sqler` numbers are unmeasured; the Linux spike binary was 57.7 MB and the app grew by 8.8 MiB on macOS in M2, so the 60 MB budget on Linux needs a real measurement soon.
- GPUI is a pre-release fork (`gpui-pre` =0.3.9) with MPL-2.0 exceptions and advisory ignores in `deny.toml`.
- GitHub Pages still needs enabling by hand.
### Decisions and open items for the product owner
- (a) RSA SSH keys are unsupported. `russh` is built with `ring` only, so an RSA private key gives `TunnelError::UnsupportedKeyType`, because enabling RSA pulls in the `rsa` crate (RUSTSEC-2023-0071, no fixed release). Re-enabling is the `russh` `rsa` feature plus an advisory ignore in `deny.toml`. Decision needed: accept the advisory, or keep RSA keys unsupported (ed25519 and ECDSA keys work).
- (b) The plaintext RSA password exchange for `MySQL` is unsupported (`sqlx` `mysql-rsa` feature off, same advisory). A plaintext login to an account that is not cached by `caching_sha2_password` fails unless TLS is used. Decision needed: same as (a).
- (c) `verify-full` TLS through an SSH tunnel is refused (fail-closed, `TlsHostnameThroughTunnel`), because the driver would verify the certificate against `127.0.0.1`. It stays refused until the core `TlsConfig` gets a server-name field and the drivers use it. Decision needed: schedule that work in M3 or later.
- (d) The maintainer-only manual checks listed above (keychain per OS, real bastion, UI interactions in PRs #24 and #25). Decision needed: who runs them and when, and whether M3 may start before they are recorded. The roadmap rule says a milestone does not start before the previous Definition of Done is met.
- (e) Known limits to accept or schedule: the `MySQL` cancel race window and the blocking "Dropping..." step.
### Next iteration plan
M3 (editor and intellisense), once the owner has answered the decisions above and the maintainer checks are recorded or waived:
- Tree-sitter editor with run modes, statement splitting and formatting (`sqler-sql`).
- Query history and saved queries.
- `sqler-completion` with alias resolution, hover and diagnostics, property-based tests, completion p95 < 16 ms on a 5,000-table synthetic schema (fed by `sqler-catalog`).
- Run editor queries through the session bridge into the grid; keep the 2 s bound on closing sessions.
- Add a Linux and Windows desktop measurement and an app path that opens sessions without a click, so idle RSS with three open sessions can be measured by a script.
### AGENTS.md changes
- Section 9: roadmap status now reads "M2 code complete; Definition of Done pending maintainer verification and product-owner decisions; M3 next", with the delivered list, the measured numbers and the explicit lists of pending maintainer checks (carried from the PR bodies of #16, #19, #24 and #25, plus the M1 items) and pending owner decisions.
### Raw measurements
Commands run at `ba24c9a` on macOS 27.0.1 arm64 (Apple M4 Pro), from the repository root; `HOME` pointed at a scratch directory whose `Library/Application Support/SQLer/connections.toml` held a `SQLite` `:memory:`, a `PostgreSQL` (`127.0.0.1:54329`) and a `MySQL` (`127.0.0.1:33069`) entry.
- Build: `cargo build --release -p sqler-app --locked`.
- Binary size: `stat -f %z target/release/sqler` printed `40168784` (40168784 / 1048576 = 38.3 MiB).
- Cold start: `hyperfine -w 3 -r 20 'target/release/sqler --exit-after-first-frame'` reported mean 155.1 ms, σ 11.0 ms, min 138.3 ms, max 167.2 ms (peak memory per run 68.4 MiB).
- Idle RSS: `ps -o rss= -p <pid>` (KiB) at 6 s and 10 s, three runs: 102448 and 102400, 102464 and 102400, 102592 and 102544 (100.0 to 100.2 MiB).
- Test count: `PostgreSQL` 18 (`start-postgres.sh`) and `MySQL` 8.4 (`start-mysql.sh mysql`) in Docker, `SQLER_TEST_REQUIRE_DB=postgres,mysql`, `cargo nextest run --workspace --no-tests=pass`: 814 run, 814 passed, 2 skipped. Then `MariaDB` 11.8 (`start-mysql.sh mariadb`): `cargo nextest run -p sqler-driver-mysql --no-tests=fail`: 26 passed. Both containers were removed afterwards.
