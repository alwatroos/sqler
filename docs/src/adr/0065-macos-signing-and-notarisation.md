# ADR-0065: macOS signing and notarisation

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

The release pipeline ([ADR-0064](0064-release-pipeline.md)) ships the macOS downloads unsigned. The owner decided that
macOS signing and notarisation steps are written now but run only once Apple credentials exist (owner decision 1), and
that Windows stays unsigned (owner decision 2). The project has no Apple Developer account today, so the steps must
be inert, visible in the run and safe to leave in place until the owner enrols.

## Decision Drivers

- No cost and no account for the project until the owner decides to enrol.
- Signing secrets must never be reachable from a pull request or a branch run (ADR-0064, "Signing secrets").
- Every checksum a user can verify must describe the bytes that are downloaded, so all of them are computed after
  signing.
- A signing problem must not stop a release.

## Decision

### Job and gate

A job `sign (macOS)` (id `sign-macos`) sits between `build` and `global artifacts` in `release.yml`. It runs only on
publishing runs (`needs.plan.outputs.publishing == 'true'`, a `v*` tag push), on `macos-14`, in the GitHub environment
`release`, whose deployment rule limits it to `v*` tags (the environment that already holds
`SQLER_UPDATE_SIGNING_KEY`). It has `contents: read` and no token. It is the only job that reads `APPLE_*`; the `build`
job, which also runs on pull requests, has a secret-free notice step only.

The secrets live in the environment `release` and have these exact names:

| Secret | Content |
| ------ | ------- |
| `APPLE_CERTIFICATE` | base64 of the Developer ID Application certificate with its private key (`.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | password of that `.p12` |
| `APPLE_API_KEY` | base64 of the App Store Connect API key (`AuthKey_<id>.p8`) |
| `APPLE_API_KEY_ID` | key id of that API key |
| `APPLE_API_ISSUER` | issuer id of the App Store Connect team |

Signing needs the first two. Notarisation needs all five. A step `secrets` reads the secrets only to test whether they
are non-empty and writes two outputs, `sign` and `notarise` (`true` or `false`, never a value). With no
`APPLE_CERTIFICATE` or `APPLE_CERTIFICATE_PASSWORD` the job prints the notice `Unsigned macOS build` and uploads
nothing, so the macOS archives are exactly the bytes of the build job. With a certificate but no API key it signs and
prints the notice `Not notarised`.

The API key (team scoped, revocable) was chosen over an Apple ID with an app-specific password, which would tie CI to
a personal Apple ID.

### What is signed

The signed object is the `sqler` Mach-O binary inside each of the two macOS archives
(`sqler-app-aarch64-apple-darwin.tar.xz` and `sqler-app-x86_64-apple-darwin.tar.xz`). The archives themselves are
re-packed with the signed binary, not signed: tar has no signature. The script `.github/scripts/sign-macos.sh` does,
per archive, on a copy in a temporary directory: import the certificate into a temporary keychain (created in the
runner temp directory, removed on exit, the user's keychain search list is not changed), `codesign` with the hardened
runtime, a secure timestamp and the entitlements file, `codesign --verify --strict`, then notarise, then re-pack with
the same top-level entries. Only at the end are the archive, its `.sha256` sidecar and the checksum in the
`dist-manifest.json` replaced, so a failure leaves the originals untouched. The script never uses `set -x` and never
echoes a secret; the only secret on a command line is the `.p12` password of `security import`, which has no other
input form.

### Notarisation, no stapling

The signed binary is zipped (`ditto -c -k --keepParent`) and submitted with `xcrun notarytool submit --wait --timeout
20m` using the API key. The result must be `Accepted`; otherwise the script prints the notary log and fails. The ticket
is not stapled in 1.0: `stapler` supports only `.app`, `.pkg` and `.dmg`, and the 1.0 download is a tar archive with a
bare binary. Gatekeeper fetches the ticket online on first run. A future `.app` or `.dmg` would be stapled.

### Order and checksums

`global artifacts` and `host` need `sign (macOS)`. `global` downloads the build artifacts, so the per-artifact
`.sha256` files, `sha256.sum`, the manifest and the installers are all computed or checked after signing. The signing
job re-uploads each macOS artifact under its original name (`overwrite: true`) with every file it contained, after
running `release-guard.sh` on it. Linux and Windows are untouched.

### Failure

If signing or notarisation fails while the secrets exist, the sign step is `continue-on-error`: the job emits the
warning `macOS signing failed`, that target keeps its unsigned archive and the release continues. The release must not
depend on Apple's service availability. To make a failure fatal, remove `continue-on-error` from the two sign steps
(a one-line change per step); this is a decision for the owner once signing is on.

### Windows and Linux

Windows is unsigned by owner decision 2: no step, no script and no secret exists for it, and the SmartScreen note in
the install page is the mitigation. Linux downloads carry checksums only. `github-attestations` stays off.

### Entitlements

`packaging/macos/entitlements.plist` is passed to `codesign` with the hardened runtime. The app links the Cranelift
code generator of the plugin host (`plugins` feature, owner decision 12), which writes machine code to memory at run
time, so the question is which entitlements the hardened runtime needs for that.

The committed set is a single key: `com.apple.security.cs.allow-unsigned-executable-memory`.

Method: the plugin host test binary (`cargo test -p sqler-plugin-host --features cl --lib --no-run`) was signed ad hoc
with the hardened runtime (`codesign --force --options runtime --sign - --entitlements <plist> <binary>`) once per
entitlement set, and run with the filter `runtime::` (38 tests that instantiate WebAssembly modules and so generate
machine code at run time). Results on macOS:

| Entitlements | Outcome |
| ------------ | ------- |
| none | killed by the system (SIGKILL, exit 137) partway through the tests |
| `allow-jit` only | killed, exit 137 |
| `allow-jit` and `allow-unsigned-executable-memory` | 38 passed |
| `allow-unsigned-executable-memory` only | 38 passed |

`allow-jit` alone is not enough, and with `allow-unsigned-executable-memory` present `allow-jit` adds nothing, so the
smaller set is committed.

### Cost

None to the project until the owner enrols in the Apple Developer Program. The steps skip with a notice while the
secrets are absent. How the owner provides the secrets is in
[Signing (owner guide)](../releasing/signing.md).

## Consequences

- Until the secrets exist, macOS downloads are unsigned and Gatekeeper warns on first run, as the install page says.
- With a certificate but no notary key, the binary is signed but not notarised, and Gatekeeper still warns.
- The sign job adds a macOS runner step to every tag run, bounded by `timeout-minutes: 60` and by a 20-minute notary
  timeout per archive.
- A failed signing run ships an unsigned macOS archive with a warning, not an error; the warning must be read.
- The code path with secrets cannot run in CI before the owner adds them; it is tested locally with a self-signed
  identity (below).

## Local test

The script supports `SQLER_SIGN_LOCAL_TEST=1`: no secure timestamp, an untrusted self-signed identity is accepted, and
nothing is notarised. The procedure (self-signed code signing certificate, test archive, positive and negative checks)
is in the owner guide, section "Test the script locally".

## Measurements

Local test on macOS with a throwaway self-signed identity and a small stub binary named `sqler`:

- Positive: exit 0, last line `sign-macos: ok (..., notarised: no)`. On the extracted binary `codesign --verify --strict`
  passes, the flags are `0x10000(runtime)`, the entitlements show the single key above, the binary runs, the sidecar
  `shasum -a 256 -c` passes, the manifest digest equals the new hash and the tar entries are unchanged.
- Negative: a wrong certificate password exits 1 and leaves the archive and manifest byte-identical; a file that is
  not an archive exits 1; a missing tool exits 1; a missing `APPLE_CERTIFICATE` or no arguments exit 2. The captured
  output contains neither the password nor the certificate.
- Cleanup: no temporary directory, keychain entry or identity is left behind.

Not testable without Apple credentials: real notarisation, the secure timestamp and the Developer ID chain.

The environment `release` is restricted to `v*` tags (deployment rule of type tag, verified through the GitHub API), so
a run on a branch or a pull request cannot read the Apple secrets even if a workflow names the environment.

## Rollback

Remove the `sign-macos` job from `release.yml` (and `sign-macos` from the `needs` and `if` of `global` and `host`),
restore the placeholder notice step in `build`, and delete `.github/scripts/sign-macos.sh` and `packaging/macos/`.
Nothing else depends on them.
