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
RSA keys are unsupported until the owner decides on RUSTSEC-2023-0071 (the `rsa` crate): an RSA
key file gives `TunnelError::UnsupportedKeyType`; use an ed25519 or ECDSA key.

Layer: L1

See [AGENTS.md](../../AGENTS.md) for the project handbook.
