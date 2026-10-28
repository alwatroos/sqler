#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Checks a disk image built by build-dmg.sh (ADR-0075): attaches it read-only, asserts the layout, the ad-hoc
# signature and `--version`, and always detaches.
#
#   check-dmg.sh <dmg>
#
# Exit 0: ok. Exit 1: a failure (::error title=Installer::...). Exit 2: usage error.
set -eu

fail() {
    echo "::error title=Installer::$1"
    exit 1
}

[ $# -eq 1 ] && [ -f "$1" ] || {
    echo "usage: check-dmg.sh <dmg>" >&2
    exit 2
}
dmg=$1

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
"$bin" --version >"$vf" 2>&1 || true
ver=$(cat "$vf")
echo "$ver"
printf '%s\n' "$ver" | grep -Eq '^sqler [0-9]' || fail "--version does not match ^sqler [0-9]"
echo "check-dmg: ok ($dmg)"
