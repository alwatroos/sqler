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
for p in ./usr/bin/sqler ./usr/share/applications/sqler.desktop \
    ./usr/share/doc/sqler/copyright ./usr/share/doc/sqler/NOTICE \
    ./usr/share/doc/sqler/THIRD-PARTY-LICENSES.txt ./usr/share/doc/sqler/licenses/LUCIDE-ISC.txt; do
    printf '%s\n' "$contents" | awk '{print $NF}' | grep -Fxq "$p" || fail "$p is not in the package"
done

sudo dpkg -i "$deb" || fail "dpkg -i failed"
[ "$(command -v sqler)" = "/usr/bin/sqler" ] || fail "sqler is not /usr/bin/sqler on the PATH"
ver=$(sqler --version 2>&1) || true
echo "$ver"
printf '%s\n' "$ver" | grep -Eq '^sqler [0-9]' || fail "--version does not match ^sqler [0-9]"

sudo dpkg -r sqler || fail "dpkg -r failed"
[ ! -e /usr/bin/sqler ] || fail "/usr/bin/sqler is still there after removal"
echo "check-deb: ok ($deb)"
