#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Fails, listing every tracked *.rs file (outside vendor/) whose first line is not the SPDX header.
set -eu

header='// SPDX-License-Identifier: Apache-2.0'
missing=''

# One path per line, read with IFS= so spaces survive (POSIX sh has no read -d).
# Reading from a file keeps the loop in the current shell.
list=$(mktemp)
trap 'rm -f "$list"' EXIT
# vendor/ holds third-party code that keeps its upstream licence files (vendor/*/README.md).
git ls-files '*.rs' ':!:vendor/' >"$list"

while IFS= read -r f; do
    first=$(head -n 1 "$f" | tr -d '\r')
    if [ "$first" != "$header" ]; then
        missing="$missing$f
"
    fi
done <"$list"

if [ -n "$missing" ]; then
    echo "Missing '$header' on line 1:" >&2
    printf '%s' "$missing"
    exit 1
fi
