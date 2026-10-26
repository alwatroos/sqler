#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Builds the macOS disk image (ADR-0075) from the extracted release archive.
#
#   build-dmg.sh <bundle-dir> <version> <triple> <out-dir>
#
# <bundle-dir> holds `sqler`, LICENSE, NOTICE, THIRD-PARTY-LICENSES.txt and licenses/. Writes
# <out-dir>/sqler-app-<triple>.dmg and its .sha256 sidecar. The app is signed ad hoc only (no identity, no hardened
# runtime, no notarisation). Exit 0: ok. Exit 1: a failure (::error title=Installer::...). Exit 2: usage error.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

fail() {
    echo "::error title=Installer::$1"
    exit 1
}

usage() {
    echo "usage: build-dmg.sh <bundle-dir> <version> <triple> <out-dir>" >&2
    exit 2
}

[ $# -eq 4 ] || usage
bundle=$1
version=$2
triple=$3
out=$4
[ -d "$bundle" ] || usage
[ -x "$bundle/sqler" ] || fail "$bundle/sqler is missing or not executable"
[ -n "$version" ] && [ -n "$triple" ] || usage
mkdir -p "$out"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
app="$stage/SQLer.app/Contents"
mkdir -p "$app/MacOS" "$app/Resources/licenses"
install -m 755 "$bundle/sqler" "$app/MacOS/sqler"
for f in LICENSE NOTICE THIRD-PARTY-LICENSES.txt; do
    [ -s "$bundle/$f" ] || fail "$f is missing from $bundle"
    install -m 644 "$bundle/$f" "$app/Resources/$f"
done
cp -R "$bundle/licenses/." "$app/Resources/licenses/" || fail "licenses/ is missing from $bundle"

short=${version%%-*}
sed "s/@SHORT_VERSION@/$short/g; s/@VERSION@/$version/g" "$here/Info.plist.in" >"$app/Info.plist"
plutil -lint "$app/Info.plist" >/dev/null || fail "Info.plist is not valid"

sh "$root/.github/scripts/release-guard.sh" binary "$app/MacOS/sqler"
sh "$root/.github/scripts/release-guard.sh" artifacts "$stage"

codesign --force --deep --sign - "$stage/SQLer.app" || fail "ad-hoc codesign failed"
ln -s /Applications "$stage/Applications"

dmg="$out/sqler-app-$triple.dmg"
n=0
until hdiutil create -volname "SQLer $version" -srcfolder "$stage" -ov -format UDZO "$dmg"; do
    n=$((n + 1))
    [ "$n" -lt 3 ] || fail "hdiutil create failed 3 times"
    echo "hdiutil create failed, retrying ($n/3)" >&2
    sleep 5
done

(cd "$out" && shasum -a 256 "sqler-app-$triple.dmg" >"sqler-app-$triple.dmg.sha256")
echo "built $dmg"
