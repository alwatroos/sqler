# 0018 RSA without the `rsa` crate: a `ring` SSH signer and a vendored `sqlx-mysql` patch on `aws-lc-rs`

- Status: accepted
- Date: 2026-10-20
- Refines: [ADR-0010](0010-mysql-driver.md) and [ADR-0013](0013-ssh-tunnel-crate.md), which left both RSA features off

## Context and problem statement

Two features were deliberately disabled in M2 because the only RSA implementation behind them is the `rsa` crate, which carries RUSTSEC-2023-0071 (Marvin timing attack, no patched version):

- (a) RSA SSH user keys in `sqler-tunnel` (`russh` feature `rsa`).
- (b) The MySQL RSA password exchange over a connection without TLS (`caching_sha2_password` with a cold server cache, and `sha256_password`; `sqlx-mysql` feature `rsa`).

Many bastions still hold RSA user keys, and a plaintext MySQL login to a new account fails without (b). The product owner asked for an investigation of alternatives that keep `rsa` out of the tree and add no advisory ignore. The constraints are: the `rsa` crate is never added, directly or transitively; `cargo deny check` stays clean with no new ignore; CI stays green on Linux, macOS and Windows; no hand-written cryptography (key-format decoding and constant-time big-integer arithmetic on key components from a vetted crate are allowed; padding, hashing and modular exponentiation must come from `ring` or `aws-lc-rs`).

## Decision drivers

- No `rsa` crate, no advisory ignore (hard constraint).
- Constant-time, maintained, widely reviewed cryptography.
- Licences accepted by `deny.toml` without an exception.
- No new tool-install step in CI (no CMake, NASM, Go, LLVM).
- A small, justified, maintainable diff for any fork.

## Considered options

Part (a), SSH:

1. Keep the refusal (status quo).
2. `russh` feature `rsa`: pulls the `rsa` crate. Rejected by the constraint.
3. A signer adapter over `ring`: `russh` has a pluggable `russh::Signer` trait (the agent client uses it), so the signing can be done outside `russh`.
4. The `aws-lc-rs` backend of `russh` (question QA1: does any `russh` backend sign RSA without the `rsa` crate?). Rejected, see the evidence below.

Part (b), MySQL:

1. Keep the refusal.
2. `sqlx-mysql` feature `rsa`: pulls the `rsa` crate. Rejected.
3. An upstream option for another backend: none exists. No released or merged `sqlx` change offers a non-`rsa` backend, and `MySqlConnectOptions` has no hook for an auth plugin or an encryption function. The upstream request is transact-rs/sqlx#4082 (open).
4. `ring`: has no RSA encryption API and no raw public-key operation. RSA-OAEP on top of `ring` would be hand-written cryptography. Rejected.
5. A patch of `sqlx-mysql` that does the OAEP step with `aws-lc-rs`.
6. Switch to `mysql_async`/`mysql_common`: its own unaudited RSA code, and ADR-0010 chose `sqlx`. Rejected.
7. `openssl`, `boring`: a system library on Windows, or a CMake and Go build. Rejected. `graviola` was not pursued.

## Decision outcome

Both parts are GO. Chosen: (a) option 3, (b) option 5. The evidence below comes from the phase A investigation of 2026-10-20 (cargo 1.99.0, cargo-deny 0.20.2, versions read live from crates.io), including working spikes against OpenSSH 10.0p2 and MySQL 8.4.

### (a) RSA SSH user keys: a `ring` signer

Versions checked live on crates.io on 2026-10-20: `russh` 0.64.1 (released 2026-10-05; the 0.64.0 of 2026-10-04 and the 0.63.x line have the same feature layout) and `ssh-key` 0.7.0-rc.11 (2026-06-29; still a release candidate, the version `russh` 0.64.1 depends on). `ring` 0.17.14.

QA1, the `aws-lc-rs` backend of `russh`: not an option. In `russh` 0.64.1 `Cargo.toml`, `aws-lc-rs = ["dep:aws-lc-rs"]` only selects the crypto library for the ciphers and key exchange (`aws_lc_rs` is referenced in `src/cipher/` only, nowhere in `src/keys/`), and RSA signing and verification stay behind the separate feature `rsa = ["dep:rsa", "dep:pkcs1", "ssh-key/rsa"]`, which is also what turns on `Algorithm::Rsa` in the supported list. So no `russh` backend, `aws-lc-rs` included, signs RSA without the `rsa` crate; that `aws-lc-rs` is now in the tree for MySQL changes nothing for `russh`. This was established by reading the 0.64.1 source in phase A. Choosing the `aws-lc-rs` backend would also bring the C build of `aws-lc-sys` into the SSH path for no RSA benefit, so `russh` stays on `ring`.

- `russh::Signer::auth_sign(&mut self, key, hash_alg, to_sign) -> Vec<u8>` is used through `Handle::authenticate_publickey_with(user, public_key, hash, &mut signer)`. It returns the signed data with the signature blob appended. With `rsa` off this path never parses or verifies the RSA signature locally.
- `sqler-tunnel/src/rsa_key.rs` holds `RsaSigner`. The key comes from an OpenSSH file (plain or passphrase-encrypted, through `ssh-key`, which exposes `n, e, d, p, q, iqmp` without `rsa`) or from an unencrypted PKCS#1 or PKCS#8 PEM (`pem-rfc7468` to DER, then `ring`). Signing is `ring` `RSA_PKCS1_SHA256` / `RSA_PKCS1_SHA512` (`rsa-sha2-256`, `rsa-sha2-512`) with `SystemRandom`.
- OpenSSH files carry no `dP` and `dQ`, which `ring` needs. They are computed with `crypto-bigint` 0.7.5 (`d mod (p-1)`, `d mod (q-1)` with the constant-time `div_rem`; both the quotient and the remainder are wiped, because `BoxedUint` has no zeroizing `Drop`). `num-bigint` on secrets is not acceptable and is not used. Every secret buffer we own is zeroized; the `ring` `RsaKeyPair` held for the session is `ring` state and is not wiped on drop. The PKCS#8 RSA check reads only the fixed header before the key octets, never the key bytes. After loading, one test signature validates the key, because `ring` checks the CRT values only when signing.
- `crypto-bigint`, `pem-rfc7468` and `zeroize` were already in `Cargo.lock`; `ring` 0.17.14 is already the TLS provider. No new native build.
- Limits (documented in the user guide):
  - Moduli of 2048, 3072 and 4096 bits only (a `ring` restriction); 1024-bit and other sizes are refused with a readable error.
  - No `ssh-rsa` (SHA-1) signatures. If the server offers only SHA-1 RSA, the error says so. A server that sends no extension info gets SHA-256.
  - No RSA-only host keys: host key verification still needs the `rsa` feature of `russh`. A server with an Ed25519 or ECDSA host key is fine.
  - Legacy-encrypted PEM (`Proc-Type`/`DEK-Info`) is refused with a hint to convert it with `ssh-keygen -p -o`. Encrypted PKCS#8 (`ENCRYPTED PRIVATE KEY`) is not read by the RSA path; it goes to the generic `russh` loader, which handles Ed25519 and ECDSA keys, and an RSA key in that form fails with the error text of that loader (no conversion hint). Convert such a key with `ssh-keygen -p -o -f <file>`.
  - RSA keys in an SSH agent already worked before this change, because the agent signs. They are not affected by these limits.
- `TunnelError::UnsupportedKeyType` is removed; the refusals above are `KeyFile` or `Ssh` errors with readable messages that name Ed25519 and ECDSA as the alternative.

### (b) MySQL RSA exchange: a vendored `sqlx-mysql` patch

- In `sqlx-mysql` 0.9.0 `src/connection/auth.rs`, the RSA step encrypts the (scrambled) password with the server's SPKI PEM public key using RSA-OAEP with SHA-1 and MGF1-SHA-1, for both plugins. With the feature off, a stub returns the error "RSA auth backend disabled".
- The patch (`vendor/sqlx-mysql`, a copy of the crates.io 0.9.0 source) adds a Cargo feature `aws-lc-rs` and a `rsa_backend` module that decodes the PEM with `pem-rfc7468` and encrypts with `aws-lc-rs` (`OaepPublicEncryptingKey`, `OAEP_SHA1_MGF1SHA1`). The diff is 56 changed lines in 4 files (the backend, plus the downgrade guard below), below the 60-line limit set for a fork. Everything else, including the upstream `LICENSE-APACHE` and `LICENSE-MIT`, is untouched. `vendor/sqlx-mysql/README.md` states the origin (upstream commit `003b698e`) and the diff.
- Wiring: `[patch.crates-io] sqlx-mysql = { path = "vendor/sqlx-mysql" }` in the root `Cargo.toml`, and `sqler-driver-mysql` depends directly on `sqlx-mysql = "=0.9.0"` with the feature `aws-lc-rs`. `sqlx` itself stays on 0.9.0. The vendored crate is not a workspace member, so `fmt`, `clippy` and the doc build do not touch it. A source patch is the only route that uses a documented API of a released crate on both sides without writing cryptography; there is no git dependency, so `allow-git` stays unset.
- Why a vendored path and not a git fork: a git fork needs a hosted repository, `allow-git` in `deny.toml` and a second place to maintain. A path copy is reviewed in the same PR.

#### Maintenance procedure

1. The workspace pins `sqlx-mysql = "=0.9.0"`, so a `cargo update` never moves it silently.
2. On every `sqlx` bump: download the new `sqlx-mysql` from crates.io into `vendor/sqlx-mysql`, re-apply the hunks listed in its README, change the pin and the README, and run the MySQL test `rsa_exchange_over_plaintext_cold_cache`.
3. Drop condition: remove `vendor/sqlx-mysql`, the `[patch.crates-io]` entry and the direct `sqlx-mysql` dependency as soon as upstream offers a non-`rsa` backend (tracking issue transact-rs/sqlx#4082). Supersede this ADR then.
4. Nothing but the RSA backend may change in `vendor/`.

#### Downgrade protection: plaintext RSA only with an explicit `ssl-mode=disabled`

The server's RSA public key arrives in-band, so over a plaintext link an active attacker can substitute their own key and read the password. Before this change that path failed ("RSA auth backend disabled"), so it was not a risk. Enabling the exchange would open a downgrade under the default `PREFERRED`: an attacker removes the server's TLS offer, forces the full `caching_sha2_password` exchange, supplies their own key and reads the password. Decision: the exchange over a plaintext link works only when the user explicitly chose plaintext, never as an opportunistic fallback.

- Implementation: the patched `encrypt_rsa` returns `Error::Configuration("plaintext RSA password exchange needs an explicit ssl-mode=disabled; use TLS")` unless the new field `MySqlStream::plaintext_rsa_allowed` is true. The field is set from the connect options (`MySqlSslMode::Disabled`) when the stream is created, copied when the stream is boxed and false on the TLS stream (5 lines in `stream.rs` and `tls.rs`, plus the guard in `auth.rs`). Over TLS nothing changes (the password is sent directly). `mysql_native_password` and a hot `caching_sha2_password` cache never use the RSA step.
- What protects the password: only `REQUIRED`, `VERIFY_CA` and `VERIFY_IDENTITY` (`require`, `verify-ca`, `verify-full`). `PREFERRED` (the default) does not protect against an active attacker, because TLS can be stripped. With an explicit `ssl-mode=disabled` the RSA exchange protects only against a passive observer: a man in the middle can still read the password with a key of their choosing.
- Cost: with the default `prefer`, a server that offers no TLS and has a cold cache now gets the readable error above, and the user must choose `ssl-mode=disabled` knowingly. The user guide says so.
- Rejected: leaving the exchange on for `prefer` and documenting the risk (a silent downgrade of a password); a guard in `sqler-driver-mysql` that maps `prefer` to another `sqlx` mode (it would change the TLS behaviour of every other plugin).

### Backend weighing

| Aspect | `ring` (SSH signing) | `aws-lc-rs` / `aws-lc-sys` (MySQL OAEP) |
| ------ | -------------------- | ---------------------------------------- |
| Licence | `Apache-2.0 AND ISC`, already allowed | `aws-lc-rs` 1.18.2 `ISC AND (Apache-2.0 OR ISC)`; `aws-lc-sys` 0.46.0 `ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)`. Every term is already on the `deny.toml` allow-list, so no exception is needed (the M2 note that `aws-lc-sys` has a term outside the list no longer holds for 0.46.0) |
| RSA API | signing only, 2048/3072/4096 bits | OAEP encryption with SHA-1/MGF1-SHA-1 |
| Constant time | RSA signing with blinding, documented by `ring` | AWS-LC (BoringSSL/OpenSSL lineage), constant-time and blinded private operations; only the public operation is used here, on a public key |
| Build | the C compiler that CI already has | C build; on x86_64 Windows NASM is needed, avoided with the `prebuilt-nasm` feature. CMake is not needed: `aws-lc-sys` 0.46.0 builds with its own `cc` builder (it has `win_x86_64.rs`) and falls back to CMake only if that fails, and CMake is present on `windows-latest` anyway; the Linux and macOS CI targets need only the C compiler they have |
| Maintenance | none | the vendored patch above |

The `rsa` crate is the baseline that is rejected: variable-time private-key operations, advisory without a fix.

### Windows and `aws-lc-sys`

On `windows-latest` x86_64, `aws-lc-sys` needs NASM (CMake is not required, see the table above), or its `prebuilt-nasm` feature. `sqler-driver-mysql` enables `aws-lc-sys` `prebuilt-nasm` through a target-specific dependency (`cfg(all(windows, target_arch = "x86_64"))`), so no tool-install step is added to `ci.yml`. Only PR CI can confirm the Windows and macOS builds. Decision rule: if the first PR CI run fails on Windows and the failure cannot be fixed without a new tool-install step, part (b) reverts to NO-GO: the vendored patch, the `[patch.crates-io]` entry and the direct dependencies are removed, the typed refusal returns, and this ADR is amended with the evidence. Part (a) does not depend on `aws-lc-*` and stays.

### Guard rails

- `deny.toml`: `[bans] deny = [{ crate = "rsa", reason = "RUSTSEC-2023-0071, ADR-0018" }]`. If any dependency brings `rsa` back, `cargo deny check` fails. There is no advisory ignore and no licence exception.
- `cargo tree --workspace -i rsa` matches nothing and `Cargo.lock` has no `rsa` package.
- `.github/scripts/check-spdx.sh` skips `vendor/`: the vendored files keep their upstream licence files and get no SPDX header (72 upstream files). The check still covers every other tracked `.rs` file.

### Tests

- (a) Unit tests sign with `RsaSigner` and verify with `ring` for SHA-256 and SHA-512 on 2048/3072/4096-bit keys, plus the refusals. Container tests (`tests/ssh_rsa.rs`, Alpine OpenSSH 10 with one sshd limited to `rsa-sha2-256` and one to `rsa-sha2-512`) open real tunnels with plain, PEM, PKCS#8 and encrypted keys. The CI `db` job runs them; the gate id is `ssh`.
- (b) `rsa_exchange_over_plaintext_cold_cache` in the MySQL DB tests creates a `caching_sha2_password` user, runs `FLUSH PRIVILEGES` and connects with `ssl-mode=disabled` on MySQL 8.4 (skipped on `MariaDB`, which has no such plugin). The same test puts a TCP proxy that clears the `CLIENT_SSL` capability in the server handshake (an attacker stripping the TLS offer) in front of the server: `prefer` (URL default, explicit `ssl-mode=preferred` and the typed `TlsMode::Prefer`) is refused with the error above, `require` fails with "server does not support TLS", and an explicit disable works through the same proxy.

## Consequences

- Positive: RSA SSH keys and the plaintext MySQL RSA exchange work, the `rsa` crate stays out and nothing is ignored.
- Negative: a vendored third-party crate to re-sync on every `sqlx` bump; `aws-lc-sys` is a new C build in the tree (and a Windows-only risk until PR CI runs); RSA keys of other sizes, SHA-1-only servers and RSA-only host keys are still unsupported.
- Supersession: this ADR replaces the "no RSA password exchange" decision of ADR-0010 and the "RSA keys unsupported" decision of ADR-0013 (their wording points here).
- Maintainer-only checks (real bastion with an RSA key, Windows CI, a plaintext MySQL exchange against a real server) are listed in the PR body.
