# 0019 TLS server name through a tunnel: `TlsConfig::server_name` and a local Unix-socket bridge

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

Through an SSH tunnel a driver dials `127.0.0.1:<port>`, but the server certificate names the original database host. [ADR-0016](0016-connect-pipeline.md) therefore refused `verify-full` together with a tunnel and listed a server-name field in `TlsConfig` as the follow-up. This ADR adds the field and decides how the drivers apply it, without weakening verification in any mode.

## Decision drivers

- `verify-full` must keep checking the chain and the host name; there is no downgrade and no custom verifier that skips a check.
- The drivers stay on `sqlx` 0.9.0 with `rustls` ([ADR-0009](0009-postgres-driver-tls-db-tests.md), [ADR-0010](0010-mysql-driver.md)); no fork and no git source in `cargo deny`.
- `sqler-driver-api` stays free of a runtime ([ADR-0004](0004-driver-api.md)).
- No persisted setting: the name is derived at connect time.

## Evidence (sqlx 0.9.0 source)

- `sqlx-postgres/src/connection/tls.rs:57` and `sqlx-mysql/src/connection/tls.rs:62` pass `hostname: &options.host` to `sqlx-core/src/net/tls/tls_rustls.rs`, which builds `ServerName::try_from(hostname)`. That one name is used for certificate verification and for SNI.
- The same `options.host` is the TCP dial address (`connect_tcp(&options.host, ..)`).
- sqlx has no server-name option, no hook for a custom verifier and no way to hand in a connected stream. `hostaddr` only overwrites `host`.
- The one place where dial target and TLS name separate is the Unix-socket path: when `socket` is set, both drivers dial it (`connect_uds`) and still run TLS against `options.host`.

## Considered options

1. A `[patch]` or fork of `sqlx` with a server-name option. Rejected: a fork to maintain and a git source in `cargo deny`. An upstream issue is the better route.
2. A TLS-terminating local proxy. Rejected: it would re-implement the in-protocol upgrade (`SSLRequest`, the `MySQL` handshake) and client-certificate authentication outside `sqlx`.
3. A resolver override. Rejected: tokio resolves with `getaddrinfo`, which has no per-connection hook.
4. A per-session local Unix-socket bridge in each driver. Chosen.

## Decision outcome

### Core

`TlsConfig` gains `server_name: Option<String>` and `with_server_name(..)`: the name for certificate host-name verification and SNI when it differs from the dial host. It is not a secret and appears in `Debug`. Drivers use it only when the effective mode is `verify-full` (`PgSslMode::VerifyFull`, `MySqlSslMode::VerifyIdentity`), the target is TCP (no `socket`) and the name differs from the dial host (ASCII case-insensitive, brackets trimmed). Otherwise it is ignored.

### Drivers: the bridge

When the rule above applies, on Unix the driver starts a per-session bridge: a `UnixListener` in a fresh `0700` directory (`sqler-tls-<16 hex random>`) under `$XDG_RUNTIME_DIR` when that is usable, else under the temp dir, that forwards each accepted stream to the dial `host:port`, which is the tunnel's `127.0.0.1:<port>`. The driver then sets `host(server_name)` and `socket(..)` on the `sqlx` options. `sqlx` dials the socket and runs its own `WebPki` verification of the chain and of the name against `server_name`. Nothing is skipped. The bridge lives as long as the session and any cancel handle (the cancel path connects with the same options); dropping it aborts the accept task and its forwards and removes the socket and the directory. A bind or directory error is a `DriverError::Connection` and never falls back to weaker TLS. Each driver has its own private copy of the bridge so `sqler-driver-api` needs no runtime.

### Pipeline

`sqler-app` sets `server_name` to the profile's original host when a tunnel is used. A profile TLS config keeps its mode and files and gains the name. A target that asks for `verify-full` only in the URL (`sslmode=verify-full` or `ssl-mode=verify_identity`) is lifted into an explicit `verify-full` config, with the certificate files from the query (`sslrootcert`, `ssl-root-cert`, `ssl-ca`, `sslca`, `sslcert`, `ssl-cert`, `sslkey`, `ssl-key`), because the URL has nowhere to hold the name. The refusal `TlsHostnameThroughTunnel` is no longer raised on Unix. A wrong certificate name fails closed as `ConnectFailure::Driver(DriverError::Connection(..))` and the tunnel is closed.

### Windows

`sqlx` has `connect_uds` only on Unix. On Windows the driver returns `DriverError::Unsupported`, and the pipeline keeps the refusal there (the old message), checked before anything is opened. This is a documented limit, not a hidden one. Follow-up: an upstream `sqlx` issue asking for a server-name option, after which the bridge and this limit can go.

### Consequences

- Positive: `verify-full` works through a tunnel on macOS and Linux with `sqlx`'s own verifier; no change to the stored profile.
- Negative: two private copies of about 100 lines of bridge code, and a short-lived socket in the temp dir (owner-only directory, reaching only the loopback port the tunnel already exposes). The macOS socket path limit (104 bytes) is enough for the chosen names.
- Negative: Windows still cannot use `verify-full` through a tunnel.
- Not checked by CI (maintainer): `verify-full` through a real bastion on macOS and Linux.

This supersedes the "Known limitation: verify-full through a tunnel" section of [ADR-0016](0016-connect-pipeline.md).
