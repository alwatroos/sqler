#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Cases for tag-release-precheck.sh (R18). No network. Prints "ok N cases" and exits 0, or the failing case and exits 1.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
pre=$here/tag-release-precheck.sh

nl='
'
cr=$(printf '\r')
long=1.0.0-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
n=0

# expect <pass|fail> <name> <ref> <version> <confirm>
expect() {
    want=$1 name=$2
    n=$((n + 1))
    if out=$(REF=$3 VERSION=$4 CONFIRM=$5 sh "$pre" 2>&1); then rc=0; else rc=$?; fi
    if [ "$want" = pass ]; then
        [ "$rc" -eq 0 ] || { printf 'FAIL %s: expected pass, got rc=%s: %s\n' "$name" "$rc" "$out"; exit 1; }
    else
        [ "$rc" -eq 1 ] || { printf 'FAIL %s: expected rc=1, got rc=%s\n' "$name" "$rc"; exit 1; }
        case $out in
            "::error::"?*) ;;
            *) printf 'FAIL %s: no error message: %s\n' "$name" "$out"; exit 1 ;;
        esac
    fi
}

main=refs/heads/main
expect pass valid "$main" 1.0.0 "release v1.0.0"
expect pass valid-prerelease "$main" 1.2.3-rc.1 "release v1.2.3-rc.1"
expect fail wrong-ref refs/heads/other 1.0.0 "release v1.0.0"
expect fail tag-ref refs/tags/v1.0.0 1.0.0 "release v1.0.0"
expect fail empty-ref "" 1.0.0 "release v1.0.0"
expect fail v-prefix "$main" v1.0.0 "release vv1.0.0"
expect fail two-part "$main" 1.0 "release v1.0"
expect fail injection "$main" '1.0.0;id' 'release v1.0.0;id'
expect fail empty-version "$main" "" "release v"
expect fail leading-zero "$main" 01.0.0 "release v01.0.0"
expect fail long-70 "$main" "$(printf '1.0.0-%064d' 0)" "release v$(printf '1.0.0-%064d' 0)"
expect fail too-long "$main" "$long$long" "release v$long$long"
expect fail newline "$main" "1.0.0${nl}x" "release v1.0.0${nl}x"
expect fail trailing-newline-via-literal "$main" "1.0.0${nl}" "release v1.0.0${nl}"
expect fail carriage-return "$main" "1.0.0${cr}" "release v1.0.0${cr}"
expect fail embedded-cr "$main" "1.0${cr}.0" "release v1.0${cr}.0"
expect fail wrong-confirm "$main" 1.0.0 "release v1.0.1"
expect fail empty-confirm "$main" 1.0.0 ""
expect fail confirm-no-v "$main" 1.0.0 "release 1.0.0"
expect fail confirm-other-version "$main" 1.0.0 "release v2.0.0"

# Regression: the literal-newline pattern must not match a plain single-line version.
expect pass plain-not-matched-as-newline "$main" 10.20.30 "release v10.20.30"

echo "ok $n cases"
