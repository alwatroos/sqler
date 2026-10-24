# Update feed

The update check (ADR-0066) reads one signed file, `sqler-update.json`, from the newest GitHub Release. This page
records the signing key. The release automation extends it with the manifest job.

## Signing key

- Algorithm: Ed25519.
- Key id: `9735ae5e612eaa24`.
- Public key file: `crates/sqler-update/keys/9735ae5e612eaa24.pub`.
- Fingerprint (SHA-256 of the raw 32-byte public key): `9735ae5e612eaa24353b605a6569138bed289cb95f9ce0b4daf237f98767108c`.
- Created: 2026-10-24 (rotated before 1.0; the 2026-10-23 key never signed anything and is removed).
- Private key: the environment secret `SQLER_UPDATE_SIGNING_KEY` of the GitHub environment `release`. It holds one
  line, the standard base64 of the 32-byte Ed25519 seed. It is not in the repository and cannot be read back from
  GitHub.

Backup: the owner's login keychain holds the private key under the service `sqler-update-signing-key` (account
`sqler`). To restore the secret without printing it, pipe `security find-generic-password -s sqler-update-signing-key
-a sqler -w` into `gh secret set SQLER_UPDATE_SIGNING_KEY --env release`.

To check a build against this page, compute the SHA-256 of the base64-decoded content of the key file and compare
it with the fingerprint above; its first 8 bytes are the key id.

Rotation is described in ADR-0066 (add the new key beside the old one, sign with it from the next release, remove the
old one one release later).
