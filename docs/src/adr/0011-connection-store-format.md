# 0011 Connection store format: versioned TOML, groups, derived secret references

- Status: accepted
- Date: 2026-10-20
- Amends: [ADR-0005](0005-config-files-and-keybindings.md), section "Formats", `connections.toml`

## Context and problem statement

ADR-0005 defined `connections.toml` as a flat, read-only list. M2 needs an editable store: connections in nested groups, with colour, environment, TLS settings and an SSH tunnel profile, saved by the app and still hand-editable. Secrets (database password, SSH password, key passphrase) must never reach the file, but a profile has to point at them so the keychain module can resolve them. Files written by M1 must keep loading. `sqler-config` is layer L1 and depends on `sqler-core` only, so the keychain access itself stays in its own module.

## Decision drivers

- Human-editable, typo-safe TOML (as ADR-0005): unknown keys are errors.
- No secret in the file, by construction, not by convention.
- Renaming a connection must not orphan its keychain entries.
- Order of groups and connections is stable across save and load.
- M1 files load without a manual step.
- `sqler-core` stays free of `serde`.

## Considered options

Secret references:

1. A derived reference: the profile stores keychain flags, and the reference is computed from the stable connection ID and a secret kind.
2. An explicit `password_ref = "..."` string per profile.

File versioning:

1. A `version` key, with a file without it read as the M1 format and migrated in memory.
2. No version key; detect the format from the keys present.

## Decision outcome

Chosen: derived references, and a versioned file.

### Format v1

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
port = 22
user = "deploy"
auth = "key-file"
key_file = "/home/me/.ssh/id_ed25519"
passphrase_in_keychain = true
```

- The order of the `[[group]]` entries and of the `[[connection]]` entries is the order of the store. The order of siblings is their order in the array, filtered by parent.
- `id` is a stable key of the form `[a-z0-9-]+`, 1 to 64 characters, starting with a letter or digit (the keychain connection-id grammar of ADR-0012, enforced on load too). It is made from the name once, at creation (lowercase ASCII letters and digits kept, any other run becomes one `-`, cut to 64 characters, uniquified with `-2`, `-3` with the base shortened so the suffix still fits) and never changes, not even on rename.
- Connection names are unique in the whole store (as in ADR-0005). Group names are unique among siblings.
- Unknown keys are errors. Errors are `ConfigError::Parse` (TOML syntax and types, no echo of values) or `ConfigError::Invalid`, which names the file and the entry, for example `connection[2] "Billing": ...`.
- Load checks duplicate IDs, a `group` or `parent` that does not exist, and parent cycles.
- `version` greater than 1 is rejected: "written by a newer SQLer". A `version` of the wrong type is a parse error.

### Migration

A file without `version` is the M1 format. It loads with IDs derived from the names, every connection at the root, and no TLS or SSH settings. Load never rewrites the file. The next save writes `version = 1`.

### Secret policy

- The file holds only flags: `password_in_keychain` and, for the key-file SSH method, `passphrase_in_keychain`. There is no key that can carry a password, passphrase, secret or token.
- A profile's keychain reference is derived: the connection ID plus a kind (`db-password`, `ssh-password`, `ssh-key-passphrase`). Its account name is `<connection id>:<kind>`, for example `billing:db-password`, which is the string form of the keychain module's `SecretRef` ([ADR-0012](0012-keychain-secrets.md)). The keychain service name belongs to the keychain module.
- Because the reference follows the ID, a rename keeps the keychain entries. Deleting a connection returns the deleted profile, and the caller removes the entries listed by `secret_refs()`.
- A target that embeds a secret is still rejected (`TargetHasSecret`), and neither the error nor the `Debug` output of the spec repeats it. The database user stays in the target (`?user=`).

### Groups

Groups nest to any depth. Moving a group below itself or a descendant is rejected. A group can be deleted only when it is empty; the caller moves or deletes the children first.

### TLS

The store keeps its own TLS profile (mode `disable`, `prefer`, `require`, `verify-ca` or `verify-full`, plus optional CA certificate, client certificate and client key paths), so the file format does not depend on how drivers consume it. A client certificate and key must be given together. The profile is stored and round-tripped. `sqler-core` `ConnectionConfig` has no TLS field yet, so the conversion maps name, driver, target, colour and environment only; mapping the TLS profile follows when that field exists.

### Writes

Saves serialise first, then write a sibling temporary file, flush it to disk (`fsync`) and rename it over `connections.toml`. On Unix the directory is also flushed after the rename (best effort; a failure there does not fail the save), and on Windows the directory flush is skipped. A crash leaves either the old or the new file, never a truncated one. This amends the write described for ADR-0005, which renamed without flushing.

## Consequences

- Good: no secret can be serialised, because the model has no field for one.
- Good: renames never orphan keychain entries; references need no validation and cannot drift from the ID.
- Good: M1 files load unchanged, and the next version bump has a clear dispatch point.
- Bad: the ID is a second identifier next to the name, and a hand-edited ID that is changed orphans that connection's keychain entries.
- Bad: moving or deleting a group takes several steps in the UI, because only empty groups can be deleted.
- Neutral: the file is rewritten whole on each save, so comments and key order in a hand-edited file are not kept.

## Pros and cons of the options

- Derived references: no extra key, no drift, nothing to validate. The cost is that the ID must never change. Chosen.
- Explicit `password_ref`: flexible, but it can be duplicated between profiles or drift from the ID, and it invites storing a value there. Rejected.
- Version key: cheap, explicit, and gives future migrations a switch. Chosen.
- Detect by keys: no new key, but ambiguous as soon as a format change adds only optional keys. Rejected.
