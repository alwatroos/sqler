# sqler-driver-mysql

`MySQL` and `MariaDB` driver implementation.

Status: implemented (M2)

Layer: L2

Implements the `sqler-driver-api` `Driver`, `Session`, `QueryStream` and `Introspect` traits on
`sqlx`'s `MySQL` backend. One connection per session; no pool. The driver id `mysql` serves both
servers.

## Usage

```rust,ignore
let mut registry = sqler_driver_api::DriverRegistry::new();
registry.register(Box::new(sqler_driver_mysql::MySqlDriver::new()))?;
```

The `ConnectionConfig::target` is a `mysql://` or `mariadb://` URL, for example
`mysql://user:password@localhost:3306/database?ssl-mode=required`.

## Runtime

`sqlx` runs on Tokio: the futures returned by the driver must be polled inside a Tokio runtime
context.

## Results

Every statement is prepared, so results use the binary protocol and stream in batches of 256 rows.
A statement the server cannot prepare (error 1295, for example `CREATE PROCEDURE`) and that has no
parameters runs over the text protocol. Temporal cells are decoded from the binary layout (no
`chrono`); zero dates and `TIME` values that are negative or 24 hours or longer come back as `Text`.
Unsigned `BIGINT` values above `i64::MAX` are `Decimal`. Text in a `_bin` collation, and
`MariaDB`'s `JSON` alias, arrive as `Bytes` because the protocol flags them as binary.

## Cancellation

Cancelling sets a flag and runs `KILL QUERY <connection id>` from a short-lived side connection
built from the same options. The stream then ends with `Err(Cancelled)`. A cancel that reaches the
server before the statement does has no effect there, but the flag still ends the stream at its
first poll.

## TLS

TLS uses `rustls` with the system trust store. With no `ConnectionConfig::tls`, the URL parameters
(`ssl-mode`, `ssl-ca`, `ssl-cert`, `ssl-key`) apply and the default is `PREFERRED`. A `Some` TLS
config overrides them. The RSA password exchange over an unencrypted connection is not compiled in
(it would pull the `rsa` crate, RUSTSEC-2023-0071): use TLS, or log in once over TLS so the server
caches the account.

## Introspection

Databases are the schemas (there is no database level). Schemas, tables, views, functions,
procedures, triggers, indexes and accounts (`user@host`) are listed from `information_schema`;
definitions come from `SHOW CREATE`. Other kinds are reported as unsupported.

## Tests

The integration tests `tests/db_mysql*.rs` read `SQLER_TEST_MYSQL_URL` and `SQLER_TEST_MYSQL_CA`
(ADR-0009, ADR-0010). `tests/docker/start-mysql.sh <outdir> [mysql|mariadb]` starts `mysql:8.4` or
`mariadb:11.8` with TLS and prints them. Without the URL the tests print a skip message and pass;
`SQLER_TEST_REQUIRE_DB=mysql` makes a missing URL an error.

See [AGENTS.md](../../AGENTS.md) for the project handbook.
