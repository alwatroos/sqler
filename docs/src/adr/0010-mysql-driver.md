# 0010 `MySQL` and `MariaDB` driver: sqlx, KILL QUERY cancel, no RSA exchange

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

M2 adds the `MySQL` driver (`sqler-driver-mysql`, driver id `mysql`) after the `PostgreSQL` one (ADR-0009). It serves `MySQL` 8.4 and `MariaDB` 11.8 with one code path. It must stream rows in batches, cancel a running statement on the server, honour the TLS options of `ConnectionConfig` (ADR-0008), map `MySQL` types to the typed `Value`s of ADR-0008 and browse the object kinds the database has. AGENTS.md §8 requires an ADR before a new dependency feature.

## Decision drivers

- Row streaming straight off the socket, no full materialisation.
- Cancel that stops the statement on the server (ADR-0004).
- The TLS backend and the DB test convention of ADR-0009, unchanged.
- `cargo deny` stays clean without an ignore entry.
- No new crates for temporal values (ADR-0008 kept `chrono` out).

## Considered options

Client library:

1. `sqlx` 0.9 with the `mysql` feature (already used for `SQLite` and `PostgreSQL`).
2. `mysql_async`.

Password exchange:

3. `sqlx` feature `mysql-rsa` plus an `RUSTSEC-2023-0071` ignore in `deny.toml`.
4. Leave `mysql-rsa` off.

## Decision outcome

Chosen: options 1 and 4.

**Library.** `sqlx` streams rows with `fetch`. `mysql_async` is a second database stack with no streaming or cancel advantage. The driver keeps one `MySqlConnection` per session, no pool. Every statement is prepared, so results use the binary protocol and stream in batches of 256. A statement the server cannot prepare (error 1295, for example `CREATE PROCEDURE`) and that has no parameters runs through `sqlx::raw_sql` and returns an affected-only result.

**Cancel.** `sqlx` has no cancel API. At connect the driver reads `connection_id()`. `cancel()` sets a flag, opens a short side connection from the same `MySqlConnectOptions` and runs `KILL QUERY <id>`. The stream checks the flag before each batch and after every poll, because a killed `SLEEP()` returns 1 instead of an error; with the flag set, error 1317 or any poll result ends the stream with `Cancelled`. A statement without result columns runs inside the stream, not in `execute()`, so its cancel surfaces the same way (the flag is checked after it, since a killed `DO SLEEP()` returns OK). The canceller tracks whether a statement is running: when idle, `cancel()` only sets the flag and sends no `KILL`; otherwise the side connection and the kill share a 10 s timeout. A cancel that arrives just as a statement ends can still hit the next statement of the same session.

**TLS (ADR-0009).** `sqlx` features `mysql` and `tls-rustls-ring-native-roots` on the driver crate's own dependency line. `Some(TlsConfig)` overrides the URL: `Disable` to `Disabled`, `Prefer` to `Preferred`, `Require` to `Required`, `VerifyCa` to `VerifyCa`, `VerifyFull` to `VerifyIdentity`; the CA, client certificate and key paths map to `ssl_ca`, `ssl_client_cert` and `ssl_client_key`. With `tls: None` the URL parameters (`ssl-mode`, `ssl-ca`, `ssl-cert`, `ssl-key`) apply and the default is `PREFERRED`.

**No `mysql-rsa`.** With the feature on, `sqlx-mysql` pulls the `rsa` crate, which has `RUSTSEC-2023-0071` (Marvin attack) and no fixed version. The feature is left off, so the tree has no `rsa` and `cargo deny` needs no ignore. The price: the RSA public-key password exchange (`sha256_password`, and `caching_sha2_password` when the server has no cached entry for the account) fails over an unencrypted connection with the sqlx error "RSA auth backend disabled". Over TLS the password travels inside the TLS channel and works; with the default `PREFERRED` mode servers that offer TLS (all `MySQL` 8 servers) are fine. Over a plaintext link a cached `caching_sha2_password` account and `mysql_native_password` work. If a user needs the plaintext exchange, enabling `mysql-rsa` with a reasoned ignore is a one-line change.

**Values.** Cells are decoded by the column type reported with the row, with no new crates. Temporal cells are read from the documented binary layout (a length byte of 0 to 12, then little-endian fields; microseconds become nanoseconds). Any other first byte is parsed as text. Mapping:

| `MySQL` type | `DataType` | `Value` |
| ------------ | ---------- | ------- |
| `BOOLEAN` (`tinyint(1)`) | Boolean | Bool |
| `TINYINT` to `BIGINT`, `YEAR` | Integer | Int; unsigned above `i64::MAX` is Decimal |
| `FLOAT`, `DOUBLE` | Float | Float |
| `DECIMAL` | Decimal | Decimal, else Text |
| `CHAR`, `VARCHAR`, `*TEXT`, `ENUM`, `SET` | Text | Text (invalid UTF-8 is Bytes) |
| `BINARY`, `VARBINARY`, `*BLOB`, `BIT` | Binary | Bytes |
| `DATE`, `DATETIME`, `TIMESTAMP`, `TIME` | Date, Timestamp, Time | typed; zero dates, negative or 24 h and longer `TIME` are Text |
| `JSON` | Json | Json |
| `GEOMETRY` and others | Other | Bytes |

`sqlx` reports a zero date (`0000-00-00`) as NULL through `ValueRef::is_null()`, so the driver reads date columns as raw bytes and treats only a missing value as NULL; a zero date is shown as text. Parameters: Null, Bool, Int, Float, Text and Bytes are bound natively; Decimal, Date, Time and Timestamp are bound as text; Json as a string; other variants are `Unsupported`.

**`MariaDB` differences.** The protocol marks any text column with a `_bin` collation as binary, and `MariaDB` implements `JSON` as `LONGTEXT` with `utf8mb4_bin`. Such cells come back as Bytes on both servers, not as Text or Json. `COLUMN_DEFAULT` is reported with quotes around string literals, and `NULL` as text, by `MariaDB`; `MySQL` 8.4 reports the unquoted value. `table()` still reports the declared type.

**Introspection.** There is no database level: databases are the schemas (as with `SQLite`). Supported kinds: schema, table, view, function, procedure, trigger, index and role. Database, materialized view, sequence, type and extension are `Listing::Unsupported`. Everything is read from `information_schema` with bound parameters, definitions from `SHOW CREATE` (identifiers quoted with backticks, doubled). A missing schema means `DATABASE()`; a missing object gives `Ok(None)`. Roles are the accounts of `mysql.user` as `user@host`; without privilege the list is the single `CURRENT_USER()`. `MariaDB` sequences and events are not listed.

**DB tests (ADR-0009).** `tests/docker/start-mysql.sh <outdir> [mysql|mariadb]` creates a CA and a certificate for `DNS:localhost`, starts `mysql:8.4` or `mariadb:11.8` on `127.0.0.1:33069` with TLS on and prints `SQLER_TEST_MYSQL_URL` and `SQLER_TEST_MYSQL_CA`. The test files are `tests/db_mysql.rs` and `tests/db_mysql_introspect.rs`; they work in their own databases or tables and detect the flavour from `version()`. The CI `db` job runs them twice, once per server. `testcontainers` is not used (ADR-0009).

## Consequences

- The cancel needs a free connection slot and the `CONNECTION_ADMIN` privilege, or the same account as the session (an account may kill its own queries).
- Plaintext connections to a server whose account has no cached `caching_sha2_password` entry fail until the account logs in once over TLS.
- `_bin` text and `MariaDB` `JSON` arrive as bytes; showing them as text needs the column collation, which `sqlx` 0.9 does not expose.
- The cancel request has a 10 s timeout, so the driver needs a Tokio runtime with the time driver enabled.
- Browsing needs the privileges that `information_schema` applies; objects the account cannot see are not listed.
- The Docker-based tests run only on the Linux `db` job; elsewhere they skip.

Versions and images are in [Dependencies](../dependencies.md).
