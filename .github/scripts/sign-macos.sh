#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Signs and notarises the `sqler` binary inside a macOS release archive and, with --dmg, also builds, signs, notarises
# and staples the disk image (ADR-0065). Only the `sign (macOS)` job of release.yml runs it with real secrets.
#
#   sign-macos.sh [--dmg <version> <out-dir>] <archive.tar.xz> [<dist-manifest.json>]
#
# --dmg (must come first; <archive> must be named sqler-app-<triple>.tar.xz, else exit 2): after the binary is signed
# and notarised, SQLer.app is staged from it with `packaging/macos/build-dmg.sh --app`, signed (hardened runtime,
# entitlements, secure timestamp), its zip notarised and the app stapled; then `build-dmg.sh --pack` makes the disk
# image, which is signed, notarised and stapled (two more notary submissions). At the end it replaces
# <out-dir>/sqler-app-<triple>.dmg and its .sha256 sidecar (the ad-hoc dmg of the `build` job) next to the archive
# files. Without --dmg the behaviour is exactly the archive-only flow.
#
# Environment (all six are required outside the local test; a missing one exits 2 and names the variable only):
#   APPLE_CERTIFICATE_P12_BASE64  base64 of the Developer ID Application .p12
#   APPLE_CERTIFICATE_PASSWORD    password of the .p12
#   APPLE_TEAM_ID                 10 characters [A-Z0-9]; the identity must be a Developer ID Application of this team
#                                 and the signature's TeamIdentifier must equal it (checked silently, never printed)
#   APPLE_API_KEY_ID              App Store Connect API key id (notarytool --key-id)
#   APPLE_API_ISSUER_ID           issuer id (notarytool --issuer)
#   APPLE_API_KEY_P8_BASE64       base64 (or the raw PEM) of AuthKey_<id>.p8
#   SQLER_SIGN_ENTITLEMENTS       entitlements file (default packaging/macos/entitlements.plist)
#   SQLER_NOTARY_TIMEOUT          notarytool --timeout (default 20m)
#   SQLER_TICKET_TRIES            attempts of the online ticket check, 20 s apart (default 6)
#   SQLER_SIGN_LOCAL_TEST=1       local test: only the certificate and its password are required; no secure timestamp,
#                                 accepts an untrusted self-signed identity, no team check, never notarises
#
# Notarisation is mandatory outside the local test. The ticket of the bare binary is confirmed online with spctl and is
# NOT stapled: a binary in a .tar.xz cannot carry a ticket. The app and the dmg of --dmg are stapled.
#
# Exit 0: the archive is replaced in place (signed and notarised), the `<archive>.sha256` sidecar digest is
# rewritten (the hex only, dist's format is kept) and, when a manifest is given and holds the archive, its
# `.artifacts["<archive name>"].checksums.sha256` is updated. With --dmg the line
# `sign-macos: dmg ok (sqler-app-<triple>.dmg, sha256 <hex>, notarised: yes|no)` comes before the last stdout line, which is
# `sign-macos: ok (<archive name>, sha256 <hex>, notarised: yes|no)` (`no` only in the local test).
# Exit 1: failure; archive, sidecar and manifest are untouched (all work happens on a copy in a temp dir and the
# files are replaced at the very end). Exit 2: usage error or a required variable missing.
# Every message is `sign-macos: <reason>`.
#
# SECRETS: never add `set -x`, never echo a secret or a command line that holds one. The secrets are decoded into
# files in a 0700 temp dir that the EXIT trap removes together with the temporary keychain. The only secret on a
# command line is the `.p12` password of `security import -P` (macOS offers no other way). Output of codesign,
# find-identity and spctl holds the identity and team: it only goes into `grep -q` or a variable never echoed.
#
# In CI (GITHUB_ACTIONS=true) the temporary keychain is first in the user keychain search list until exit, when the
# original list is restored. Simulate it locally with GITHUB_ACTIONS=true on top of the local test below.
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
#   SQLER_SIGN_LOCAL_TEST=1 APPLE_CERTIFICATE_P12_BASE64=$(base64 < "$d/id.p12") APPLE_CERTIFICATE_PASSWORD=test \
#     sh .github/scripts/sign-macos.sh "$d/sqler-app-aarch64-apple-darwin.tar.xz"
#   # then: extract, `codesign --verify --strict --verbose=2`, `codesign -dvv` (flags=0x10000(runtime)),
#   # `codesign -d --entitlements - <bin>`, and `( cd "$d" && shasum -a 256 -c *.sha256 )`.
# A wrong APPLE_CERTIFICATE_PASSWORD must exit 1 and leave the archive byte-identical.
#
# Local --dmg test (the recipe above, plus the dmg of the default build; run from the repository root):
#   mkdir "$d/out"; cp "$d"/sqler-app-aarch64-apple-darwin.tar.xz* "$d/out/"
#   sh packaging/macos/build-dmg.sh "$d/in/sqler-app-aarch64-apple-darwin" 0.0.0 aarch64-apple-darwin "$d/out"
#   (needs LICENSE, NOTICE, THIRD-PARTY-LICENSES.txt and licenses/ next to the binary in the bundle dir)
#   SQLER_SIGN_LOCAL_TEST=1 APPLE_CERTIFICATE_P12_BASE64=$(base64 < "$d/id.p12") APPLE_CERTIFICATE_PASSWORD=test \
#     sh .github/scripts/sign-macos.sh --dmg 0.0.0 "$d/out" "$d/out/sqler-app-aarch64-apple-darwin.tar.xz"
#   # then: `( cd "$d/out" && shasum -a 256 -c *.sha256 )`, attach the dmg and `codesign --verify --deep --strict` the app
#   # (flags=0x10000(runtime)). The local test never notarises or staples, so `check-dmg.sh --notarized` does not apply.
set -eu
umask 077

work=''
kc=''
list_file=''
list_changed=0

say() {
    echo "sign-macos: $1"
}

die() {
    echo "sign-macos: $1" >&2
    exit 1
}

usage() {
    echo "usage: sign-macos.sh [--dmg <version> <out-dir>] <archive.tar.xz> [<dist-manifest.json>]" >&2
    exit 2
}

# Puts the user keychain search list back as it was before the temporary keychain was added (CI only).
restore_list() {
    [ "$list_changed" = 1 ] && [ -n "$list_file" ] && [ -f "$list_file" ] || return 0
    set --
    while IFS= read -r p; do
        [ -n "$p" ] && set -- "$@" "$p"
    done <"$list_file"
    security list-keychains -d user -s "$@" >/dev/null 2>&1 || true
    list_changed=0
}

# Removes the temporary keychain and every decoded secret, on success, failure and signals.
cleanup() {
    rc=$?
    trap - EXIT INT TERM
    restore_list
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

dmg_mode=no
dmg_version=''
dmg_out=''
if [ "${1:-}" = "--dmg" ]; then
    [ $# -ge 4 ] || usage
    dmg_mode=yes
    dmg_version=$2
    dmg_out=$3
    shift 3
    [ -n "$dmg_version" ] || usage
    [ -d "$dmg_out" ] || {
        echo "sign-macos: $dmg_out is not a directory" >&2
        exit 2
    }
fi
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
entitlements=${SQLER_SIGN_ENTITLEMENTS:-packaging/macos/entitlements.plist}
notary_timeout=${SQLER_NOTARY_TIMEOUT:-20m}
local_test=${SQLER_SIGN_LOCAL_TEST:-0}

triple=''
if [ "$dmg_mode" = yes ]; then
    case $(basename "$archive") in
    sqler-app-?*.tar.xz) triple=$(basename "$archive" .tar.xz) ; triple=${triple#sqler-app-} ;;
    *)
        echo "sign-macos: with --dmg the archive must be named sqler-app-<triple>.tar.xz" >&2
        exit 2
        ;;
    esac
fi

tools="security codesign openssl tar base64 shasum jq awk sed find ditto xcrun spctl"
[ "$dmg_mode" = yes ] && tools="$tools hdiutil plutil"
for tool in $tools; do
    command -v "$tool" >/dev/null 2>&1 || die "required tool not found: $tool"
done
[ -f "$entitlements" ] || die "entitlements file not found: $entitlements"

# Required variables: certificate and password always, the rest unless this is the local test.
missing=''
need() {
    eval "v=\${$1:-}"
    [ -n "$v" ] || missing="$missing $1"
}
need APPLE_CERTIFICATE_P12_BASE64
need APPLE_CERTIFICATE_PASSWORD
if [ "$local_test" != 1 ]; then
    need APPLE_TEAM_ID
    need APPLE_API_KEY_ID
    need APPLE_API_ISSUER_ID
    need APPLE_API_KEY_P8_BASE64
fi
if [ -n "$missing" ]; then
    echo "sign-macos: missing required variable(s):$missing" >&2
    exit 2
fi
notarise=yes
if [ "$local_test" = 1 ]; then
    notarise=no
    say "local test: not notarised"
else
    case ${APPLE_TEAM_ID} in
    *[!A-Z0-9]*) echo "sign-macos: APPLE_TEAM_ID has an invalid format" >&2; exit 2 ;;
    esac
    [ "${#APPLE_TEAM_ID}" -eq 10 ] || {
        echo "sign-macos: APPLE_TEAM_ID has an invalid format" >&2
        exit 2
    }
fi
ticket_tries=${SQLER_TICKET_TRIES:-6}
case $ticket_tries in
'' | *[!0-9]*) ticket_tries=6 ;;
esac

base=$(basename "$archive")
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sqler-sign.XXXXXX") || die "cannot create a temp dir"

# Decode the certificate into the temp dir (never echoed).
p12="$work/cert.p12"
printf '%s' "$APPLE_CERTIFICATE_P12_BASE64" | base64 --decode >"$p12" 2>/dev/null || die "APPLE_CERTIFICATE_P12_BASE64 is not valid base64"
[ -s "$p12" ] || die "APPLE_CERTIFICATE_P12_BASE64 decodes to nothing"

# The notary key (base64, or the raw PEM pasted in), validated before any keychain exists.
p8="$work/AuthKey.p8"
if [ "$notarise" = yes ]; then
    case $APPLE_API_KEY_P8_BASE64 in
    -----BEGIN*) printf '%s\n' "$APPLE_API_KEY_P8_BASE64" >"$p8" ;;
    *) printf '%s' "$APPLE_API_KEY_P8_BASE64" | base64 --decode >"$p8" 2>/dev/null || : ;;
    esac
    [ "$(head -n 1 "$p8" 2>/dev/null)" = "-----BEGIN PRIVATE KEY-----" ] ||
        die "APPLE_API_KEY_P8_BASE64 does not hold a .p8 private key"
fi

# Temporary keychain. In CI it is put first in the user keychain search list (restored on exit): codesign resolves the
# identity and builds its certificate chain through the search list, `--keychain` alone is not enough. The default
# keychain is never changed. Outside CI (a developer's Mac) the search list is left alone.
kc="$work/sqler-sign.keychain-db"
kcpass=$(openssl rand -hex 24) || die "cannot generate a keychain password"
security create-keychain -p "$kcpass" "$kc" >/dev/null 2>&1 || die "cannot create the temporary keychain"
security set-keychain-settings "$kc" >/dev/null 2>&1 || die "cannot configure the temporary keychain"
security unlock-keychain -p "$kcpass" "$kc" >/dev/null 2>&1 || die "cannot unlock the temporary keychain"
security import "$p12" -k "$kc" -f pkcs12 -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null 2>&1 ||
    die "cannot import the certificate (wrong password or not a .p12)"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kcpass" "$kc" >/dev/null 2>&1 ||
    die "cannot allow codesign to use the key"

in_ci=no
if [ "${GITHUB_ACTIONS:-}" = true ]; then
    in_ci=yes
    list_file="$work/search-list"
    security list-keychains -d user 2>/dev/null | sed -e 's/^[[:space:]]*"//' -e 's/"[[:space:]]*$//' >"$list_file" ||
        die "cannot read the keychain search list"
    set --
    while IFS= read -r p; do
        [ -n "$p" ] && set -- "$@" "$p"
    done <"$list_file"
    list_changed=1
    security list-keychains -d user -s "$kc" "$@" >/dev/null 2>&1 || {
        restore_list
        die "cannot add the temporary keychain to the search list"
    }
    security unlock-keychain -p "$kcpass" "$kc" >/dev/null 2>&1 || die "cannot unlock the temporary keychain"
fi

# The Developer ID intermediate must resolve. The .p12 normally carries it; when no keychain of the search list has
# it, fetch Apple's public G2 intermediate (official URL, SHA-256 pinned) into the temporary keychain.
if [ "$in_ci" = yes ] && [ "$local_test" != 1 ]; then
    if ! security find-certificate -c "Developer ID Certification Authority" >/dev/null 2>&1; then
        say "Developer ID intermediate not found in the search list; fetching Apple's G2 intermediate"
        g2="$work/DeveloperIDG2CA.cer"
        g2_sha256=f16cd3c54c7f83cea4bf1a3e6a0819c8aaa8e4a1528fd144715f350643d2df3a
        curl -fsSL --proto '=https' --max-time 60 -o "$g2" https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer ||
            die "cannot download the Developer ID intermediate"
        [ "$(shasum -a 256 "$g2" | awk '{ print $1 }')" = "$g2_sha256" ] ||
            die "the downloaded Developer ID intermediate does not match its pinned SHA-256"
        security import "$g2" -k "$kc" >/dev/null 2>&1 || die "cannot import the Developer ID intermediate"
    fi
fi

# Exactly one signing identity; the local test accepts an untrusted (self-signed) one.
if [ "$local_test" = 1 ]; then
    idlist=$(security find-identity -p codesigning "$kc" 2>/dev/null || true)
else
    idlist=$(security find-identity -v -p codesigning "$kc" 2>/dev/null || true)
fi
if [ "$local_test" != 1 ]; then
    idlist=$(printf '%s\n' "$idlist" | grep -E "\"Developer ID Application: .* \(${APPLE_TEAM_ID}\)\"" || true)
    n=$(printf '%s\n' "$idlist" | awk 'NF { n++ } END { print n + 0 }')
    [ "$n" -eq 1 ] || die "the certificate is not one valid Developer ID Application identity of APPLE_TEAM_ID (found $n)"
fi
ids=$(printf '%s\n' "$idlist" | awk '$1 ~ /^[0-9]+\)$/ && $2 ~ /^[0-9A-Fa-f]{40}$/ { print $2 }')
count=$(printf '%s\n' "$ids" | awk 'NF { n++ } END { print n + 0 }')
[ "$count" -eq 1 ] || die "expected one valid signing identity in the certificate, found $count"
identity=$ids
# The quoted common name of the same identity: a fallback if the hash is not resolved (never printed).
cn=$(printf '%s\n' "$idlist" | awk -v h="$identity" '$2 == h' | sed -n 's/^[^"]*"\(.*\)".*$/\1/p' | head -n 1)

# Work on a copy of the archive contents.
ex="$work/extract"
mkdir "$ex"
(umask 022 && tar -xJf "$archive" -C "$ex") || die "cannot extract $base"
bins=$(find "$ex" -type f -name sqler)
nbins=$(printf '%s\n' "$bins" | awk 'NF { n++ } END { print n + 0 }')
[ "$nbins" -eq 1 ] || die "expected one sqler binary in $base, found $nbins"
bin=$bins

if [ "$local_test" = 1 ]; then
    stamp=--timestamp=none
else
    stamp=--timestamp
fi
# sign_with <identity> <path> [runtime]: signs <path>; codesign's output is kept for a redacted diagnosis. With
# `runtime` the hardened runtime and the entitlements are applied (code); a disk image gets neither.
cs_out="$work/codesign.out"
sign_with() {
    if [ "${3:-}" = runtime ]; then
        # umask 022 for codesign: it writes _CodeSignature/CodeResources, which other users must be able to read.
        # The output file is opened here, still under 077.
        (umask 022 && codesign --force --options runtime "$stamp" --entitlements "$entitlements" --keychain "$kc" \
            --sign "$1" "$2") >"$cs_out" 2>&1
    else
        (umask 022 && codesign --force "$stamp" --keychain "$kc" --sign "$1" "$2") >"$cs_out" 2>&1
    fi
}
# Counts only; neither identity nor team appears in the output.
diagnose() {
    nid=$(security find-identity -v -p codesigning 2>/dev/null | awk '$1 ~ /^[0-9]+\)$/ { n++ } END { print n + 0 }')
    nkc=$(security list-keychains 2>/dev/null | awk 'NF { n++ } END { print n + 0 }')
    echo "sign-macos: valid codesigning identities in the search list: $nid; keychains in the search list: $nkc" >&2
    # Only codesign's error text, with the identity, any "Developer ID ...: ... (TEAMID)" and the team redacted.
    sed -e "s/$identity/<redacted>/g" -e "s/${APPLE_TEAM_ID:-NOTEAM}/<team>/g" \
        -e 's/Developer ID [A-Za-z ]*:[^(]*([A-Z0-9]\{10\})/<redacted>/g' \
        -e 's/([A-Z0-9]\{10\})/(<team>)/g' "$cs_out" | head -n 10 | sed 's/^/sign-macos: codesign: /' >&2
}
# sign_path <path> [runtime]: the identity hash first, the common name as a fallback.
sign_path() {
    if ! sign_with "$identity" "$@"; then
        if [ -n "$cn" ] && sign_with "$cn" "$@"; then
            say "signed using the common name (the hash was not resolved)"
        else
            diagnose
            die "codesign failed"
        fi
    fi
}
# team_check <path>: the signature's team equals APPLE_TEAM_ID (silent; never printed). Skipped in the local test.
team_check() {
    [ "$local_test" = 1 ] && return 0
    codesign -dv --verbose=2 "$1" 2>&1 | grep -qx "TeamIdentifier=$APPLE_TEAM_ID" ||
        die "the signature's team does not match APPLE_TEAM_ID"
}
# notarise <file> <label>: submits and waits; anything but Accepted prints the reduced log and exits 1.
notarise() {
    result="$work/notary.json"
    submit_rc=0
    xcrun notarytool submit "$1" --key "$p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" \
        --wait --timeout "$notary_timeout" --output-format json >"$result" 2>"$work/notary.err" || submit_rc=$?
    status=$(jq -r '.status // empty' "$result" 2>/dev/null || true)
    subid=$(jq -r '.id // empty' "$result" 2>/dev/null || true)
    if [ "$status" != Accepted ]; then
        echo "sign-macos: notarisation of $2 ended with status '${status:-none}' (notarytool exit $submit_rc)" >&2
        if [ -n "$subid" ]; then
            nlog="$work/notary-log.json"
            if xcrun notarytool log "$subid" --key "$p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" \
                "$nlog" >/dev/null 2>&1; then
                jq '{status, statusSummary, issues: [.issues[]? | {severity, message, path}]}' "$nlog" >&2 || true
            fi
        fi
        exit 1
    fi
}
# staple <path> <label>: staples and validates; the ticket CDN may lag right after Accepted, so it retries.
staple() {
    i=0
    ok=no
    while [ "$i" -lt "$ticket_tries" ]; do
        if xcrun stapler staple "$1" >/dev/null 2>&1 && xcrun stapler validate "$1" >/dev/null 2>&1; then
            ok=yes
            break
        fi
        i=$((i + 1))
        [ "$i" -lt "$ticket_tries" ] && sleep 20
    done
    [ "$ok" = yes ] || die "cannot staple the notarisation ticket to $2"
}

sign_path "$bin" runtime
codesign --verify --strict --verbose=2 "$bin" || die "the signature does not verify"
codesign -dv --verbose=2 "$bin" 2>&1 | grep -Eq 'flags=0x[0-9a-f]+\(.*runtime' || die "the signature lacks the hardened runtime flag"
team_check "$bin"
say "signed sqler in $base (hardened runtime)"

if [ "$notarise" = yes ]; then
    zip="$work/notarise.zip"
    ditto -c -k --keepParent "$bin" "$zip" || die "cannot zip the binary for notarisation"
    notarise "$zip" "$base"
    say "notarised sqler in $base"
    # The ticket cannot be stapled to a bare binary: confirm it online (the CDN may lag).
    i=0
    ok=no
    while [ "$i" -lt "$ticket_tries" ]; do
        # A bare Mach-O is an executable: assess it as such first, then the document form as a fallback.
        sp=$(spctl --assess --type execute -vv "$bin" 2>&1 || true)
        if printf '%s\n' "$sp" | grep -q 'source=Notarized Developer ID'; then
            ok=yes
            break
        fi
        sp=$(spctl --assess --type open --context context:primary-signature -v "$bin" 2>&1 || true)
        if printf '%s\n' "$sp" | grep -q 'source=Notarized Developer ID'; then
            ok=yes
            break
        fi
        i=$((i + 1))
        [ "$i" -lt "$ticket_tries" ] && sleep 20
    done
    if [ "$ok" != yes ]; then
        # Only source=/override= lines: never origin=, which holds the identity.
        printf '%s\n' "${sp:-}" | grep -E '^(source|override)=' >&2 || true
    fi
    [ "$ok" = yes ] || die "notarised but the ticket is not online yet for $base"
    say "ticket online for sqler in $base (not stapled: bare binary in a tar archive)"
fi

# --dmg: SQLer.app from the signed binary, signed (and notarised, stapled), then the disk image the same way.
if [ "$dmg_mode" = yes ]; then
    dmg_name="sqler-app-$triple.dmg"
    appdir="$work/app"
    dmgdir="$work/dmg"
    mkdir "$appdir" "$dmgdir"
    # umask 022: the script-wide 077 would leave the staged app unreadable to other users.
    (umask 022 && sh packaging/macos/build-dmg.sh --app "$(dirname "$bin")" "$dmg_version" "$appdir" >&2) ||
        die "cannot stage SQLer.app"
    app="$appdir/SQLer.app"
    sign_path "$app" runtime
    codesign --verify --deep --strict "$app" >/dev/null 2>&1 || die "the SQLer.app signature does not verify"
    codesign -dv --verbose=2 "$app" 2>&1 | grep -Eq 'flags=0x[0-9a-f]+\(.*runtime' ||
        die "the SQLer.app signature lacks the hardened runtime flag"
    team_check "$app"
    say "signed SQLer.app (hardened runtime)"
    if [ "$notarise" = yes ]; then
        appzip="$work/app-notarise.zip"
        ditto -c -k --keepParent "$app" "$appzip" || die "cannot zip SQLer.app for notarisation"
        notarise "$appzip" "SQLer.app"
        say "notarised SQLer.app"
        staple "$app" "SQLer.app"
        say "stapled SQLer.app"
    fi
    (umask 022 && sh packaging/macos/build-dmg.sh --pack "$app" "$dmg_version" "$triple" "$dmgdir" >&2) ||
        die "cannot build the disk image"
    dmg="$dmgdir/$dmg_name"
    sign_path "$dmg"
    codesign --verify --strict "$dmg" >/dev/null 2>&1 || die "the dmg signature does not verify"
    team_check "$dmg"
    say "signed $dmg_name"
    if [ "$notarise" = yes ]; then
        notarise "$dmg" "$dmg_name"
        say "notarised $dmg_name"
        staple "$dmg" "$dmg_name"
        i=0
        ok=no
        while [ "$i" -lt "$ticket_tries" ]; do
            sp=$(spctl --assess --type open --context context:primary-signature -v "$dmg" 2>&1 || true)
            if printf '%s\n' "$sp" | grep -q 'source=Notarized Developer ID'; then
                ok=yes
                break
            fi
            i=$((i + 1))
            [ "$i" -lt "$ticket_tries" ] && sleep 20
        done
        if [ "$ok" != yes ]; then
            printf '%s\n' "${sp:-}" | grep -E '^(source|override)=' >&2 || true
            die "$dmg_name is stapled but not assessed as Notarized Developer ID"
        fi
        say "stapled $dmg_name"
    fi
    (cd "$dmgdir" && shasum -a 256 "$dmg_name" >"$dmg_name.sha256") || die "cannot hash $dmg_name"
    dmg_hex=$(awk '{ print $1 }' "$dmgdir/$dmg_name.sha256")
    [ "${#dmg_hex}" -eq 64 ] || die "cannot hash $dmg_name"
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
if [ "$dmg_mode" = yes ]; then
    replace "$dmgdir/$dmg_name" "$dmg_out/$dmg_name"
    replace "$dmgdir/$dmg_name.sha256" "$dmg_out/$dmg_name.sha256"
    say "dmg ok ($dmg_name, sha256 $dmg_hex, notarised: $notarise)"
fi

say "ok ($base, sha256 $hex, notarised: $notarise)"
