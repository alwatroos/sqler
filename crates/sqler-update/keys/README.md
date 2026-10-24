# Trusted update keys

This directory holds the public Ed25519 keys the client trusts. It holds the production key
`9735ae5e612eaa24.pub` (ADR-0066 note, R17). With no key listed in `src/keys.rs`, `sqler-update` reports
`NotConfigured`.

- One file per key, named `<key_id>.pub`, where `<key_id>` is the 16 lowercase hex characters of the first 8 bytes
  of SHA-256 over the raw public key.
- Content: one line, standard base64 of the 32 raw public key bytes. Line endings (LF or CRLF) are ignored.
- A file is trusted only when `src/keys.rs` lists it with `include_str!` (and in `EMBEDDED_FILE_IDS`; a test
  checks that every file here is listed and that each file name equals its key id).
- Rotation: add the new key beside the old one in a release, sign with the new key from the next release, remove
  the old key one release later.
- Never commit a private key or a seed here. Test keys live only in `tests/support/mod.rs`, marked TEST ONLY.
