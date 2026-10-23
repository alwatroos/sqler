#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Signs (and, when the App Store Connect key is set, notarises) the `sqler` binary inside a macOS release archive
# (ADR-0065). Only the `sign (macOS)` job of release.yml runs it with real secrets.
#
#   sign-macos.sh <archive.tar.xz> [<dist-manifest.json>]
#
# Environment:
#   APPLE_CERTIFICATE           base64 of the Developer ID Application .p12 (required)
#   APPLE_CERTIFICATE_PASSWORD  password of the .p12 (required)
#   APPLE_API_KEY               base64 of the App Store Connect AuthKey_<id>.p8   } all three set: notarise,
#   APPLE_API_KEY_ID            key id                                           } otherwise sign only and
#   APPLE_API_ISSUER            issuer id                                        } print "not notarised"
#   SQLER_SIGN_ENTITLEMENTS     entitlements file (default packaging/macos/entitlements.plist)
#   SQLER_NOTARY_TIMEOUT        notarytool --timeout (default 20m)
#   SQLER_SIGN_LOCAL_TEST=1     local test: no secure timestamp, accepts an untrusted self-signed identity, never
#                               notarises
#
# Exit 0: the archive is replaced in place (signed; notarised when enabled), the `<archive>.sha256` sidecar digest is
# rewritten (the hex only, dist's format is kept) and, when a manifest is given and holds the archive, its
# `.artifacts["<archive name>"].checksums.sha256` is updated. The last stdout line is
# `sign-macos: ok (<archive name>, sha256 <hex>, notarised: yes|no)`.
# Exit 1: failure; archive, sidecar and manifest are untouched (all work happens on a copy in a temp dir and the
# files are replaced at the very end). Exit 2: usage error or APPLE_CERTIFICATE/APPLE_CERTIFICATE_PASSWORD missing.
# Every message is `sign-macos: <reason>`.
#
# SECRETS: never add `set -x`, never echo a secret or a command line that holds one. The secrets are decoded into
# files in a 0700 temp dir that the EXIT trap removes together with the temporary keychain. The only secret on a
# command line is the `.p12` password of `security import -P` (macOS offers no other way).
#
# Local test (macOS, from the repository root; everything in a scratch dir that you delete afterwards):
#   d=$(mktemp -d); mkdir "$d/in"
#   printf '[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=SQLer local test\n[ext]\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n' > "$d/openssl.cnf"
#   /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 1 -config "$d/openssl.cnf" -extensions ext -keyout "$d/k.pem" -out "$d/c.pem"
#   /usr/bin/openssl pkcs12 -export -inkey "$d/k.pem" -in "$d/c.pem" -out "$d/id.p12" -passout pass:test
#   # an archive in the dist layout: sqler-app-<target>/sqler (a release build, or any Mach-O named sqler)
#   mkdir "$d/in/sqler-app-aarch64-apple-darwin"; cp target/release/sqler "$d/in/sqler-app-aarch64-apple-darwin/"
#   tar -cJf "$d/sqler-app-aarch64-apple-darwin.tar.xz" -C "$d/in" sqler-app-aarch64-apple-darwin
#   ( cd "$d" && shasum -a 256 sqler-app-aarch64-apple-darwin.tar.xz > sqler-app-aarch64-apple-darwin.tar.xz.sha256 )
#   SQLER_SIGN_LOCAL_TEST=1 APPLE_CERTIFICATE=$(base64 < "$d/id.p12") APPLE_CERTIFICATE_PASSWORD=test \
#     sh .github/scripts/sign-macos.sh "$d/sqler-app-aarch64-apple-darwin.tar.xz"
#   # then: extract, `codesign --verify --strict --verbose=2`, `codesign -dvv` (flags=0x10000(runtime)),
#   # `codesign -d --entitlements - <bin>`, and `( cd "$d" && shasum -a 256 -c *.sha256 )`.
# A wrong APPLE_CERTIFICATE_PASSWORD must exit 1 and leave the archive byte-identical.
set -eu
umask 077

work=''
kc=''

say() {
    echo "sign-macos: $1"
}

die() {
    echo "sign-macos: $1" >&2
    exit 1
}

usage() {
    echo "usage: sign-macos.sh <archive.tar.xz> [<dist-manifest.json>]" >&2
    exit 2
}

# Removes the temporary keychain and every decoded secret, on success, failure and signals.
cleanup() {
    rc=$?
    trap - EXIT INT TERM
    if [ -n "$kc" ] && [ -e "$kc" ]; then
        security delete-keychain "$kc" >/dev/null 2>&1 || true
    fi
    if [ -n "$work" ] && [ -d "$work" ]; then
        rm -rf "$work"
    fi
    exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

[ $# -ge 1 ] && [ $# -le 2 ] || usage
archive=$1
manifest=${2:-}
[ -f "$archive" ] || {
    echo "sign-macos: $archive is not a file" >&2
    exit 2
}
if [ -n "$manifest" ] && [ ! -f "$manifest" ]; then
    echo "sign-macos: $manifest is not a file" >&2
    exit 2
fi
if [ -z "${APPLE_CERTIFICATE:-}" ] || [ -z "${APPLE_CERTIFICATE_PASSWORD:-}" ]; then
    echo "sign-macos: APPLE_CERTIFICATE and APPLE_CERTIFICATE_PASSWORD are required" >&2
    exit 2
fi

entitlements=${SQLER_SIGN_ENTITLEMENTS:-packaging/macos/entitlements.plist}
notary_timeout=${SQLER_NOTARY_TIMEOUT:-20m}
local_test=${SQLER_SIGN_LOCAL_TEST:-0}

for tool in security codesign openssl tar base64 shasum jq awk sed find ditto xcrun; do
    command -v "$tool" >/dev/null 2>&1 || die "required tool not found: $tool"
done
[ -f "$entitlements" ] || die "entitlements file not found: $entitlements"

# Notarise only with the whole API key set and outside the local test.
notarise=no
if [ "$local_test" = 1 ]; then
    reason="local test"
elif [ -z "${APPLE_API_KEY:-}" ] || [ -z "${APPLE_API_KEY_ID:-}" ] || [ -z "${APPLE_API_ISSUER:-}" ]; then
    reason="APPLE_API_KEY, APPLE_API_KEY_ID or APPLE_API_ISSUER not set"
else
    notarise=yes
    reason=''
fi
if [ "$notarise" = no ]; then
    say "not notarised ($reason)"
fi

base=$(basename "$archive")
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sqler-sign.XXXXXX") || die "cannot create a temp dir"

# Decode the certificate into the temp dir (never echoed).
p12="$work/cert.p12"
printf '%s' "$APPLE_CERTIFICATE" | base64 --decode >"$p12" 2>/dev/null || die "APPLE_CERTIFICATE is not valid base64"
[ -s "$p12" ] || die "APPLE_CERTIFICATE decodes to nothing"

# Temporary keychain; the user's keychain search list and default keychain are not changed.
kc="$work/sqler-sign.keychain-db"
kcpass=$(openssl rand -hex 24) || die "cannot generate a keychain password"
security create-keychain -p "$kcpass" "$kc" >/dev/null 2>&1 || die "cannot create the temporary keychain"
security set-keychain-settings -lut 3600 "$kc" >/dev/null 2>&1 || die "cannot configure the temporary keychain"
security unlock-keychain -p "$kcpass" "$kc" >/dev/null 2>&1 || die "cannot unlock the temporary keychain"
security import "$p12" -k "$kc" -f pkcs12 -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null 2>&1 ||
    die "cannot import the certificate (wrong password or not a .p12)"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kcpass" "$kc" >/dev/null 2>&1 ||
    die "cannot allow codesign to use the key"

# Exactly one signing identity; the local test accepts an untrusted (self-signed) one.
if [ "$local_test" = 1 ]; then
    idlist=$(security find-identity -p codesigning "$kc" 2>/dev/null || true)
else
    idlist=$(security find-identity -v -p codesigning "$kc" 2>/dev/null || true)
fi
ids=$(printf '%s\n' "$idlist" | awk '$1 ~ /^[0-9]+\)$/ && $2 ~ /^[0-9A-Fa-f]{40}$/ { print $2 }')
count=$(printf '%s\n' "$ids" | awk 'NF { n++ } END { print n + 0 }')
[ "$count" -eq 1 ] || die "expected one valid signing identity in the certificate, found $count"
identity=$ids

# Work on a copy of the archive contents.
ex="$work/extract"
mkdir "$ex"
tar -xJf "$archive" -C "$ex" || die "cannot extract $base"
bins=$(find "$ex" -type f -name sqler)
nbins=$(printf '%s\n' "$bins" | awk 'NF { n++ } END { print n + 0 }')
[ "$nbins" -eq 1 ] || die "expected one sqler binary in $base, found $nbins"
bin=$bins

if [ "$local_test" = 1 ]; then
    stamp=--timestamp=none
else
    stamp=--timestamp
fi
codesign --force --options runtime "$stamp" --entitlements "$entitlements" --keychain "$kc" --sign "$identity" "$bin" ||
    die "codesign failed"
codesign --verify --strict --verbose=2 "$bin" || die "the signature does not verify"
say "signed sqler in $base (hardened runtime)"

if [ "$notarise" = yes ]; then
    p8="$work/AuthKey.p8"
    printf '%s' "$APPLE_API_KEY" | base64 --decode >"$p8" 2>/dev/null || die "APPLE_API_KEY is not valid base64"
    zip="$work/notarise.zip"
    ditto -c -k --keepParent "$bin" "$zip" || die "cannot zip the binary for notarisation"
    result="$work/notary.json"
    submit_rc=0
    xcrun notarytool submit "$zip" --key "$p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" \
        --wait --timeout "$notary_timeout" --output-format json >"$result" 2>"$work/notary.err" || submit_rc=$?
    status=$(jq -r '.status // empty' "$result" 2>/dev/null || true)
    subid=$(jq -r '.id // empty' "$result" 2>/dev/null || true)
    if [ "$status" != Accepted ]; then
        echo "sign-macos: notarisation of $base ended with status '${status:-none}' (notarytool exit $submit_rc)" >&2
        if [ -n "$subid" ]; then
            xcrun notarytool log "$subid" --key "$p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" >&2 || true
        fi
        exit 1
    fi
    say "notarised sqler in $base (submission $subid)"
fi

# Re-pack with the same top-level entries. tar has no signature: the archive is rebuilt, not signed.
new="$work/$base"
set --
for e in "$ex"/* "$ex"/.[!.]*; do
    [ -e "$e" ] || continue
    set -- "$@" "$(basename "$e")"
done
[ $# -gt 0 ] || die "the archive $base is empty"
COPYFILE_DISABLE=1 tar --no-mac-metadata -cJf "$new" -C "$ex" "$@" || die "cannot re-pack $base"
tar -tJf "$new" >/dev/null || die "the re-packed archive is unreadable"

hex=$(shasum -a 256 "$new" | awk '{ print $1 }')
case $hex in
*[!0-9a-f]* | '') die "cannot hash the re-packed archive" ;;
esac
[ "${#hex}" -eq 64 ] || die "cannot hash the re-packed archive"

sidecar="$archive.sha256"
if [ -f "$sidecar" ]; then
    sed "s/^[0-9a-fA-F]\{64\}/$hex/" "$sidecar" >"$work/sidecar" || die "cannot rewrite $sidecar"
fi
if [ -n "$manifest" ]; then
    jq --arg k "$base" --arg h "$hex" \
        'if (.artifacts // {})[$k] != null then .artifacts[$k].checksums.sha256 = $h else . end' \
        "$manifest" >"$work/manifest" || die "cannot rewrite $manifest"
fi

# Replace at the very end: copy next to the target, then rename.
replace() {
    cp "$1" "$2.sign-tmp" && mv -f "$2.sign-tmp" "$2" || {
        rm -f "$2.sign-tmp"
        die "cannot replace $2"
    }
}
replace "$new" "$archive"
if [ -f "$work/sidecar" ]; then
    replace "$work/sidecar" "$sidecar"
fi
if [ -f "$work/manifest" ]; then
    replace "$work/manifest" "$manifest"
fi

say "ok ($base, sha256 $hex, notarised: $notarise)"
