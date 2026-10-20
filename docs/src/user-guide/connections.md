# Connections

Saved connections live in `connections.toml` in the [config directory](settings.md#where-the-files-are).
You can edit the file by hand. A missing file means no connections. A wrong key or value is reported
with the file name and the entry; it is never silently ignored.

## File format

```toml
version = 1

[[group]]
id = "clients"
name = "Clients"

[[group]]
id = "work"
name = "Work"
parent = "clients"

[[connection]]
id = "billing"
name = "Billing"
group = "work"
driver = "postgres"
target = "postgres://db.internal/billing?user=app"
color = "#22aa66"
environment = "prod"
password_in_keychain = true

[connection.tls]
mode = "verify-full"
ca_cert = "/etc/ssl/ca.pem"

[connection.ssh]
host = "bastion.example.com"
user = "deploy"
auth = "key-file"
key_file = "/home/me/.ssh/id_ed25519"
passphrase_in_keychain = true
```

The order of the `[[group]]` entries and of the `[[connection]]` entries is the order SQLer shows.

### Top level

| Key | Required | Rule |
| --- | -------- | ---- |
| `version` | no | `1`. A file without it is the old format, see [Older files](#older-files). Any other number, including 0, is rejected. |
| `[[group]]` | no | a folder, see below |
| `[[connection]]` | no | a saved connection, see below |

### `[[group]]`

| Key | Required | Rule |
| --- | -------- | ---- |
| `id` | yes | 1 to 64 lowercase letters, digits and `-`, starting with a letter or digit; unique among groups; never changes |
| `name` | yes | not empty; unique among the groups with the same parent |
| `parent` | no | the `id` of another group; leave it out for a top-level group |

Groups can nest as deep as you like. A group cannot be its own ancestor.

### `[[connection]]`

| Key | Required | Rule |
| --- | -------- | ---- |
| `id` | yes | 1 to 64 lowercase letters, digits and `-`, starting with a letter or digit; unique among connections; never changes |
| `name` | yes | not empty; unique among all connections |
| `group` | no | the `id` of a group; leave it out for a top-level connection |
| `driver` | yes | a driver ID such as `sqlite` or `postgres` |
| `target` | yes | file path or connection string, without a password |
| `color` | no | `#rrggbb` |
| `environment` | no | `dev`, `staging` or `prod` |
| `password_in_keychain` | no | `true` when the database password is in the system keychain; default `false` |
| `[connection.tls]` | no | TLS settings, below |
| `[connection.ssh]` | no | SSH tunnel, below |

The `id` is made from the name when SQLer creates the connection and does not change when you rename
it. Keep it when you edit the file by hand.

A target that contains a secret is rejected, and the error does not repeat the target. This covers any
user part in a `scheme://` URL and any `password`, `pwd`, `secret` or `token` parameter. Put the
database user in the target as a parameter, for example `?user=app`.

### `[connection.tls]`

| Key | Required | Rule |
| --- | -------- | ---- |
| `mode` | yes | `disable`, `prefer`, `require`, `verify-ca` or `verify-full` |
| `ca_cert` | no | path to the CA certificate |
| `client_cert` | no | path to the client certificate; needs `client_key` |
| `client_key` | no | path to the client key; needs `client_cert` |

### `[connection.ssh]`

| Key | Required | Rule |
| --- | -------- | ---- |
| `host` | yes | not empty |
| `port` | no | 1 to 65535; default `22` |
| `user` | yes | not empty |
| `auth` | yes | `password`, `key-file` or `agent` |
| `key_file` | with `key-file` | path to the private key; not allowed with the other methods |
| `passphrase_in_keychain` | no | `true` when the key passphrase is in the system keychain; only with `key-file`; default `false` |

## Secrets

Passwords and passphrases are never written to `connections.toml`. The file holds only the flags
`password_in_keychain` and `passphrase_in_keychain`. SQLer finds the secret in the system keychain
under the account `<id>:<kind>`, where the kind is `db-password`, `ssh-password` or
`ssh-key-passphrase`; for example `billing:db-password`. Because the account uses the `id` and not the
name, renaming a connection keeps its secrets. If you change an `id` by hand, SQLer no longer finds
the old entries.

## Older files

A file without `version` is the format of the first release, a plain list of `[[connection]]` entries
with `name`, `driver`, `target`, `color` and `environment`. It still loads: each `id` is made from the
name, every connection is at the top level, and there are no TLS or SSH settings. SQLer does not
change the file when it loads it. The next time SQLer saves the connections, it writes `version = 1`.

## Saving

SQLer writes the file in one step: it writes a temporary file next to it, flushes it to disk and
renames it, so a crash does not leave a truncated file. On Unix it also flushes the directory
after the rename (best effort); Windows skips that step. Saving rewrites the whole file, so comments
and the key order of a hand-edited file are not kept.

See [ADR-0011](../adr/0011-connection-store-format.md) for the design.
