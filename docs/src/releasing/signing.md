# Signing

This page is for the project owner. It explains what is needed to sign and notarise the macOS downloads, and how to
turn it on. The design is in [ADR-0065](../adr/0065-macos-signing-and-notarisation.md).

## Current state

The release is unsigned. The `sign (macOS)` job exists in `release.yml` and runs on `v*` tag pushes only. While the
secrets below are absent it prints two notices in the run (`Unsigned macOS build` and `Not notarised`), uploads
nothing, and the macOS archives keep the bytes of the build job. Nothing costs money until the owner enrols with
Apple.

Windows stays unsigned by owner decision: there is no step, script or secret for it. The install page explains the
SmartScreen warning. Linux downloads carry SHA-256 checksums only.

## What the owner provides

The owner decides whether to enrol in the Apple Developer Program (a paid yearly membership, Apple's price at the
time) and carries that cost. The team does not enrol on the owner's behalf. Once enrolled:

1. Create a **Developer ID Application** certificate (Apple Developer account, Certificates). Export it from
   Keychain Access with its private key as a `.p12` file protected by a password.
2. Encode it: `base64 -i DeveloperID.p12 -o DeveloperID.p12.b64`.
3. In App Store Connect, Users and Access, Integrations, create a **Team API key** with the role Developer. Download
   the `AuthKey_<KEY ID>.p8` file (Apple offers the download once), and note the key id and the issuer id.
4. Encode it: `base64 -i AuthKey_<KEY ID>.p8 -o AuthKey.p8.b64`.
5. Add the secrets to the GitHub **environment** `release` (Settings, Environments, `release`), not to the
   repository secrets:

| Secret name | Value |
| ----------- | ----- |
| `APPLE_CERTIFICATE` | content of `DeveloperID.p12.b64` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_API_KEY` | content of `AuthKey.p8.b64` |
| `APPLE_API_KEY_ID` | the key id |
| `APPLE_API_ISSUER` | the issuer id (a UUID) |

   With the command line, give the value from a file, never on the command line:
   `gh secret set APPLE_CERTIFICATE --env release < DeveloperID.p12.b64`.
6. Check that the environment `release` has a deployment rule limiting it to the tag pattern `v*`. Without it, a run
   on a branch that names the environment could read the secrets.
7. Delete the local `.b64` and `.p8` files, or keep them in the owner's password manager. GitHub cannot show a secret
   again.

Signing needs the first two secrets. Notarisation needs all five. The next `v*` tag run then signs and notarises both
macOS archives; with only the first two, it signs and prints the `Not notarised` notice.

## Check a signed download

Extract the macOS archive, then run these on the `sqler` binary:

```text
codesign --verify --strict --verbose=2 sqler
codesign -dvv sqler
spctl --assess --type open --context context:primary-signature -v sqler
shasum -a 256 -c sqler-app-aarch64-apple-darwin.tar.xz.sha256
```

Expected: the first command reports a valid signature; `codesign -dvv` shows `Authority=Developer ID Application:
<name> (<TEAM ID>)` and `flags=0x10000(runtime)`; `spctl` says `accepted` with `source=Notarized Developer ID`; the
checksum matches (run it next to the downloaded archive).

If signing failed in the run, the job shows the warning `macOS signing failed` and that target's archive is unsigned;
read the step log of `sign (macOS)`.

## Test the script locally

The script `.github/scripts/sign-macos.sh` can be tried on a Mac without Apple credentials. The local test mode uses a
self-signed certificate, no secure timestamp and no notarisation. Work in a scratch directory and delete it
afterwards.

PLACEHOLDER (to be completed by the integration step with the exact commands from the local test of work package W1):
create the self-signed code signing identity with `/usr/bin/openssl`, export it as a `.p12`, build a test archive in
the layout of the release archive, run `SQLER_SIGN_LOCAL_TEST=1 sh .github/scripts/sign-macos.sh <archive>`, and the
positive and negative checks.

## After signing is on

The macOS section of the install page still describes the unsigned download. Update it when the first signed and
notarised release is out (release item R15 or the owner).
