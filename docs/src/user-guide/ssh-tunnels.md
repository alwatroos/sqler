# SSH tunnels

An SSH tunnel lets SQLer reach a database that is only visible from an SSH bastion host. SQLer
opens an SSH session to the bastion and listens on a local port; the database driver connects to
that local address, and SQLer forwards the traffic through the bastion to the database.

The tunnel is implemented in the `sqler-tunnel` crate (see
[ADR-0013](../adr/0013-ssh-tunnel-crate.md)). It is pure Rust and needs no `OpenSSH` installation,
also on Windows.

> Status: the crate is done and tested, but the connect dialog does not use it yet. Wiring it into
> the connect flow, and saving SSH settings with a connection, come in a later release.

## Local address

The tunnel listens on `127.0.0.1` only, never on a network interface. By default it picks a free
port; a fixed port can be set. Closing the tunnel (or dropping it) closes the listener, the open
forwards and the SSH session. Opening a tunnel can be cancelled, and it has a connect timeout
(15 seconds by default).

## Authentication

Secrets are passed in when the tunnel is opened. They are not read from config files and are not
written to logs or error messages.

| Method | Notes |
| ------ | ----- |
| Password | A rejected password gives "authentication failed". |
| Private key file | With an optional passphrase. An encrypted key without a passphrase gives a "passphrase required" error. |
| SSH agent | Uses `SSH_AUTH_SOCK` on Linux and macOS, or an explicit socket path. On Windows it uses the `OpenSSH` agent pipe. If no agent is reachable, the error is "SSH agent unavailable". Pageant is not supported. |

### RSA keys

RSA private keys work, without the `rsa` crate (see [ADR-0018](../adr/0018-rsa-alternatives.md)).
SQLer signs with `rsa-sha2-256` or `rsa-sha2-512`, whichever the server offers.

- Key files: `OpenSSH` format (also passphrase-encrypted), and unencrypted PKCS#1 (`BEGIN RSA PRIVATE
  KEY`) and PKCS#8 (`BEGIN PRIVATE KEY`) PEM files.
- Key sizes: 2048, 3072 and 4096 bits. A 1024-bit key, or an odd size such as 2560 bits, is
  refused with an error that names the supported sizes and Ed25519/ECDSA.
- Not supported: the old SHA-1 `ssh-rsa` signature (a server that accepts RSA keys only with it
  gives an error; enable `rsa-sha2-256` on the server or use an Ed25519 or ECDSA key), PEM files
  encrypted the legacy way (`Proc-Type: 4,ENCRYPTED`; convert with `ssh-keygen -p -o -f <file>`),
  and an RSA-only server host key.
- Keys held by an SSH agent are signed by the agent and work too; the agent decides the key
  size and hash.

## Host key verification

SQLer checks the bastion's host key against your `known_hosts` file, by default
`~/.ssh/known_hosts` (`%USERPROFILE%\.ssh\known_hosts` on Windows). It never accepts a key
silently.

- **Unknown key** (or no `known_hosts` file): the tunnel is not opened. The error carries the key
  algorithm and its fingerprint, in the form `SHA256:...`. Compare the fingerprint with the one
  your administrator gave you (`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on the bastion).
  If it matches, trust the key; SQLer appends a line to `known_hosts` (`[host]:port` when the port
  is not 22), and the next attempt succeeds. You can also connect once with the `ssh` command line
  tool to add it.
- **Changed key**: the tunnel is not opened, and the error names the line in `known_hosts`. This
  can mean a reinstalled server or an attack. SQLer does not offer to replace the key; check with
  your administrator, then remove the old line by hand.
- **Unreadable `known_hosts`**: reported as an error with the file path.

## Limits

- Not available from the connect dialog yet.
- A TLS connection through the tunnel must verify the database's real host name, not `127.0.0.1`.
  This is handled when the tunnel is wired into the connect flow.
- If the SSH session drops while the tunnel is open, new forwards fail; the tunnel does not yet
  report the dead session.
