# ADR-0065: macOS signing and notarisation

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

The release pipeline ([ADR-0064](0064-release-pipeline.md)) first shipped the macOS downloads unsigned. The owner decided
that macOS signing and notarisation steps are written now but run only once Apple credentials exist (owner decision 1),
and that Windows stays unsigned (owner decision 2). Amendment 1 below reverses decision 1: macOS is signed and
notarised from 1.0.1.

## Decision Drivers

- The owner has enrolled in the Apple Developer Program, so signing and notarisation run in every tag run.
- Signing secrets must never be reachable from a pull request or a branch run (ADR-0064, "Signing secrets").
- Every checksum a user can verify must describe the bytes that are downloaded, so all of them are computed after
  signing.
- A signing problem stops the release: a macOS download is never published unsigned.

## Decision

### Job and gate

A job `sign (macOS)` (id `sign-macos`) sits between `build` and `global artifacts` in `release.yml`. It runs only on
publishing runs (`needs.plan.outputs.publishing == 'true'`, a `v*` tag push), on `macos-14`, in the GitHub environment
`release`, whose deployment rule limits it to `v*` tags (the environment that already holds
`SQLER_UPDATE_SIGNING_KEY`). It has `contents: read` and no token. It is the only job that reads the `APPLE_*` secrets; the `build`
job, which also runs on pull requests, has a secret-free notice step only.

The secrets live in the environment `release` and have these exact names:

| Secret | Content |
| ------ | ------- |
| `APPLE_CERTIFICATE_P12_BASE64` | base64 of the Developer ID Application certificate with its private key (`.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | password of that `.p12` |
| `APPLE_TEAM_ID` | the 10-character Apple team ID; the identity must belong to it |
| `APPLE_API_KEY_ID` | key id of the App Store Connect API key |
| `APPLE_API_ISSUER_ID` | issuer id of the App Store Connect team |
| `APPLE_API_KEY_P8_BASE64` | base64 of the App Store Connect API key (`AuthKey_<id>.p8`) |

All six are required. A step `secrets` reads the secrets only to test whether they are non-empty. If one is missing it
prints the error `Apple secrets missing` with the names (never a value) and fails the job, so nothing publishes.

The API key (team scoped, revocable) was chosen over an Apple ID with an app-specific password, which would tie CI to
a personal Apple ID.

### What is signed

The signed object is the `sqler` Mach-O binary inside each of the two macOS archives
(`sqler-app-aarch64-apple-darwin.tar.xz` and `sqler-app-x86_64-apple-darwin.tar.xz`). The archives themselves are
re-packed with the signed binary, not signed: tar has no signature. The script `.github/scripts/sign-macos.sh` does,
per archive, on a copy in a temporary directory: import the certificate into a temporary keychain (created in the
runner temp directory, removed on exit, the user's keychain search list is not changed), `codesign` with the hardened
runtime, a secure timestamp and the entitlements file, `codesign --verify --strict`, check that the identity is a
Developer ID Application of `APPLE_TEAM_ID` and that the signature's `TeamIdentifier` equals it, then notarise, then re-pack with
the same top-level entries. Only at the end are the archive, its `.sha256` sidecar and the checksum in the
`dist-manifest.json` replaced, so a failure leaves the originals untouched. The script never uses `set -x` and never
echoes a secret; the only secret on a command line is the `.p12` password of `security import`, which has no other
input form.

### Notarisation, no stapling

The signed binary is zipped (`ditto -c -k --keepParent`) and submitted with `xcrun notarytool submit --wait --timeout
20m` using the API key. The result must be `Accepted`; otherwise the script prints a reduced notary log (status,
summary and issues only) and fails. The ticket is not stapled: `stapler` supports only `.app`, `.pkg` and `.dmg`, and
the download is a tar archive with a bare binary. Gatekeeper fetches the ticket online on first run, and the script
confirms that the ticket is online with `spctl --assess` (`source=Notarized Developer ID`, a bounded number of tries).
A future `.app` or `.dmg` would be stapled.

### Order and checksums

`global artifacts` and `host` need `sign (macOS)`. `global` downloads the build artifacts, so the per-artifact
`.sha256` files, `sha256.sum`, the manifest and the installers are all computed or checked after signing. The signing
job re-uploads each macOS artifact under its original name (`overwrite: true`) with every file it contained, after
running `release-guard.sh` on it. Linux and Windows are untouched.

### Failure

The pipeline fails closed. If a secret is missing, or signing, notarisation, the team check, the online ticket check or
the hardened-runtime check fails, the `sign (macOS)` job fails and nothing publishes, so a release can never mix a
signed and an unsigned macOS download. There is no opt-out: an Apple outage delays the release until the job is
re-run.

The verification of each signed archive (and the script itself) asserts the hardened-runtime flag: the output of
`codesign -dv --verbose=2` must contain `flags=0x...(...runtime...)`, otherwise the job fails.

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

The owner pays the Apple Developer Program membership. How the owner provides the secrets is in
[Signing (owner guide)](../releasing/signing.md).

## Consequences

- From 1.0.1 the macOS downloads are signed and notarised and open without the Gatekeeper prompt. 1.0.0 stays unsigned.
- The sign job adds a macOS runner step to every tag run, bounded by `timeout-minutes: 60` and by a 20-minute notary
  timeout per archive.
- A failed signing run blocks the release (fail closed); Apple service outages therefore delay a release until the job
  is re-run.
- The entitlements were tested on the plugin-host test binary, not on the shipped `sqler`. Pending (maintainer): run a
  plugin smoke test (load and run a plugin) on the first signed build.
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
  not an archive exits 1; a missing tool exits 1; a missing `APPLE_CERTIFICATE_P12_BASE64` or no arguments exit 2. The captured
  output contains neither the password nor the certificate.
- Cleanup: no temporary directory, keychain entry or identity is left behind.

Not testable without Apple credentials: real notarisation, the secure timestamp, the Developer ID chain, the team check and the online ticket check. Without a network the script was also run with all six variables set and a dummy key: a garbage key exits 1 at the key check and a self-signed certificate exits 1 at the Developer ID identity check, both with the archive untouched.

The environment `release` is restricted to `v*` tags (deployment rule of type tag, verified through the GitHub API), so
a run on a branch or a pull request cannot read the Apple secrets even if a workflow names the environment.

## Rollback

Remove the `sign-macos` job from `release.yml` (and `sign-macos` from the `needs` and `if` of `global` and `host`),
restore the notice step in `build`, and delete `.github/scripts/sign-macos.sh` and `packaging/macos/`.
Nothing else depends on them.

## Amendment 1 (2026-10-25, T1): signed and notarised from 1.0.1, fail closed

- Owner decision 1 is reversed (owner request, 2026-10-25): macOS is signed and notarised from 1.0.1. Windows stays
  unsigned. The 1.0.0 downloads stay as they are.
- The six secrets in the environment `release` are `APPLE_CERTIFICATE_P12_BASE64`, `APPLE_CERTIFICATE_PASSWORD`,
  `APPLE_TEAM_ID`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID` and `APPLE_API_KEY_P8_BASE64` (table above). All six are
  required; a missing one fails the job.
- Team check: the identity must be a Developer ID Application of `APPLE_TEAM_ID`, and the signature's
  `TeamIdentifier` must equal it. The check runs silently; the identity and the team are never printed.
- Notarisation is mandatory, and the script confirms the ticket online with `spctl` (nothing is stapled).
- The opt-out variable that let a failed target ship unsigned is removed and is no longer read. A publishing run can
  never ship an unsigned macOS archive.
- Cost and rollback: the owner carries the membership cost. Rollback is as in the section above, or a re-run after an
  Apple outage.
