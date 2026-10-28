#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Builds the Debian package (ADR-0075) from the extracted release archive, with dpkg-deb only.
#
#   build-deb.sh <bundle-dir> <version> <triple> <out-dir>
#
# <bundle-dir> holds `sqler`, LICENSE, NOTICE, THIRD-PARTY-LICENSES.txt and licenses/. Writes
# <out-dir>/sqler-app-<triple>.deb and its .sha256 sidecar. `SQLER_DEB_DEPENDS`, when set, replaces the
# dpkg-shlibdeps result (a local run without the libraries). Exit 0: ok. Exit 1: a failure
# (::error title=Installer::...). Exit 2: usage error (also an unknown triple).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

fail() {
    echo "::error title=Installer::$1"
    exit 1
}

usage() {
    echo "usage: build-deb.sh <bundle-dir> <version> <triple> <out-dir>" >&2
    exit 2
}

[ $# -eq 4 ] || usage
bundle=$1
version=$2
triple=$3
out=$4
[ -d "$bundle" ] && [ -n "$version" ] || usage
case $triple in
x86_64-*) arch=amd64 ;;
aarch64-*) arch=arm64 ;;
*) usage ;;
esac
[ -x "$bundle/sqler" ] || fail "$bundle/sqler is missing or not executable"
mkdir -p "$out"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
pkg="$stage/pkg"
doc="$pkg/usr/share/doc/sqler"
mkdir -p "$pkg/DEBIAN" "$pkg/usr/bin" "$pkg/usr/share/applications" "$doc/licenses"
install -m 755 "$bundle/sqler" "$pkg/usr/bin/sqler"
install -m 644 "$here/sqler.desktop" "$pkg/usr/share/applications/sqler.desktop"
# The app icon: hicolor PNGs plus the scalable SVG. dpkg's hicolor-icon-theme trigger refreshes the cache (no postinst).
for n in 16 22 24 32 48 64 128 256 512; do
    src="$root/assets/icon/png/sqler-$n.png"
    [ -s "$src" ] || fail "assets/icon/png/sqler-$n.png is missing"
    mkdir -p "$pkg/usr/share/icons/hicolor/${n}x${n}/apps"
    install -m 644 "$src" "$pkg/usr/share/icons/hicolor/${n}x${n}/apps/sqler.png"
done
[ -s "$root/assets/icon/sqler.svg" ] || fail "assets/icon/sqler.svg is missing"
mkdir -p "$pkg/usr/share/icons/hicolor/scalable/apps"
install -m 644 "$root/assets/icon/sqler.svg" "$pkg/usr/share/icons/hicolor/scalable/apps/sqler.svg"
for f in NOTICE THIRD-PARTY-LICENSES.txt; do
    [ -s "$bundle/$f" ] || fail "$f is missing from $bundle"
    install -m 644 "$bundle/$f" "$doc/$f"
done
[ -s "$bundle/LICENSE" ] || fail "LICENSE is missing from $bundle"
install -m 644 "$bundle/LICENSE" "$doc/copyright"
[ -s "$bundle/licenses/LUCIDE-ISC.txt" ] || fail "licenses/LUCIDE-ISC.txt is missing from $bundle"
install -m 644 "$bundle/licenses/LUCIDE-ISC.txt" "$doc/licenses/LUCIDE-ISC.txt"

sh "$root/.github/scripts/release-guard.sh" binary "$pkg/usr/bin/sqler"

if [ -n "${SQLER_DEB_DEPENDS+x}" ]; then
    depends=$SQLER_DEB_DEPENDS
else
    # dpkg-shlibdeps reads debian/control from the working directory.
    mkdir -p "$stage/shl/debian"
    printf 'Source: sqler\n\nPackage: sqler\nArchitecture: %s\n' "$arch" >"$stage/shl/debian/control"
    mkdir -p "$stage/shl/usr/bin"
    cp "$pkg/usr/bin/sqler" "$stage/shl/usr/bin/sqler"
    (cd "$stage/shl" && dpkg-shlibdeps -O -e usr/bin/sqler >"$stage/shlibdeps.out") ||
        fail "dpkg-shlibdeps failed"
    depends=$(sed -n 's/^shlibs:Depends=//p' "$stage/shlibdeps.out")
    [ -n "$depends" ] || fail "dpkg-shlibdeps produced no Depends"
fi

deb_version=$(printf '%s' "$version" | sed 's/-/~/')
{
    echo "Package: sqler"
    echo "Version: $deb_version"
    echo "Architecture: $arch"
    echo "Maintainer: The SQLer Authors"
    echo "Homepage: https://github.com/alwatroos/sqler"
    echo "Section: database"
    echo "Priority: optional"
    echo "Depends: $depends"
    echo "Recommends: libwayland-client0, libvulkan1, libfontconfig1, libfreetype6"
    echo "Description: Lightweight native SQL IDE"
    echo " SQLer is a native desktop SQL IDE with a plugin system and no embedded runtime."
} >"$pkg/DEBIAN/control"

sh "$root/.github/scripts/release-guard.sh" artifacts "$pkg"

deb="$out/sqler-app-$triple.deb"
dpkg-deb --build --root-owner-group "$pkg" "$deb" || fail "dpkg-deb --build failed"
(cd "$out" && sha256sum "sqler-app-$triple.deb" >"sqler-app-$triple.deb.sha256")
echo "built $deb"
