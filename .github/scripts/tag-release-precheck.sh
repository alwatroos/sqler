#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Cheap pre-check of the "Tag release" workflow (R18), run before anything is checked out. Fails closed: the first
# failed check prints "::error::<reason>" and exits 1. Inputs are environment variables only:
#   REF      github.ref, must be refs/heads/main
#   VERSION  e.g. 1.0.0 (no "v" prefix), single line, at most 64 chars, anchored semver
#   CONFIRM  must be exactly "release v<VERSION>"
# Newline and CR are matched against literal variables, never `$(printf '\n')`: command substitution strips trailing
# newlines, which would turn the pattern into `**` and match every value.
set -eu

: "${REF:=}" "${VERSION:=}" "${CONFIRM:=}"

nl='
'
cr=$(printf '\r')

fail() {
    printf '::error::%s\n' "$1"
    exit 1
}

[ "$REF" = "refs/heads/main" ] || fail "Tag release runs from refs/heads/main only"

case $VERSION in
    *"$nl"* | *"$cr"*) fail "version must not contain a newline or carriage return" ;;
esac

if [ "${#VERSION}" -gt 64 ] || ! printf '%s\n' "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'; then
    fail "version must be semver X.Y.Z[-pre], no v prefix"
fi

[ "$CONFIRM" = "release v$VERSION" ] || fail "confirm must be exactly: release v<version>"
