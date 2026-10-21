# sqler-config

Settings, keybindings, connection store and keychain access.

Status: M2: settings, keybindings, connection store, OS keychain secrets

Layer: L1

Connection store: `ConnectionStore` holds nested groups and connection profiles
(colour, environment, TLS, SSH tunnel) and is saved to `connections.toml` as a
versioned file (`version = 1`, or `2` when a connection has AI settings) with atomic writes. A profile keeps only keychain
references derived from its stable id, never a password or passphrase. Files
written by M1 load unchanged and are migrated on the next save.

See [AGENTS.md](../../AGENTS.md) for the project handbook.
