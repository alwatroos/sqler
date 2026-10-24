# Databases

SQLer 1.0 connects to: SQLite, PostgreSQL, MySQL/MariaDB, SQL Server. For what is left out, see
[Not in 1.0](../connections.md#not-in-10).

SQLer talks to databases through drivers. This page lists what each driver offers. The pages below
describe one driver each.

- [SQLite](sqlite.md)
- [PostgreSQL](postgresql.md)
- [MySQL and MariaDB](mysql.md)
- [SQL Server](sqlserver.md) (a plugin driver, built in; approve it once in the Plugins view)

## Object kinds

The object browser and the DDL view ask the driver which kinds of object exist. A kind the database
does not have is reported as unsupported, not as an error.

| Kind | `SQLite` | `PostgreSQL` | `MySQL` | `SQL Server` |
| ---- | -------- | ------------ | ------- | ------------ |
| Database | no | yes | no | yes (listed, not browsed across) |
| Schema | yes (`main`, `temp`, attached) | yes | yes (the databases) | yes |
| Table | yes | yes | yes | yes |
| View | yes | yes | yes | yes |
| Materialized view | no | yes | no | no |
| Function | no | yes | yes | yes |
| Procedure | no | yes | yes | yes |
| Trigger | yes | yes | yes | yes |
| Sequence | no | yes | no | yes |
| Type | no | yes | no | yes |
| Index | yes | yes | yes | yes |
| Extension | no | yes | no | no |
| Role | no | yes | yes (accounts) | yes |

The `MySQL` column also holds `MariaDB`; a `MySQL` database is a schema, so there is no database
level. See [MySQL and MariaDB](mysql.md). The `PostgreSQL` driver lists only the current database;
see [PostgreSQL](postgresql.md). The `SQL Server` driver also lists the current database only; see
[SQL Server](sqlserver.md#object-browser).

## How values display

Query results carry typed values. In the command-line output and in the results grid they display as
follows.

| Value | Display |
| ----- | ------- |
| Null | `NULL` |
| Date | `2024-02-29` |
| Time | `03:04:05`, with a fraction when it is not zero: `03:04:05.5` |
| Timestamp | `2024-02-29 03:04:05` |
| Timestamp with zone | `2024-02-29 03:04:05+01:00` |
| Decimal | the exact text, scale kept: `-12.50` |
| JSON | the server text, unchanged |
| Bytes | `0x` and hex digits |

A value that does not fit a typed value (`infinity`, `NaN`, a leap second, a time with zone) is
shown as text.

## TLS modes

Network drivers read the TLS mode of a connection. File-based databases such as `SQLite` ignore it.
You set it in the connection form and in `connections.toml` (see [Connections](../connections.md#connectiontls)).

| Mode | Meaning |
| ---- | ------- |
| `disable` | No TLS. |
| `prefer` | Use TLS if the server offers it, else connect without. This is the default. |
| `require` | Use TLS, fail if the server does not offer it. The certificate is not checked. |
| `verify-ca` | Use TLS and check that the server certificate is signed by a trusted CA (the configured CA file, if any). |
| `verify-full` | Like `verify-ca`, and also check that the certificate matches the host name. |

Optional paths set a CA certificate, a client certificate and a client key. Client key passphrases
are not supported yet.
