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
| `version` | no | `1` or `2`. SQLer writes `2` only when a connection has AI settings. A file without it is the old format, see [Older files](#older-files). Any other number, including 0, is rejected; a larger one reads "written by a newer SQLer". |
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
| `[connection.ai]` | no | AI opt-in, below; needs `version = 2` |

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

### `[connection.ai]`

| Key | Required | Rule |
| --- | -------- | ---- |
| `enabled` | no | `true` lets the AI features use this connection; default `false` |
| `send_rows` | no | `true` or `false`; whether row data may be sent to the model. If unset: `true` except on `prod` connections, `false` on `prod` |

The table needs `version = 2`; a `version = 1` file that contains it is rejected. A connection without
the table has AI off. See [AI](ai.md) for the providers.

## Secrets

Passwords and passphrases are never written to `connections.toml`. The file holds only the flags
`password_in_keychain` and `passphrase_in_keychain`. SQLer finds the secret in the system keychain
under the account `<id>:<kind>`, where the kind is `db-password`, `ssh-password` or
`ssh-key-passphrase`; for example `billing:db-password`. Because the account uses the `id` and not the
name, renaming a connection keeps its secrets. If you change an `id` by hand, SQLer no longer finds
the old entries.

## In the app

The sidebar lists your connections in their groups. Every action below is also a command in the
command palette (`connection:new`, `connection:edit`, `connection:delete`, `connection:test`,
`connection:cancel-test`, `connection:save`, `connection:cancel`, `connection:move-to-group`,
`connection-group:new`, `connection-group:rename`, `connection-group:delete`).

### Sidebar

- Groups are folders that nest; click the chevron to expand or collapse one.
- A row shows the connection colour and an environment badge. A `prod` connection also gets a red
  edge and a tinted row, so it is hard to mistake for another one.
- Select a connection to see Edit and Delete; select a group to see New subgroup, Rename and Delete.
  The header has New connection and New group buttons.
- To move a connection, drag it onto a group or onto the header (top level), or use the command
  `connection:move-to-group`. It goes to the end of the target group.
- A group can be deleted only when it is empty (no connections and no subgroups); otherwise SQLer shows an error and changes nothing.

### The connection form

New connection and Edit open the same form: name, driver, target, colour (presets or `#rrggbb`),
environment (none, dev, staging, prod), group, TLS and SSH tunnel. The fields follow
[the file format](#file-format). TLS "from target" leaves TLS to the connection string; the other
modes set `mode`, the CA certificate and an optional client certificate with its key. The SSH part
has host, port, user and the method (password, key file, agent). The **AI** block turns AI on or off for the
connection and sets whether result rows may be sent (see [AI](ai.md#the-connection-dialog)).

A name that is empty, a colour that is not `#rrggbb`, a port outside 1 to 65535, a missing key file
path, or a client certificate without its key is reported next to the field, and Save stays blocked.

### Target rule

The target is a file path (for example `SQLite`) or a URL `scheme://host:port/db?user=name`. Put the
database user in the `user` parameter. A target with a user part, `password=` or a similar parameter
is rejected, and the message points to the password field. An SSH tunnel and a database password need
a URL target.

### Supported databases

Version 1.0 connects to `SQLite`, `PostgreSQL` and `MySQL`/`MariaDB`.

**Not in 1.0:** `DuckDB`. Its code is in the repository but switched off; no release, installer or update contains it
([ADR-0070](../adr/0070-duckdb-engine-and-licence.md), amendment 2). It may return after 1.0 as a separate download.

### Passwords

The database password, the SSH password and the key passphrase have their own masked fields. They go
to the system keychain when you save, never to `connections.toml`; the file only gets the
`..._in_keychain` flags. When you edit a connection that has a stored secret, the field shows that
one is kept; type to replace it, or choose to clear it to remove the stored value on save. Saving a form without a password does not
touch the keychain. See [Secrets](secrets.md).

### Test connection

Test connects through the same path as opening a connection: it reads the passwords (the typed ones,
or the keychain), opens the SSH tunnel if configured, and connects to the database. It runs in the
background; the form shows a spinner and a Cancel button, then "Connected" or the error. The test
session and its tunnel are closed at once. A test works on an unsaved form.

### Delete

Delete asks for confirmation, names the connection and says that its keychain entries are removed.
Confirming removes the connection from `connections.toml` and its database password, SSH password and
key passphrase from the keychain. Tabs that are already open keep their old title.

### SSH host key

SQLer never accepts an SSH host key silently. If the bastion is not in `~/.ssh/known_hosts`, a
dialog shows the host, port, key type and `SHA256:` fingerprint. Compare it with the fingerprint you
got from the server administrator, then choose Trust (the key is added to `known_hosts` and the
action runs again) or Reject. A changed key is an error with the line number in `known_hosts`; fix
the file yourself.

### Limits

- `verify-full` TLS through an SSH tunnel checks the certificate against the connection's original
  host, not the tunnel's local address ([ADR-0019](../adr/0019-tls-server-name-through-tunnel.md)).
  A certificate for another name fails the connection. On Windows this is still refused with the
  message "verify-full TLS through an SSH tunnel is not supported yet; use verify-ca or connect
  without the tunnel", because the TLS library cannot separate the dial address from the name there.
- Quitting closes open sessions and tunnels, waiting at most a couple of seconds.

See [ADR-0016](../adr/0016-connect-pipeline.md) for the connect pipeline.

## Older files

A file without `version` is the format of the first release, a plain list of `[[connection]]` entries
with `name`, `driver`, `target`, `color` and `environment`. It still loads: each `id` is made from the
name, every connection is at the top level, and there are no TLS or SSH settings. SQLer does not
change the file when it loads it. The next time SQLer saves the connections, it writes `version = 1`, or `version = 2` if a connection has AI settings.

## Saving

SQLer writes the file in one step: it writes a temporary file next to it, flushes it to disk and
renames it, so a crash does not leave a truncated file. On Unix it also flushes the directory
after the rename (best effort); Windows skips that step. Saving rewrites the whole file, so comments
and the key order of a hand-edited file are not kept.

See [ADR-0011](../adr/0011-connection-store-format.md) for the design.
