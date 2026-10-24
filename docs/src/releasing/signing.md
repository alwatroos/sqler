# Signing

This page is for the project owner. It explains what is needed to sign and notarise the macOS downloads, and how to
turn it on. The design is in [ADR-0065](../adr/0065-macos-signing-and-notarisation.md).

## Current state

The release is unsigned. The `sign (macOS)` job exists in `release.yml` and runs on `v*` tag runs only (a tag push or the Tag release dispatch). While the
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
6. The environment `release` is already limited to the tag pattern `v*` (deployment rule of type tag), so a run on a
   branch cannot read the secrets. Anyone adding the Apple secrets must put them in this environment and must not
   remove that rule.
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

If signing or notarisation fails while the secrets are set, the `sign (macOS)` job fails and nothing publishes (fail
closed); read the step log, fix the cause and re-run the job. To publish anyway, set the repository variable
`SQLER_ALLOW_UNSIGNED_FALLBACK` to `true`: the failing target then ships unsigned with the warning `macOS signing
failed` (possibly next to a signed one). Remove the variable again afterwards.

Pending (maintainer): the entitlements were tested on the plugin-host test binary only, so run a plugin smoke test
(load and run a plugin) on the first signed build.

## Test the script locally

The script `.github/scripts/sign-macos.sh` can be tried on a Mac without Apple credentials. The local test mode uses a
self-signed certificate, no secure timestamp and no notarisation. Work in a scratch directory and delete it
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
SQLER_SIGN_LOCAL_TEST=1 APPLE_CERTIFICATE=$(base64 < "$d/id.p12") APPLE_CERTIFICATE_PASSWORD=test \
  sh .github/scripts/sign-macos.sh "$d/sqler-app-aarch64-apple-darwin.tar.xz"
```

Expected: exit 0 and a last line `sign-macos: ok (..., notarised: no)`. Then check the result: extract the archive,
run `codesign --verify --strict --verbose=2` and `codesign -dvv` (the flags show `0x10000(runtime)`) on the binary,
`codesign -d --entitlements - <binary>` (one key), and `( cd "$d" && shasum -a 256 -c *.sha256 )`.

The script itself fails (exit 1) when the signature lacks the hardened-runtime flag, so a successful run proves it.

Negative check: with `APPLE_CERTIFICATE_PASSWORD=wrong` the script must exit 1 with `cannot import the certificate`
and leave the archive byte-identical. Without `APPLE_CERTIFICATE` it exits 2.

Delete the scratch directory when done (`rm -r "$d"`). The script removes its temporary keychain by itself.

## After signing is on

The macOS section of the install page still describes the unsigned download. Update it when the first signed and
notarised release is out (release item R15 or the owner).
