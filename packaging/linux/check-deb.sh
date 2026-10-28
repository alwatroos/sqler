#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Checks a package built by build-deb.sh (ADR-0075): contents, then install, `sqler --version`, remove.
#
#   check-deb.sh <deb>
#
# INSTALLS ON THE HOST (sudo dpkg -i): run it on CI runners only, never on a workstation.
# Exit 0: ok. Exit 1: a failure (::error title=Installer::...). Exit 2: usage error.
set -eu

fail() {
    echo "::error title=Installer::$1"
    exit 1
}

[ $# -eq 1 ] && [ -f "$1" ] || {
    echo "usage: check-deb.sh <deb>" >&2
    exit 2
}
deb=$1

[ "$(dpkg-deb --field "$deb" Package)" = "sqler" ] || fail "Package field is not sqler"
contents=$(dpkg-deb --contents "$deb")
icons=""
for n in 16 22 24 32 48 64 128 256 512; do
    icons="$icons ./usr/share/icons/hicolor/${n}x${n}/apps/sqler.png"
done
for p in ./usr/bin/sqler ./usr/share/applications/sqler.desktop \
    $icons ./usr/share/icons/hicolor/scalable/apps/sqler.svg \
    ./usr/share/doc/sqler/copyright ./usr/share/doc/sqler/NOTICE \
    ./usr/share/doc/sqler/THIRD-PARTY-LICENSES.txt ./usr/share/doc/sqler/licenses/LUCIDE-ISC.txt; do
    printf '%s\n' "$contents" | awk '{print $NF}' | grep -Fxq "$p" || fail "$p is not in the package"
done
dpkg-deb --fsys-tarfile "$deb" | tar -xO ./usr/share/applications/sqler.desktop | grep -Fxq "Icon=sqler" ||
    fail "the packaged sqler.desktop has no Icon=sqler"

sudo dpkg -i "$deb" || fail "dpkg -i failed"
[ "$(command -v sqler)" = "/usr/bin/sqler" ] || fail "sqler is not /usr/bin/sqler on the PATH"
[ -e /usr/share/icons/hicolor/256x256/apps/sqler.png ] || fail "the 256x256 icon is not installed"
ver=$(sqler --version 2>&1) || true
echo "$ver"
printf '%s\n' "$ver" | grep -Eq '^sqler [0-9]' || fail "--version does not match ^sqler [0-9]"

sudo dpkg -r sqler || fail "dpkg -r failed"
[ ! -e /usr/bin/sqler ] || fail "/usr/bin/sqler is still there after removal"
echo "check-deb: ok ($deb)"
