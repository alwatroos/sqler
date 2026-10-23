# 0009 `PostgreSQL` driver: sqlx, server-side cancel, rustls TLS, DB test convention

- Status: accepted
- Superseded in part by [ADR-0069](0069-binary-size-budget-70-mb.md) (2026-10-23): the binary size budget is now 70 MB (73,400,320 bytes); the 60 MB figures below are the budget at the time of writing.
- Date: 2026-10-20

## Context and problem statement

M2 adds the first network driver, for `PostgreSQL` (`sqler-driver-postgres`, driver id `postgres`). It must stream rows in batches, cancel a running statement on the server, honour the TLS options of `ConnectionConfig` (ADR-0008), map `PostgreSQL` types to the typed `Value`s of ADR-0008 and browse all 13 object kinds. The `MySQL` driver (T3) and later drivers need the same TLS backend and a shared way to test against a real server. AGENTS.md §8 requires an ADR before a new dependency feature or a cross-cutting convention.

## Decision drivers

- Row streaming straight off the socket, no full materialisation.
- Cancel that stops the statement on the server, not only in the client (ADR-0004).
- One TLS stack for all network drivers: no OpenSSL, no `aws-lc` (needs cmake and NASM), no CDLA-licensed data crate.
- `sqler-cli --no-default-features --features driver-sqlite` must still pull no TLS code.
- Tests against a real server that run on the Linux CI job, can also be run by hand, and skip cleanly where Docker is absent.

## Considered options

Client library:

1. `sqlx` 0.9 with the `postgres` feature (already used for `SQLite`).
2. `tokio-postgres`.

Test server:

3. The `testcontainers` crate.
4. GitHub Actions `services:`.
5. A `docker run` shell script, used by CI and by hand.

## Decision outcome

Chosen: options 1 and 5.

**Library.** `sqlx` streams rows with `fetch` as they arrive. It has no cancel API, so the driver cancels on the server: `PgCanceller` holds the session's connect options and its backend pid (read once at connect with `select pg_backend_pid()`). `cancel()` opens a short side connection with the same options and runs `select pg_cancel_backend(<pid>)`, bounded by a 10 s timeout. A cancel is reported as `DriverError::Cancelled` only when SQLSTATE `57014` arrives while a cancel was requested; a `statement_timeout` with the same code stays a `Query` error. `tokio-postgres` does have a `CancelToken`, but it would add a second TLS stack (its own `rustls` glue plus our own `verify-ca` and `verify-full` verifiers) while T3 stays on `sqlx`.

Cells are decoded from the binary wire format by type OID into `Value`s, with no new crates: `chrono`, `time` and `bigdecimal` are not used, and `rust_decimal` cannot hold `numeric`. A cell that fails to decode falls back to text or bytes and never fails the query. `uuid`, `timetz`, `interval`, `inet`, `cidr` and arrays are shown as text; `timestamptz` is shown in UTC.

**TLS backend (binding for T3 and later drivers).** `sqlx` feature `tls-rustls-ring-native-roots`, enabled per driver crate and not on the workspace dependency line. The `rustls` crate uses the `ring` provider and the operating system trust roots.

- `openssl-sys`, `native-tls`, `aws-lc-sys` and `aws-lc-rs` must not appear in the dependency tree. Check with `cargo tree --workspace -i <crate>`.
- The `MySQL` driver uses `features = ["mysql", "tls-rustls-ring-native-roots"]`.
- With `tls: Some(..)` the options of `ConnectionConfig` override the `sslmode`, `sslrootcert`, `sslcert` and `sslkey` parameters of the URL; with `tls: None` the URL applies, and the default is `prefer`.
- Fallback, only if a later `sqlx` drops the feature: `tls-rustls-ring-webpki` and a per-crate `deny.toml` exception for `webpki-roots` (CDLA-Permissive-2.0, data only).

**DB test convention (binding for T3 and later drivers).**

- Each database has `SQLER_TEST_<DB>_URL` and, for TLS, `SQLER_TEST_<DB>_CA`. When the URL is unset, a test prints `skipped: <VAR> is not set (see AGENTS.md §3)` and passes.
- `SQLER_TEST_REQUIRE_DB` is a comma-separated list of database ids. A listed id turns a missing variable into a panic; CI sets it so nothing skips silently. An unreachable server always fails.
- Test files are `tests/db_<db>*.rs`, selectable with the nextest filter `-E 'binary(/^db_/)'`. Every test works in its own schema `sqler_t_<test>`, dropped first.
- `tests/common/mod.rs` of each crate has `db_env(var, db) -> Option<String>`. There is no shared test crate.
- `tests/docker/start-postgres.sh <outdir>` creates a CA and a server certificate (SAN `DNS:localhost` only), starts `postgres:<major>-alpine` on `127.0.0.1:54329` with `ssl=on`, waits until it accepts TCP connections and prints `KEY=value` lines on stdout. In CI they go to `$GITHUB_ENV`. It is idempotent.
- CI has one Linux job per database family, named `db`, without the GPUI apt packages. It also runs the CLI smoke test. T3 adds a step to it.

**Rejected: `testcontainers`.** It pulls `bollard` and `hyper` and their tree into the lock file, has nothing useful to detect on macOS and Windows, and cannot be run by hand. **Rejected: `services:`.** It cannot pass server arguments, so `-c ssl=on` is impossible and the TLS modes could not be tested; the shell script can also start the server locally.

Versions of `sqlx`, the TLS crates and the Postgres image tag are in [Dependencies](../dependencies.md).

## Consequences

- Cancel opens a second connection, which needs a free connection slot and the same role (or `pg_signal_backend`); the side connection uses the session's own credentials. A cancel that arrives just as a statement ends can hit the next statement on the same session; the `active` flag keeps that window small. It is documented for users.
- The driver needs a Tokio runtime with the time driver enabled.
- Browsing another database than the session's is `DriverError::Unsupported`; the UI opens a new session for it.
- The CLI and app binaries grow by `rustls` and `ring`; the release size is checked against the 60 MB budget in the PR.
- Integration tests need Docker; on a machine without it they skip, so only the Linux `db` job proves them.
- Client-certificate authentication is covered by option-mapping unit tests only, not by an integration test.
