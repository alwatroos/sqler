# 0012 Keychain secrets: `keyring` 4.x, reference scheme, failure policy

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

Connection profiles (T4) need database passwords, SSH passwords and SSH key passphrases. These must live in the operating system's keychain and never in `connections.toml` (ADR-0005). `sqler-config` (L1) therefore needs a small API to set, read and delete a secret by a stable reference, with a real keychain backend on macOS, Windows and Linux and an in-memory backend for tests, the CLI and headless use. It adds a dependency (`keyring`) and a public API, so an ADR comes first.

## Decision drivers

- Secrets only in the OS keychain; no fallback to a file or to memory.
- A missing or locked keychain is a typed error, never a panic.
- No secret in `Debug`, errors or logs.
- Linux CI must build without new system packages.
- CI runs no real keychain.
- Only licenses on the `deny.toml` allow-list.

## Considered options

1. `keyring` 4.2.0 with its `v1` feature (native stores per target).
2. Per-OS crates used directly (`keyring-core` plus the three store crates).
3. `keyring` with the `dbus-secret-service` store (links `libdbus`).
4. `keyring` with `linux-keyutils`.

## Decision outcome

Chosen: option 1. `keyring` 4.2.0 (verified live on 2026-10-20, `max_stable_version`, `MIT OR Apache-2.0`, `rust-version` 1.88). The 4.x crate no longer has per-backend features of its own; its default feature `v1` selects the native store for the target. The workspace declares it with `default-features = false`, and `sqler-config` enables `v1` for macOS, Windows, Linux and FreeBSD only:

| OS | Store (crate) | Notes |
| -- | ------------- | ----- |
| macOS | `apple-native-keyring-store` 1.0.2 (`keychain`) | Keychain Services |
| Windows | `windows-native-keyring-store` 1.1.0 | Credential Manager |
| Linux, FreeBSD | `zbus-secret-service-keyring-store` 1.0.1 (`crypto-rust`) | Secret Service over pure-Rust `zbus` with RustCrypto and the `async-io` runtime |

`cargo tree -e features -i keyring` per target confirms each target turns on its own store and nothing else. `cargo deny` passed with no change: no new license and no new advisory. Option 2 gives the same result with three more root dependencies and our own default-store setup, which `keyring` `v1` already does once and checks. Option 3 needs `libdbus-1-dev` in CI and on users' machines. Option 4 (keyutils) keeps secrets in the kernel keyring, which does not persist across reboots. OpenSSL crypto, a `tokio` runtime feature (it can panic inside T9's runtime), `vendored` and any mock store are not used.

### Blocking calls

`SecretStore` is synchronous because the keychain APIs block. On Linux the Secret Service client runs its own `async-io` executor thread, and a blocking call on a `tokio` worker stalls that worker. Async callers (T9) must run the calls with `spawn_blocking`.

### Reference scheme

A secret is named by a `SecretRef`: `<connection-id>:<kind>`, with kind one of `db-password`, `ssh-password`, `ssh-key-passphrase`. The keychain entry has service `SQLer` (`KEYCHAIN_SERVICE`) and account name equal to the reference string. The connection id is 1 to 64 characters of `[A-Za-z0-9._-]` and starts with a letter or digit; anything else is `SecretError::InvalidRef`. The id is opaque to `sqler-config` secrets: T4 creates it and keeps it unchanged when a connection is renamed. T4 stores `SecretRef::to_string()` in TOML and reads it back with `str::parse` (the exact inverse; the string is split on the last `:`). `SecretRef` has no serde. Changing the service name or the reference format later orphans stored secrets, so both are fixed by this ADR.

### Failure policy

- Every failure is a typed `SecretError` that names the reference and never a value: `Unavailable` (no keychain, locked, access denied, no Secret Service on the bus, unsupported OS), `Backend`, `Rejected` (length limit or invalid parameter), `NotUtf8` (the stored bytes are dropped), `Ambiguous`, and `InvalidRef`. The reason text is the backend error's `Display`, never its `Debug`. `keyring`'s `NoDefaultStore` (the store could not be created) maps to `Unavailable`; any other or future `keyring` error maps to `Backend`.
- No fallback: when the keychain cannot be used, the operation fails. Nothing is written to a file and nothing is silently kept in memory. `MemoryStore` is chosen explicitly by the caller (tests, the CLI, headless runs).
- `Secret` has no `Display`, no serde and no `PartialEq`; its `Debug` prints `Secret(***)`. `MemoryStore`'s `Debug` prints only the entry count. The crate logs nothing.
- On an OS without a supported keychain, `KeychainStore` returns `Unavailable` without calling `keyring`.
- Empty secrets are allowed; if a backend refuses one the result is `Rejected`.
- Secrets are plain `String`s (`keyring` returns plain `String`s, so zeroizing only our copy would add a dependency and protect little).

### Tests

CI tests `MemoryStore`, the reference grammar, redaction and the pure error mapping. The real round trip is `#[ignore]`: the maintainer runs `cargo test -p sqler-config -- --ignored keychain_round_trip` on a desktop session of each OS and checks in the keychain UI that the entry appears and is removed.

## Consequences

- Positive: one small trait, swappable in tests; no secret in config files; no new apt packages; no license exception.
- Negative: on Linux without a running Secret Service (headless, minimal desktops) every keychain call fails with `Unavailable`; users get an error, not a plain-text fallback. The Secret Service connection is set up once per process, so if it is missing at first use, SQLer must be restarted after starting it.
- Negative: `keyring` 4.x pulls the `zbus` tree on Linux; its existing advisory ignores (`zbus_names`, `zvariant`, `zvariant_derive`, ADR-0007) already cover it.
- Follow-up: T4 holds the references in profiles; T9 owns password prompts and uses `spawn_blocking`.
