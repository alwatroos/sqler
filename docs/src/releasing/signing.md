# Signing

This page is for the project owner. It explains what is needed to sign and notarise the macOS downloads, and how to
turn it on. The design is in [ADR-0065](../adr/0065-macos-signing-and-notarisation.md).

## Current state

macOS releases are signed and notarised from 1.0.1; Windows stays unsigned. The `sign (macOS)` job in `release.yml`
runs on `v*` tag runs only (a tag push or the Tag release dispatch). It needs all six secrets below: if one is missing,
or signing or notarisation fails, the job fails and nothing publishes. There is no unsigned fallback.

Windows stays unsigned by owner decision: there is no step, script or secret for it. The install page explains the
SmartScreen warning. Linux downloads carry SHA-256 checksums only.

## What the owner provides

The owner is enrolled in the Apple Developer Program (a paid yearly membership) and carries that cost. To provide or
rotate the secrets:

1. Create a **Developer ID Application** certificate (Apple Developer account, Certificates). Export it from
   Keychain Access with its private key as a `.p12` file protected by a password.
2. Encode it: `base64 -i DeveloperID.p12 -o DeveloperID.p12.b64`.
3. In App Store Connect, Users and Access, Integrations, create a **Team API key** with the role Developer. Download
   the `AuthKey_<KEY ID>.p8` file (Apple offers the download once), and note the key id and the issuer id.
4. Encode it: `base64 -i AuthKey_<KEY ID>.p8 -o AuthKey.p8.b64`. Also note the 10-character team ID of the account
   (Apple Developer account, Membership details).
5. Add the secrets to the GitHub **environment** `release` (Settings, Environments, `release`), not to the
   repository secrets:

| Secret name | Value |
| ----------- | ----- |
| `APPLE_CERTIFICATE_P12_BASE64` | content of `DeveloperID.p12.b64` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_TEAM_ID` | the 10-character team ID |
| `APPLE_API_KEY_ID` | the key id |
| `APPLE_API_ISSUER_ID` | the issuer id (a UUID) |
| `APPLE_API_KEY_P8_BASE64` | content of `AuthKey.p8.b64` |

   With the command line, give the value from a file, never on the command line:
   `gh secret set APPLE_CERTIFICATE_P12_BASE64 --env release < DeveloperID.p12.b64`.
6. The environment `release` is already limited to the tag pattern `v*` (deployment rule of type tag), so a run on a
   branch cannot read the secrets. Anyone adding the Apple secrets must put them in this environment and must not
   remove that rule.
7. Delete the local `.b64` and `.p8` files, or keep them in the owner's password manager. GitHub cannot show a secret
   again.

All six secrets are required. The next `v*` tag run signs and notarises both macOS archives. The script also checks
that the certificate is a Developer ID Application of `APPLE_TEAM_ID`, that the signature carries that team, and that
the notarisation ticket is online (`spctl`, a few tries). Nothing is stapled: the download is a bare binary in a tar
archive, which cannot carry a ticket.

## Check a signed download

Extract the macOS archive, then run these on the `sqler` binary:

```text
codesign --verify --strict --verbose=2 sqler
codesign -dvv sqler
spctl --assess --type execute -vv sqler
shasum -a 256 -c sqler-app-aarch64-apple-darwin.tar.xz.sha256
```

Expected: the first command reports a valid signature; `codesign -dvv` shows `Authority=Developer ID Application:
<name> (<TEAM ID>)` and `flags=0x10000(runtime)`; `spctl` says `accepted` with `source=Notarized Developer ID`; the
checksum matches (run it next to the downloaded archive).

If a secret is missing, or signing or notarisation fails, the `sign (macOS)` job fails and nothing publishes (fail
closed). Read the step log (it holds names and reasons, never secret values), fix the cause and re-run the job. There
is no opt-out; the former fallback variable `SQLER_ALLOW_UNSIGNED_FALLBACK` is no longer read.

Pending (maintainer): the entitlements were tested on the plugin-host test binary only, so run a plugin smoke test
(load and run a plugin) on the first signed build.

## Test the script locally

The script `.github/scripts/sign-macos.sh` can be tried on a Mac without Apple credentials. The local test mode needs only
the certificate and its password, uses a self-signed certificate, no secure timestamp, no team check and no
notarisation. Work in a scratch directory and delete it
afterwards.

From the repository root, with a release build of `sqler` (or any Mach-O file named `sqler`):

```sh
d=$(mktemp -d); mkdir "$d/in"
printf '[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=SQLer local test\n[ext]\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n' > "$d/openssl.cnf"
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 1 -config "$d/openssl.cnf" -extensions ext -keyout "$d/k.pem" -out "$d/c.pem"
/usr/bin/openssl pkcs12 -export -inkey "$d/k.pem" -in "$d/c.pem" -out "$d/id.p12" -passout pass:test
mkdir "$d/in/sqler-app-aarch64-apple-darwin"; cp target/release/sqler "$d/in/sqler-app-aarch64-apple-darwin/"
tar -cJf "$d/sqler-app-aarch64-apple-darwin.tar.xz" -C "$d/in" sqler-app-aarch64-apple-darwin
( cd "$d" && shasum -a 256 sqler-app-aarch64-apple-darwin.tar.xz > sqler-app-aarch64-apple-darwin.tar.xz.sha256 )
SQLER_SIGN_LOCAL_TEST=1 APPLE_CERTIFICATE_P12_BASE64=$(base64 < "$d/id.p12") APPLE_CERTIFICATE_PASSWORD=test \
  sh .github/scripts/sign-macos.sh "$d/sqler-app-aarch64-apple-darwin.tar.xz"
```

Expected: exit 0 and a last line `sign-macos: ok (..., notarised: no)`. Then check the result: extract the archive,
run `codesign --verify --strict --verbose=2` and `codesign -dvv` (the flags show `0x10000(runtime)`) on the binary,
`codesign -d --entitlements - <binary>` (one key), and `( cd "$d" && shasum -a 256 -c *.sha256 )`.

The script itself fails (exit 1) when the signature lacks the hardened-runtime flag, so a successful run proves it.

Negative checks: with `APPLE_CERTIFICATE_PASSWORD=wrong` the script must exit 1 with `cannot import the certificate`
and leave the archive byte-identical. Without `APPLE_CERTIFICATE_P12_BASE64` it exits 2.

Without `SQLER_SIGN_LOCAL_TEST`, nothing reaches the network until the key and identity checks: with only the
certificate and password set the script exits 2 and lists the four missing names; with `APPLE_TEAM_ID=bad` it exits 2;
with all six set, a garbage `APPLE_API_KEY_P8_BASE64` exits 1 with `does not hold a .p8 private key`; and with a dummy
PEM key and the self-signed certificate it exits 1 with `not one valid Developer ID Application identity`, leaving the
archive untouched.

Delete the scratch directory when done (`rm -r "$d"`). The script removes its temporary keychain by itself.
