# 0013 SSH tunnel crate: russh, strict known_hosts, loopback-only forward

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

SQLer must reach a database behind an SSH bastion (SQLER_AGENT_PROMPT §3.1). A new crate, `sqler-tunnel` (layer L1, no workspace dependencies), opens an SSH session and a local port forward to the database host, so any network driver connects to a local address without knowing about SSH. The crate adds a dependency and a public API, so AGENTS.md §6 and §8 require an ADR first.

## Decision drivers

- Pure Rust, no `OpenSSH` binary, works on Linux, macOS and Windows (§2.3).
- A licence that `cargo deny` accepts without a global allow.
- No silent trust of unknown or changed host keys.
- Every long-running operation is cancellable (§4).
- Testable on all three CI systems without Docker.
- No secrets in logs, no `unsafe`.

## Considered options

1. `russh`: pure-Rust SSH client and server, `Apache-2.0`, tokio-based.
2. `ssh2`: binds libssh2 (C, and OpenSSL on Linux).
3. `openssh` crate or a system `ssh` binary: needs an installed `OpenSSH`; ControlMaster does not work on Windows; cannot be tested in-process.
4. `thrussh`: the abandoned predecessor of `russh`.

## Decision outcome

Chosen: option 1, `russh` 0.64.1 (verified live, see [Dependencies](../dependencies.md)).

### Crypto backend and RSA

`russh` is declared with `default-features = false` and the feature `ring`. The default `aws-lc-rs` backend is rejected: it needs CMake and NASM on Windows, and `aws-lc-sys` carries licence terms that are not on the allow-list. `ring` is `Apache-2.0 AND ISC` and builds with the C compiler that MSVC provides. `cargo tree -i openssl-sys` and `-i aws-lc-sys` match nothing. `cargo deny check` passes with no licence exception.

The `rsa` feature is deliberately off. It pulls in the `rsa` crate, which carries RUSTSEC-2023-0071 (Marvin timing attack, no patched version), and `deny.toml` has no ignore for it. Consequences:

- RSA private key files were unsupported at this point (`TunnelError::UnsupportedKeyType`); ed25519 and ECDSA keys work. **Superseded by [ADR-0018](0018-rsa-alternatives.md):** RSA user keys now work through a `ring` signer, without the `rsa` crate.
- A server that offers only an RSA host key fails the handshake with a generic `Ssh` error. This still holds (ADR-0018).

The `rsa` feature stays off for good; ADR-0018 adds RSA user keys without it.

### Verification policy

- Host keys are checked against a `known_hosts` file that the caller passes in (the default is `~/.ssh/known_hosts`, see `default_known_hosts_path`).
- An unknown key, or a missing file, gives `TunnelError::UnknownHostKey` with the algorithm and a `SHA256:` fingerprint. A changed key gives `HostKeyChanged` with the line number. There is no silent accept and no "accept new" mode.
- Trust is explicit: the UI shows the fingerprint, and on user consent calls `trust_host_key`, which appends a line (`[host]:port` when the port is not 22) and creates the file and its directory. SQLer's own code writes the line instead of calling the `russh` learn function, because that function writes a blank first line into an empty file and shifts reported line numbers.
- The local listener binds `127.0.0.1` only, never `0.0.0.0`.
- Secrets (`Secret`: password, key passphrase) are passed in by the caller, never read from config. `Secret` prints as `Secret(***)`, and error messages never contain a secret or key bytes. `russh` logs through the `log` crate; no logger is installed and `russh` does not log credentials.
- Authentication: password, private key file with optional passphrase, and the SSH agent (Unix socket; on Windows the `OpenSSH` agent pipe). Pageant is out of scope.

### Cancel and teardown model

`Tunnel::open` is one future wrapped in the connect timeout; cancelling means dropping it. `russh` spawns the session task during the handshake, after the banner exchange. Dropping the open future drops its handle, which ends the session, but not while key exchange is running. The client config therefore sets an inactivity timeout of 90 s (three times the keepalive; keepalive replies reset it), so a server that sends its banner and then stalls cannot keep the task and the socket alive after a cancelled or timed-out open. A server that sends no banner is read inline and its socket closes at once. After success, an accept task owns the session and a `JoinSet` of per-connection copy tasks. `close()` signals shutdown, disconnects and joins; `Drop` signals shutdown and aborts the task without blocking, and dropping the `JoinSet` aborts the children. Keepalive is fixed at 30 s.

### Test strategy

`russh` ships a server, so the integration tests run against an in-process server on loopback on all three OSes, with no Docker and no CI change: password and key authentication, an echo round trip, host key unknown, trusted and changed, teardown on close and drop, cancellation and timeout, and a check that no error output contains a secret. A test against russh's in-process agent server on a temporary Unix socket runs on Unix. One `#[ignore]` test (`tests/real_bastion.rs`) is for the maintainer and uses a real bastion.

### Consequences

- Positive: no external binary, one dependency tree, tests that run everywhere.
- Negative: RSA-only host keys unsupported (RSA user keys: ADR-0018); `ring` needs a C compiler; the Windows code paths need CI confirmation.
- Not covered here: wiring into the connect flow (T9), SSH profile storage (T4) and secret storage (T5). TLS through the tunnel must verify the original host name, not `127.0.0.1` (T9).
