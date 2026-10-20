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

Over TLS the password is sent inside the encrypted channel and every authentication plugin works.
With the default mode `prefer`, servers that offer TLS are connected with TLS, but `prefer` does
not protect the password against an active attacker: a man in the middle can remove the server's
TLS offer and the connection then continues in plaintext. Only `require`, `verify-ca` and
`verify-full` protect the password (`verify-ca` and `verify-full` also check the server).

Without TLS, `mysql_native_password` works. `caching_sha2_password` (when the server has not cached
the account yet) and `sha256_password` use the RSA public-key exchange: the server sends its public
key and the driver encrypts the password with it (RSA-OAEP). This works through a patched copy of
`sqlx-mysql` that uses `aws-lc-rs`, so the vulnerable `rsa` crate is not part of SQLer
(RUSTSEC-2023-0071).

SQLer runs this exchange over a plaintext link **only when you chose plaintext explicitly**
(`ssl-mode=disabled`, or TLS mode `Disable` in the connection settings). With `prefer` (the
default), or any other mode, a connection that ends up without TLS is refused with
"plaintext RSA password exchange needs an explicit ssl-mode=disabled; use TLS", so an attacker who
strips the TLS offer cannot make SQLer send the password this way.

Even with `ssl-mode=disabled` the exchange protects the password only against a passive observer.
The server key arrives in the same stream, so an attacker who can modify the traffic can
substitute their own key and read the password. Use TLS (`require`, or `verify-ca` /
`verify-full` to also check the server) wherever the network is not trusted.

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
