# PostgreSQL

The `PostgreSQL` driver (id `postgres`) connects over TCP, with or without TLS. It needs server
version 12 or newer.

## Targets

A target is a `postgres://` or `postgresql://` URL:

```text
postgres://user:password@host:5432/database?sslmode=verify-full&sslrootcert=/path/ca.pem
```

| URL parameter | Meaning |
| ------------- | ------- |
| `sslmode` | `disable`, `prefer` (the default), `require`, `verify-ca` or `verify-full`. |
| `sslrootcert` | Path of the CA certificate file. |
| `sslcert`, `sslkey` | Paths of the client certificate and its key. |

The driver sets the application name to `sqler`.

## TLS

The driver uses `rustls` and the operating system trust roots. The modes are those of
[TLS modes](index.md#tls-modes):

| Mode | Encrypted | Certificate checked | Host name checked |
| ---- | --------- | ------------------- | ----------------- |
| `disable` | no | no | no |
| `prefer` | if the server offers it | no | no |
| `require` | yes | no | no |
| `verify-ca` | yes | yes | no |
| `verify-full` | yes | yes | yes |

When a connection has TLS options (a TLS mode, a CA file, a client certificate and key), they replace
the four `ssl` parameters of the URL, which are then ignored. A connection without TLS options uses
the URL parameters. A client certificate needs a client key and the other way round; setting only one
of them is a connection error.

The standard `libpq` environment variables `PGSSLMODE`, `PGSSLROOTCERT`, `PGSSLCERT` and `PGSSLKEY`
still apply to a setting that neither the connection's TLS options nor the URL sets.

## Cancel

Cancel stops the statement on the server: the driver opens a second short connection with the same
credentials and calls `pg_cancel_backend` for the session's backend. The stream then ends with a
cancelled error. The role must be the same as the session's, or have `pg_signal_backend`; the second
connection uses the session's own credentials, so this holds.

A cancel that arrives just as a statement ends can hit the next statement that runs on the same
session. A statement stopped by `statement_timeout` is reported as a query error, not as a cancel.

## Types

Results are read in the binary protocol and map to typed values.

| `PostgreSQL` type | Value |
| ----------------- | ----- |
| `bool` | Boolean |
| `int2`, `int4`, `int8`, `oid` | Integer |
| `float4`, `float8` | Float |
| `numeric` | Decimal, exact; `NaN` and infinity as text |
| `text`, `varchar`, `bpchar`, `name`, `xml`, enums | Text |
| `bytea` | Bytes |
| `json`, `jsonb` | JSON |
| `date`, `time` | Date, Time |
| `timestamp` | Timestamp |
| `timestamptz` | Timestamp with zone, always in UTC (`+00:00`), not in the session `TimeZone` |
| `uuid`, `timetz`, `interval`, `inet`, `cidr` | Text |
| arrays | Text, as an array literal such as `{1,NULL,"a b"}` |

`infinity` and `-infinity` for dates and timestamps, and the time `24:00:00`, are shown as text. Any
other type is shown as text when its value is valid UTF-8, else as bytes.

Query parameters are bound as integers, floats, booleans, text, bytes or NULL. Decimal and temporal
parameters are sent as text, so the SQL needs a cast, for example `$1::numeric` or `$1::date`.

## Object browsing

All 13 kinds are supported: database, schema, table, view, materialized view, function, procedure,
trigger, sequence, type, index, extension and role.

- Only the current database can be browsed. Asking for another one is reported as unsupported with
  the name of the current database; open a new connection to it.
- A missing schema means the current schema. User schemas are listed first, then `pg_catalog`,
  `information_schema` and other `pg_*` schemas. Toast and temporary schemas are hidden.
- Functions and procedures are named with their arguments, for example `add(integer, integer)`, so
  overloads are distinct.
- Triggers and indexes can be listed per table. Types are base, composite, domain, enum and range
  types; array types are not listed. Roles whose names start with `pg_` are hidden.
- A table has columns with the native type, nullability and default, the primary key and the foreign
  keys.
- Definition text comes from the server: `pg_get_viewdef` for views and materialized views,
  `pg_get_functiondef` for functions and procedures, `pg_get_triggerdef` and `pg_get_indexdef`. Other
  kinds have none.
