# sqler-tunnel

SSH session and local port forward (russh); drivers connect to a local address.

Status: API v0 (M2)

`Tunnel::open` connects to an SSH bastion, checks the host key against a `known_hosts` file,
authenticates (password, private key file with optional passphrase, or SSH agent) and binds a
loopback port. Each local connection is forwarded over a `direct-tcpip` channel to the target
host. Closing or dropping the `Tunnel` tears everything down; dropping the `open` future cancels
the open. An unknown or changed host key is a typed error (`TunnelError::UnknownHostKey`,
`TunnelError::HostKeyChanged`); `trust_host_key` records a key after the user confirmed it.
Secrets are passed in as `Secret` values and never appear in `Debug` output or error messages.
RSA user keys (2048, 3072 or 4096 bits; `rsa-sha2-256` and `rsa-sha2-512`; `OpenSSH`, PKCS#1 and
PKCS#8 files) are signed with `ring`, without the `rsa` crate (ADR-0018). SHA-1 `ssh-rsa`,
legacy-encrypted PEM files and other key sizes give a `TunnelError` that names the alternative.

Layer: L1

See [AGENTS.md](../../AGENTS.md) for the project handbook.
