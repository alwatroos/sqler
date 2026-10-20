# sqler-driver-postgres

`PostgreSQL` driver implementation built on `sqlx` (ADR-0009).

Status: implemented. `PostgresDriver` (id `postgres`) implements `Driver`, `Session` and `Introspect`: streamed results in batches of 256 rows, server-side cancel, TLS (`rustls` with `ring` and the OS trust roots) and all 13 object kinds.

Layer: L2

The target is a `postgres://` or `postgresql://` URL. See the user guide, `docs/src/user-guide/databases/postgresql.md`, for TLS, types and limits.

## Tests

Unit tests run everywhere. The integration tests (`tests/db_postgres*.rs`) need a server and skip with a message when `SQLER_TEST_POSTGRES_URL` is not set. To run them against a local Docker server with TLS:

```sh
eval "$(sh crates/sqler-driver-postgres/tests/docker/start-postgres.sh target/pg | sed 's/^/export /')"
cargo nextest run -p sqler-driver-postgres
```

`SQLER_TEST_POSTGRES_CA` (set by the script) enables the TLS tests. `SQLER_TEST_REQUIRE_DB=postgres` turns a missing variable into a failure (CI sets it). Each test works in its own schema, `sqler_t_<test>`. Remove the server afterwards with `docker rm -f sqler-test-postgres`.

See [AGENTS.md](../../AGENTS.md) for the project handbook.
