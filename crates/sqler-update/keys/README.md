# Trusted update keys

This directory holds the public Ed25519 keys the client trusts. It holds no key yet: the production key arrives
with R17 through a normal pull request, and until then `sqler-update` reports `NotConfigured`.

- One file per key, named `<key_id>.pub`, where `<key_id>` is the 16 lowercase hex characters of the first 8 bytes
  of SHA-256 over the raw public key.
- Content: one line, standard base64 of the 32 raw public key bytes. Line endings (LF or CRLF) are ignored.
- A file is trusted only when `src/keys.rs` lists it with `include_str!`.
- Rotation: add the new key beside the old one in a release, sign with the new key from the next release, remove
  the old key one release later.
- Never commit a private key or a seed here. Test keys live only in `tests/support/mod.rs`, marked TEST ONLY.
