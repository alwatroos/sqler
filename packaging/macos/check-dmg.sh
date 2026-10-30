#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Checks a disk image built by build-dmg.sh (ADR-0075): attaches it read-only, asserts the layout, the signature
# and `--version`, and always detaches.
#
#   check-dmg.sh [--notarized] <dmg>
#
# --notarized (the `sign (macOS)` gate on `v*` tag runs, ADR-0065) additionally requires: the dmg verifies strictly,
# is stapled and assessed by spctl as `source=Notarized Developer ID`; the mounted app verifies with --deep --strict,
# has the hardened runtime flag and the unsigned-executable-memory entitlement, is stapled and assessed as
# `source=Notarized Developer ID`. Output of codesign and spctl holds the identity: it only goes into `grep -q` or
# variables, and on a spctl failure only the `source=`/`override=` lines are printed.
#
# Exit 0: ok. Exit 1: a failure (::error title=Installer::...). Exit 2: usage error.
set -eu

fail() {
    echo "::error title=Installer::$1"
    exit 1
}

notarized=0
if [ "${1:-}" = "--notarized" ]; then
    notarized=1
    shift
fi
[ $# -eq 1 ] && [ -f "$1" ] || {
    echo "usage: check-dmg.sh [--notarized] <dmg>" >&2
    exit 2
}
dmg=$1

# assess_notarized <spctl args...>: fails unless spctl reports a notarised Developer ID source (prints no identity).
assess_notarized() {
    label=$1
    shift
    sp=$(spctl "$@" 2>&1 || true)
    if ! printf '%s\n' "$sp" | grep -q 'source=Notarized Developer ID'; then
        printf '%s\n' "$sp" | grep -E '^(source|override)=' >&2 || true
        fail "spctl does not assess $label as Notarized Developer ID"
    fi
}

if [ "$notarized" -eq 1 ]; then
    codesign --verify --strict "$dmg" >/dev/null 2>&1 || fail "codesign --verify --strict failed for the dmg"
    xcrun stapler validate "$dmg" >/dev/null 2>&1 || fail "stapler validate failed for the dmg"
    assess_notarized "the dmg" --assess --type open --context context:primary-signature -v "$dmg"
fi

mnt=$(mktemp -d)
vf=$(mktemp)
attached=0
cleanup() {
    if [ "$attached" -eq 1 ]; then
        hdiutil detach "$mnt" -force >/dev/null 2>&1 || true
    fi
    rmdir "$mnt" 2>/dev/null || true
    rm -f "$vf"
}
trap cleanup EXIT

hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dmg" >/dev/null || fail "hdiutil attach failed for $dmg"
attached=1

bin="$mnt/SQLer.app/Contents/MacOS/sqler"
[ -x "$bin" ] || fail "SQLer.app/Contents/MacOS/sqler is missing or not executable"
[ -L "$mnt/Applications" ] || fail "Applications is not a symlink"
[ "$(readlink "$mnt/Applications")" = "/Applications" ] || fail "Applications does not point to /Applications"
icns="$mnt/SQLer.app/Contents/Resources/sqler.icns"
[ -s "$icns" ] || fail "SQLer.app/Contents/Resources/sqler.icns is missing or empty"
[ "$(head -c 4 "$icns")" = "icns" ] || fail "sqler.icns does not start with the icns magic"
[ "$(plutil -extract CFBundleIconFile raw "$mnt/SQLer.app/Contents/Info.plist")" = "sqler" ] ||
    fail "CFBundleIconFile is not sqler"
codesign --verify "$mnt/SQLer.app" || fail "codesign --verify failed for SQLer.app"
# Other users must be able to read the whole app (a restrictive umask at build time would leave 0600 files).
unreadable=$(find "$mnt/SQLer.app" \( \( ! -type l ! -perm -o=r \) -o \( -type d ! -perm -o=x \) \) 2>/dev/null | head -n 1)
[ -z "$unreadable" ] || fail "SQLer.app has an entry other users cannot read: ${unreadable#"$mnt"/}"
[ -n "$(find "$bin" -perm -o=x)" ] || fail "SQLer.app/Contents/MacOS/sqler is not executable by others"
if [ "$notarized" -eq 1 ]; then
    app="$mnt/SQLer.app"
    codesign --verify --deep --strict "$app" >/dev/null 2>&1 || fail "codesign --verify --deep --strict failed for SQLer.app"
    codesign -dv --verbose=2 "$app" 2>&1 | grep -Eq 'flags=0x[0-9a-f]+\(.*runtime' ||
        fail "SQLer.app lacks the hardened runtime flag"
    codesign -d --entitlements - --xml "$app" 2>/dev/null | grep -q 'com.apple.security.cs.allow-unsigned-executable-memory' ||
        fail "SQLer.app lacks the allow-unsigned-executable-memory entitlement"
    xcrun stapler validate "$app" >/dev/null 2>&1 || fail "stapler validate failed for SQLer.app"
    assess_notarized "SQLer.app" --assess --type execute -vv "$app"
fi
"$bin" --version >"$vf" 2>&1 || true
ver=$(cat "$vf")
echo "$ver"
printf '%s\n' "$ver" | grep -Eq '^sqler [0-9]' || fail "--version does not match ^sqler [0-9]"
echo "check-dmg: ok ($dmg)"
