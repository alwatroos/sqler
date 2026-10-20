# Databases

SQLer talks to databases through drivers. This page lists what each driver offers. The pages below
describe one driver each.

- [SQLite](sqlite.md)

## Object kinds

The object browser and the DDL view ask the driver which kinds of object exist. A kind the database
does not have is reported as unsupported, not as an error.

| Kind | `SQLite` | `PostgreSQL` | `MySQL` |
| ---- | -------- | ------------ | ------- |
| Database | no | planned | planned |
| Schema | yes (`main`, `temp`, attached) | planned | planned |
| Table | yes | planned | planned |
| View | yes | planned | planned |
| Materialized view | no | planned | no |
| Function | no | planned | planned |
| Procedure | no | planned | planned |
| Trigger | yes | planned | planned |
| Sequence | no | planned | no |
| Type | no | planned | no |
| Index | yes | planned | planned |
| Extension | no | planned | no |
| Role | no | planned | planned |

The `PostgreSQL` and `MySQL` columns show the plan, not the current state: those drivers are not
available yet.

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
Settings for it in the UI and in the connections file come in a later release.

| Mode | Meaning |
| ---- | ------- |
| `disable` | No TLS. |
| `prefer` | Use TLS if the server offers it, else connect without. This is the default. |
| `require` | Use TLS, fail if the server does not offer it. The certificate is not checked. |
| `verify-ca` | Use TLS and check that the server certificate is signed by a trusted CA (the configured CA file, if any). |
| `verify-full` | Like `verify-ca`, and also check that the certificate matches the host name. |

Optional paths set a CA certificate, a client certificate and a client key. Client key passphrases
are not supported yet.
