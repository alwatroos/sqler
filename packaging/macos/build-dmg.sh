#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Builds the macOS disk image (ADR-0075) from the extracted release archive.
#
#   build-dmg.sh <bundle-dir> <version> <triple> <out-dir>        ad-hoc signed dmg + .sha256 (dry runs, `build` job)
#   build-dmg.sh --app <bundle-dir> <version> <app-out-dir>       writes <app-out-dir>/SQLer.app, UNSIGNED
#   build-dmg.sh --pack <app> <version> <triple> <out-dir>        dmg + .sha256 from a ready SQLer.app, no codesign
#
# <bundle-dir> holds `sqler`, LICENSE, NOTICE, THIRD-PARTY-LICENSES.txt and licenses/. Writes
# <out-dir>/sqler-app-<triple>.dmg and its .sha256 sidecar. The default mode is `--app`, an ad-hoc `codesign`, then
# `--pack` (no identity, no hardened runtime, no notarisation). On `v*` tag runs `sign-macos.sh --dmg` (ADR-0065)
# uses `--app` and `--pack` around its own Developer ID signing and notarisation. Exit 0: ok. Exit 1: a failure
# (::error title=Installer::...). Exit 2: usage error.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

fail() {
    echo "::error title=Installer::$1"
    exit 1
}

usage() {
    echo "usage: build-dmg.sh <bundle-dir> <version> <triple> <out-dir>" >&2
    echo "       build-dmg.sh --app <bundle-dir> <version> <app-out-dir>" >&2
    echo "       build-dmg.sh --pack <app> <version> <triple> <out-dir>" >&2
    exit 2
}

# stage_app <bundle> <version> <dest>: writes <dest>/SQLer.app (unsigned) and runs the release guards.
stage_app() {
    bundle=$1
    version=$2
    dest=$3
    [ -d "$bundle" ] || usage
    [ -x "$bundle/sqler" ] || fail "$bundle/sqler is missing or not executable"
    [ -n "$version" ] || usage
    mkdir -p "$dest"
    app="$dest/SQLer.app/Contents"
    mkdir -p "$app/MacOS" "$app/Resources/licenses"
    install -m 755 "$bundle/sqler" "$app/MacOS/sqler"
    for f in LICENSE NOTICE THIRD-PARTY-LICENSES.txt; do
        [ -s "$bundle/$f" ] || fail "$f is missing from $bundle"
        install -m 644 "$bundle/$f" "$app/Resources/$f"
    done
    [ -s "$root/assets/icon/sqler.icns" ] || fail "assets/icon/sqler.icns is missing"
    install -m 644 "$root/assets/icon/sqler.icns" "$app/Resources/sqler.icns"
    cp -R "$bundle/licenses/." "$app/Resources/licenses/" || fail "licenses/ is missing from $bundle"

    short=${version%%-*}
    sed "s/@SHORT_VERSION@/$short/g; s/@VERSION@/$version/g" "$here/Info.plist.in" >"$app/Info.plist"
    plutil -lint "$app/Info.plist" >/dev/null || fail "Info.plist is not valid"

    sh "$root/.github/scripts/release-guard.sh" binary "$app/MacOS/sqler"
    sh "$root/.github/scripts/release-guard.sh" artifacts "$dest"
}

# pack <app> <version> <triple> <out>: SQLer.app + an Applications symlink into <out>/sqler-app-<triple>.dmg.
pack() {
    src=$1
    version=$2
    triple=$3
    out=$4
    [ -d "$src" ] || usage
    [ -n "$version" ] && [ -n "$triple" ] || usage
    mkdir -p "$out"
    pstage=$(mktemp -d)
    # ditto keeps the signature, the stapled ticket and the extended attributes of a signed app.
    ditto "$src" "$pstage/SQLer.app" || {
        rm -rf "$pstage"
        fail "cannot copy $src"
    }
    ln -s /Applications "$pstage/Applications"

    dmg="$out/sqler-app-$triple.dmg"
    n=0
    until hdiutil create -volname "SQLer $version" -srcfolder "$pstage" -ov -format UDZO "$dmg"; do
        n=$((n + 1))
        if [ "$n" -ge 3 ]; then
            rm -rf "$pstage"
            fail "hdiutil create failed 3 times"
        fi
        echo "hdiutil create failed, retrying ($n/3)" >&2
        sleep 5
    done
    rm -rf "$pstage"

    (cd "$out" && shasum -a 256 "sqler-app-$triple.dmg" >"sqler-app-$triple.dmg.sha256")
    echo "built $dmg"
}

mode=default
case ${1:-} in
--app)
    mode=app
    shift
    ;;
--pack)
    mode=pack
    shift
    ;;
esac

case $mode in
app)
    [ $# -eq 3 ] || usage
    stage_app "$1" "$2" "$3"
    ;;
pack)
    [ $# -eq 4 ] || usage
    pack "$1" "$2" "$3" "$4"
    ;;
*)
    [ $# -eq 4 ] || usage
    [ -n "$3" ] || usage
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    stage_app "$1" "$2" "$tmp/stage"
    codesign --force --deep --sign - "$tmp/stage/SQLer.app" || fail "ad-hoc codesign failed"
    pack "$tmp/stage/SQLer.app" "$2" "$3" "$4"
    ;;
esac
