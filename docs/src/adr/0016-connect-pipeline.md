# 0016 Connect pipeline: `Connector` trait in `sqler-ui`, pipeline in `sqler-app`

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

Connecting is no longer "map a target and call the registry". A saved connection now has a keychain password ([ADR-0012](0012-keychain-secrets.md)), an optional SSH tunnel ([ADR-0013](0013-ssh-tunnel-crate.md)), TLS settings ([ADR-0008](0008-object-model-lazy-introspection-value-types-tls.md)) and a profile in `connections.toml` ([ADR-0011](0011-connection-store-format.md)). The connection manager UI must create, test and open such connections. Two questions need a decision: where does the pipeline "profile, secret, tunnel, driver" live, and how does the UI reach it without importing a driver or `sqler-tunnel` (AGENTS.md §2, [ADR-0007](0007-desktop-binary-and-ui-tokio-bridge.md))?

## Decision drivers

- `sqler-ui` is layer L6 and must not depend on driver crates or on `sqler-tunnel`; `cargo tree --workspace -i sqler-ui` shows only `sqler-app`.
- The UI thread never blocks and never calls `tokio::spawn` ([ADR-0007](0007-desktop-binary-and-ui-tokio-bridge.md)).
- The pipeline must be testable headless, without a display, a keychain or a real SSH server.
- Secrets never reach `connections.toml`, logs or error text.
- Drivers from other work items plug in through the registry with no change in the UI.

## Considered options

1. Pipeline inside `sqler-ui`: the UI crate would depend on `sqler-tunnel` and own I/O policy.
2. Pipeline in `sqler-app`, reached from the UI through a `Connector` trait defined in `sqler-ui`.
3. A new crate for the pipeline: one more crate for about 400 lines that only `sqler-app` uses.

## Decision outcome

Chosen: option 2.

### The seam

`sqler_ui::connections` defines `Connector`: `driver_ids()`, `connect(ConnectRequest)` and `trust_host_key(HostKeyPrompt)`, both returning boxed futures. A `ConnectRequest` carries the `ConnectionSpec` and two `SecretSource` values (`None`, `Keychain(SecretRef)` or `Value(Secret)`). A typed `ConnectFailure` (`thiserror`, `#[non_exhaustive]`) carries the result; it never holds a secret or the raw target. `SessionBridge` spawns the futures on the tokio runtime and the GPUI task awaits a `oneshot`. Dropping the pending future cancels the connect. The same bridge writes keychain changes through `spawn_blocking`.

`sqler-app` implements `Connector` as `Pipeline` in `crates/sqler-app/src/connect/`. `Startup` carries `connector`, `secrets` (the same `SecretStore` the pipeline reads), `connections`, `config_dir`, the runtime handle, settings and keybindings; it no longer carries the registry.

### Pipeline order

1. Resolve both secrets in one `spawn_blocking` call. A `Keychain` source with no stored entry is `MissingSecret`.
2. Build the `ConnectionConfig` from the spec: name, driver, target, colour, environment and the TLS options (`TlsProfile::to_core`).
3. Without SSH, inject the database password into the target and call `DriverRegistry::connect`.
4. With SSH: parse the target as a URL (`scheme://host[:port]/...`), refuse verify-full (see below), open the tunnel to the real host and port with the SSH secret as password or key passphrase, rewrite the target to the tunnel's loopback address, inject the password, call the registry. If the driver fails, close the tunnel first.
5. Wrap the session in `TunnelledSession`. `close` closes the driver session and then the tunnel (a driver error wins over a tunnel error). `Drop` with the tunnel still open spawns its close on the runtime handle, because a `Tunnel` must not be dropped outside tokio.

The tunnel is behind a small `TunnelOpener` trait so the pipeline is tested with a fake tunnel; `SshTunnels` is the production implementation over `sqler_tunnel::Tunnel`.

### Where the database user lives

The stored target never holds user info ([ADR-0011](0011-connection-store-format.md)): the user is the `user` query parameter, for example `postgres://db.internal/billing?user=app`. The form rejects user info and secret-looking parameters in the target and points to the password field. When the password is injected, the user is taken from the `user` parameter; a target without a user connects only when no password is set; with a password it is refused. An SSH tunnel and a password need a URL target; a file path such as a `SQLite` file does not.

### Host-key trust

An unknown host key is `ConnectFailure::UnknownHostKey(HostKeyPrompt)`. The workspace opens a dialog that shows host, port, algorithm and the `SHA256:` fingerprint. "Trust" calls `Connector::trust_host_key`, which runs `sqler_tunnel::trust_host_key` in `spawn_blocking`, and then repeats the original action (open or test). "Reject" closes the dialog. A changed key (`HostKeyChanged`) is a plain error with the line number; the user must edit `known_hosts` by hand. There is no silent accept ([ADR-0013](0013-ssh-tunnel-crate.md)).

### Known limitation: verify-full through a tunnel

*Superseded by [ADR-0019](0019-tls-server-name-through-tunnel.md): on Unix the refusal is lifted; on Windows it stays.*

With a tunnel the driver connects to `127.0.0.1:<port>`. A TLS check in `verify-full` mode would verify the certificate against `127.0.0.1`, not against the database host, and fail for every real certificate. The correct fix is a server-name override: a field on core `TlsConfig` that a driver passes to its TLS library. That is a change in `sqler-core` and in each driver. The pipeline therefore fails closed: an explicit `TlsMode::VerifyFull`, or (when the profile has no explicit TLS mode) a target with `sslmode` or `ssl-mode` set to `verify-full` or `verify_identity` (any driver, case-insensitive, also percent-encoded), together with an SSH tunnel is refused with `TlsHostnameThroughTunnel` before anything is opened. An explicit TLS mode overrides the URL parameter, so only an explicit `verify-full` or a URL verify-full without an explicit mode is refused. `verify-ca` and the weaker modes work through a tunnel. Follow-up: add the server-name field to `TlsConfig` and support it in the drivers, then lift the refusal.

### Close on quit

Open sessions are closed on quit (before this ADR they were dropped). The `app:quit` command returns `Effect::CloseSessions`; the bridge closes all sessions concurrently under a 2 s bound, then the app quits. The `cx.on_app_quit` hook covers quitting by the window system, but the platform waits only about 200 ms there, so the hook uses a 150 ms bound. Sessions that do not close in time are dropped, and a dropped `TunnelledSession` closes its tunnel on the runtime in the background.

### Consequences

- Positive: `sqler-ui` stays free of drivers and `sqler-tunnel`; the state machine, form and effects are unit-tested headless with `FakeConnector`; the pipeline is tested with a fake tunnel opener and `MemoryStore`.
- Positive: a driver registered in `sqler-app` works with the pipeline with no UI change.
- Negative: `verify-full` is unavailable on a tunnelled connection until the follow-up lands.
- Negative: quitting with a slow server can still drop a session after the bound.
- Not checked by CI (maintainer, on a display): masked password inputs, focus when a dialog opens, Escape in dialogs, the drag preview, the keychain prompt, the SSH host-key prompt and a connection through a real bastion.
