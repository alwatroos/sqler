# MySQL and MariaDB

The `MySQL` driver (id `mysql`) connects over TCP, with or without TLS, to `MySQL` 8.4 and
`MariaDB` 11.8. Both are tested; the driver does not depend on the server flavour.

## Targets

A target is a `mysql://` or `mariadb://` URL:

```text
mysql://user:password@host:3306/database?ssl-mode=verify_identity&ssl-ca=/path/ca.pem
```

| URL parameter | Meaning |
| ------------- | ------- |
| `ssl-mode` | `disabled`, `preferred` (the default), `required`, `verify_ca` or `verify_identity`. |
| `ssl-ca` | Path of the CA certificate file. |
| `ssl-cert`, `ssl-key` | Paths of the client certificate and its key. |

## TLS

The driver uses `rustls` and the operating system trust roots. The modes are those of
[TLS modes](index.md#tls-modes); `verify-full` is `verify_identity` in the URL.

| Mode | Encrypted | Certificate checked | Host name checked |
| ---- | --------- | ------------------- | ----------------- |
| `disable` | no | no | no |
| `prefer` | if the server offers it | no | no |
| `require` | yes | no | no |
| `verify-ca` | yes | yes | no |
| `verify-full` | yes | yes | yes |

When a connection has TLS options (a TLS mode, a CA file, a client certificate and key), they replace
the `ssl-*` parameters of the URL, which are then ignored. A connection without TLS options uses the
URL parameters.

## Passwords without TLS

The driver is built without the `sqlx` feature `mysql-rsa`, which would add the `rsa` crate
(RUSTSEC-2023-0071). The RSA public-key password exchange is therefore not available:

- Over TLS the password is sent inside the encrypted channel and every authentication plugin works.
  With the default mode `prefer`, servers that offer TLS are connected with TLS.
- Without TLS, `mysql_native_password` works, and so does `caching_sha2_password` when the server has
  the account in its cache (after one successful login over TLS). Otherwise the connection fails with
  `RSA auth backend disabled`; use TLS.

## Cancel

Cancel stops the statement on the server: the driver opens a second short connection with the same
credentials and runs `KILL QUERY` for the session's connection id. The stream then ends with a
cancelled error. An account may kill its own queries. A cancel that arrives just as a statement ends
can hit the next statement that runs on the same session.

## Types

Results are read in the binary protocol and map to typed values.

| `MySQL` type | Value |
| ------------ | ----- |
| `tinyint(1)` | Boolean |
| `tinyint`, `smallint`, `mediumint`, `int`, `bigint`, `year` | Integer |
| unsigned `bigint` above 9223372036854775807 | Decimal |
| `float`, `double` | Float |
| `decimal` | Decimal, exact |
| `char`, `varchar`, `text` kinds, `enum`, `set` | Text |
| `binary`, `varbinary`, `blob` kinds, `bit` | Bytes |
| `date`, `time` | Date, Time |
| `datetime`, `timestamp` | Timestamp |
| `json` (`MySQL`) | JSON |
| `geometry` and others | Bytes |

Some values are shown as text: a zero date such as `0000-00-00` or `2024-00-00`, and a `time` that
is negative or 24 hours or longer (`-01:00:00`, `838:59:59`). A zero date is not NULL.

Text in a `_bin` collation, such as a `varchar` with `utf8mb4_bin`, and every `MariaDB` `JSON` column
(`MariaDB` stores `JSON` as `LONGTEXT` with a `_bin` collation) come back as Bytes on both servers,
because the protocol marks them as binary. The browser still shows the declared type.

Query parameters are bound as integers, floats, booleans, text, bytes or NULL. Decimal, date, time
and timestamp parameters are sent as text, which the server converts.

## Object browsing

Eight kinds are supported: schema, table, view, function, procedure, trigger, index and role. A
`MySQL` database is a schema; there is no database level above it. Database, materialized view,
sequence, type and extension are reported as unsupported (`MariaDB` sequences are not listed).

- A missing schema means the current database of the connection.
- Triggers and indexes can be listed per table.
- Roles are the accounts of the server as `user@host`. Without the privilege to read `mysql.user`,
  the list holds only the current account.
- A table has columns with the native type, nullability and default, the primary key and the foreign
  keys. `MariaDB` quotes string defaults and reports an SQL `NULL` default as the text `NULL`; `MySQL`
  does not.
- Definition text comes from `SHOW CREATE` for schemas, tables, views, functions, procedures and
  triggers; it is empty when the account lacks the privilege. Indexes and roles have none.
