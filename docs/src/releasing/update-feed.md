# Update feed

The update check (ADR-0066) reads one signed file, `sqler-update.json`, from the newest GitHub Release. This page
records the signing key. The release automation extends it with the manifest job.

## Signing key

- Algorithm: Ed25519.
- Key id: `efe45a1253fd9347`.
- Public key file: `crates/sqler-update/keys/efe45a1253fd9347.pub`.
- Fingerprint (SHA-256 of the raw 32-byte public key): `efe45a1253fd934792dea0989d12ce98ed74acea6acb8832fa00362341fc394b`.
- Created: 2026-10-23.
- Private key: the environment secret `SQLER_UPDATE_SIGNING_KEY` of the GitHub environment `release`. It holds one
  line, the standard base64 of the 32-byte Ed25519 seed. It is not in the repository and cannot be read back from
  GitHub.

To check a build against this page, compute the SHA-256 of the base64-decoded content of the key file and compare
it with the fingerprint above; its first 8 bytes are the key id.

Rotation is described in ADR-0066 (add the new key beside the old one, sign with it from the next release, remove the
old one one release later).
