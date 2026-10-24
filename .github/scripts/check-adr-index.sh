#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Checks that every ADR file has a row in docs/src/adr/index.md and an entry in docs/src/SUMMARY.md, that every ADR
# link in either file points at an existing file, that the ADR links are in ascending order, and that every number
# without a file is named on the "Unused numbers:" line of the index.
# Prints one line per problem ("<file>: <problem>") and exits 1; prints nothing and exits 0 when clean.
set -eu

cd "$(git rev-parse --show-toplevel)"

adr=docs/src/adr
index=$adr/index.md
summary=docs/src/SUMMARY.md
problems=''

add() {
    problems="$problems$1: $2
"
}

# Index links look like ](NNNN-name.md); SUMMARY links look like ](adr/NNNN-name.md).
index_links=$(grep -o '](\([0-9]\{4\}\)-[^)]*\.md)' "$index" || true)
summary_links=$(grep -o '](adr/[0-9]\{4\}-[^)]*\.md)' "$summary" || true)

last=''
for f in "$adr"/[0-9][0-9][0-9][0-9]-*.md; do
    base=${f##*/}
    last=${base%%-*}
    if ! printf '%s\n' "$index_links" | grep -qxF "](${base})"; then
        add "$index" "no row for $base"
    fi
    if ! printf '%s\n' "$summary_links" | grep -qxF "](adr/${base})"; then
        add "$summary" "no entry for $base"
    fi
done

# Links that point nowhere.
for l in $index_links; do
    t=${l#\](}
    t=${t%\)}
    [ -f "$adr/$t" ] || add "$index" "link to missing file $t"
done
for l in $summary_links; do
    t=${l#\](adr/}
    t=${t%\)}
    [ -f "$adr/$t" ] || add "$summary" "link to missing file $t"
done

# Ascending order: the four-digit numbers are zero-padded, so a plain sort compares them numerically.
order_index=$(grep '^| \[[0-9]\{4\}\](' "$index" | grep -o '^| \[[0-9]\{4\}' | grep -o '[0-9]\{4\}' || true)
order_summary=$(printf '%s\n' "$summary_links" | grep -o '^](adr/\([0-9]\{4\}\)' | grep -o '[0-9]\{4\}' || true)
if ! printf '%s\n' "$order_index" | sort -c -u 2>/dev/null; then
    add "$index" "ADR rows are not in ascending order"
fi
if ! printf '%s\n' "$order_summary" | sort -c -u 2>/dev/null; then
    add "$summary" "ADR entries are not in ascending order"
fi

# Numbers without a file must be named on the "Unused numbers:" line.
unused_line=$(grep '^Unused numbers:' "$index" || true)
i=0
while [ -n "$last" ]; do
    n=$(printf '%04d' "$i")
    set -- "$adr/$n"-*.md
    if [ ! -f "$1" ]; then
        case $unused_line in
            *"$n"*) ;;
            *) add "$index" "number $n has no file and is not on the 'Unused numbers:' line" ;;
        esac
    fi
    [ "$n" = "$last" ] && break
    i=$((i + 1))
done

if [ -n "$problems" ]; then
    printf '%s' "$problems"
    exit 1
fi
