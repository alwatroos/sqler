# Vendored sqlx-mysql 0.9.0 (SQLer patch)

This is a copy of [`sqlx-mysql` 0.9.0](https://crates.io/crates/sqlx-mysql/0.9.0)
(upstream repository `launchbadge/sqlx`, commit `003b698e99e024f3621b8043a2426fde5b741171`,
path `sqlx-mysql`). It is wired in through `[patch.crates-io]` in the root `Cargo.toml` and is
**not** a workspace member, so `cargo fmt`, `cargo clippy` and `.github/scripts/check-spdx.sh`
do not touch it. Upstream keeps its own licences: `LICENSE-APACHE` and `LICENSE-MIT`.

## Why

The MySQL RSA password exchange (`caching_sha2_password` cold cache or `sha256_password` over a
connection without TLS) needs RSA-OAEP. Upstream offers it only through the `rsa` crate, which is
banned (RUSTSEC-2023-0071, no fix; see `deny.toml` and ADR-0018). This copy adds an `aws-lc-rs`
backend instead.

## The diff against upstream 0.9.0 (56 changed lines, 4 files)

- `Cargo.toml` (19 lines): new feature `aws-lc-rs` (enables `dep:aws-lc-rs`, `dep:pem-rfc7468`), the
  two optional dependencies `aws-lc-rs` (`default-features = false`, features `alloc`,
  `aws-lc-sys`) and `pem-rfc7468` (`alloc`).
- `src/connection/auth.rs` (31 added, 1 removed): a `rsa_backend` module under
  `cfg(all(feature = "aws-lc-rs", not(feature = "rsa")))` that decodes the server's SPKI PEM key and
  encrypts with RSA-OAEP, SHA-1 and MGF1-SHA-1 (what MySQL requires). The existing "disabled"
  stub is now `cfg(not(any(feature = "rsa", feature = "aws-lc-rs")))`. `encrypt_rsa` refuses to
  start the plaintext exchange unless the stream allows it (below).
- `src/connection/stream.rs` (4 lines) and `src/connection/tls.rs` (1 line): the new field
  `MySqlStream::plaintext_rsa_allowed`, true only when the connect options say
  `MySqlSslMode::Disabled` (set in `with_socket`, copied in `boxed_socket`, false on the TLS stream).

Downgrade protection: the server's RSA public key arrives in-band over a plaintext link, so a man
in the middle can substitute its own key and read the password. An attacker can also strip the
server's TLS offer, which under the default `Preferred` mode would silently continue in plaintext.
So `encrypt_rsa` returns `Error::Configuration("plaintext RSA password exchange needs an explicit
ssl-mode=disabled; use TLS")` unless the user chose `Disabled`. Over TLS nothing changes (the
password is sent directly). `Required` and `VerifyCa`/`VerifyIdentity` never reach this code
without TLS.

Nothing else differs.

`Cargo.toml.orig` is the unmodified upstream manifest; `Cargo.lock` was not kept.

## Maintenance

- The workspace pins `sqlx-mysql = "=0.9.0"`. On every sqlx bump: download the new `sqlx-mysql`
  from crates.io, re-apply the hunks above, update the pin and this file, and run the MySQL
  `rsa_exchange_over_plaintext_cold_cache` test.
- Drop this directory and the `[patch.crates-io]` entry as soon as upstream offers a non-`rsa`
  backend (tracking issue: transact-rs/sqlx#4082).
- Do not add other changes here.
