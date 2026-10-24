#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Generates THIRD-PARTY-LICENSES.txt for the release feature set (R14, ADR-0064 amendment 2).
#
#   third-party-licenses.sh <out-file>
#
# The feature list is the one `features` line of dist-workspace.toml (the same rules as release-guard.sh), the targets
# are its `targets` line. The file holds the cargo-about report (about.toml, about.hbs), then the NOTICE files of every
# shipped crate, then the verbatim licence files of the crates that bundle C code. It fails when it mentions DuckDB.
#
# Exit 0: ok (stdout `third-party-licenses: ok (<n> crates, <m> notice files) -> <out-file>`).
# Exit 1: a violation (driver-duckdb in the list, cargo-about --fail, duckdb in the output).
# Exit 2: usage or parse error (no or duplicate features line, a missing tool: cargo, cargo-about, jq).
# `SQLER_RELEASE_FEATURES` (comma list) replaces the parsed feature list; the negative test uses it
# (`SQLER_RELEASE_FEATURES=driver-duckdb` must exit 1).
set -eu

config=dist-workspace.toml
app_manifest=crates/sqler-app/Cargo.toml
# Crates that bundle C code: their licence files are reproduced verbatim.
verbatim='aws-lc-sys ring'

fail() {
    echo "third-party-licenses: $1" >&2
    exit 1
}

die() {
    echo "third-party-licenses: $1" >&2
    exit 2
}

[ $# -eq 1 ] || die "usage: third-party-licenses.sh <out-file>"
out=$1
[ -f "$config" ] || die "$config not found (run from the repository root)"
for tool in cargo cargo-about jq; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool not found"
done

# One `key = [ ... ]` line of the config as a comma-separated list.
read_list() {
    lines=$(grep -c -E "^[[:space:]]*$1[[:space:]]*=" "$config" || true)
    [ "$lines" -eq 1 ] || die "$config must hold exactly one $1 line, found $lines"
    line=$(grep -E "^[[:space:]]*$1[[:space:]]*=" "$config" | tr -d '\r' | sed 's/#.*//')
    case $line in
    *\[*\]*) ;;
    *) die "the $1 list in $config must open and close on one line" ;;
    esac
    printf '%s' "$line" | sed 's/^[^[]*\[//; s/\].*$//' | tr -d '" ' | tr -s ',' ','
}

if [ "${SQLER_RELEASE_FEATURES+set}" = set ]; then
    csv=$(printf '%s' "$SQLER_RELEASE_FEATURES" | tr -d '\r ')
else
    csv=$(read_list features)
fi
targets=$(read_list targets | tr ',' ' ')
[ -n "$targets" ] || die "no targets in $config"

# Same rule as release-guard.sh: the DuckDB engine never ships.
case ",$csv," in
*,driver-duckdb,*) fail "the release feature list contains driver-duckdb" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cargo fetch --locked || die "cargo fetch failed"
echo "third-party-licenses: generating for features: ${csv:-none}" >&2
if [ -n "$csv" ]; then
    set -- --features "$csv"
else
    set --
fi
cargo about generate --manifest-path "$app_manifest" "$@" -c about.toml --fail --offline -o "$tmp/about.txt" about.hbs ||
    fail "cargo about failed"
{
    echo "Features: ${csv:-none}"
    echo "Targets: $targets"
    echo
    cat "$tmp/about.txt"
} >"$out"

# The shipped crates: the union over all release targets (name and version, one per line).
: >"$tmp/tree.txt"
for t in $targets; do
    cargo tree -p sqler-app --locked -e normal --target "$t" "$@" --prefix none --format '{p}' >>"$tmp/tree.txt" ||
        die "cargo tree failed for $t"
done
sed 's/ (.*//; s/ *$//' "$tmp/tree.txt" | tr -d '\r' | sort -u >"$tmp/crates.txt"
crates=$(wc -l <"$tmp/crates.txt" | tr -d ' ')

# name, version and manifest directory of every package.
cargo metadata --locked --format-version 1 >"$tmp/metadata.json" || die "cargo metadata failed"
jq -r '.packages[] | [.name, .version, .manifest_path] | @tsv' "$tmp/metadata.json" | tr -d '\r' >"$tmp/packages.tsv"

dir_of() {
    # $1 = name, $2 = version
    awk -F '\t' -v n="$1" -v v="$2" '$1 == n && $2 == v { print $3; exit }' "$tmp/packages.tsv" | sed 's|/Cargo.toml$||'
}

notices=0
{
    echo
    echo "== NOTICE files =="
    echo "NOTICE files shipped by the third-party crates above."
} >>"$out"
while IFS= read -r line; do
    name=${line%% v*}
    version=${line##* v}
    dir=$(dir_of "$name" "$version")
    [ -n "$dir" ] || continue
    [ -d "$dir" ] || continue
    for f in "$dir"/*; do
        [ -f "$f" ] || continue
        base=$(basename "$f")
        case $(printf '%s' "$base" | tr '[:upper:]' '[:lower:]') in
        notice*)
            notices=$((notices + 1))
            {
                echo
                echo "-- $name $version ($base) --"
                cat "$f"
            } >>"$out"
            ;;
        esac
    done
done <"$tmp/crates.txt"

{
    echo
    echo "== Licence files of bundled C code =="
} >>"$out"
for name in $verbatim; do
    found=0
    while IFS= read -r line; do
        n=${line%% v*}
        [ "$n" = "$name" ] || continue
        version=${line##* v}
        dir=$(dir_of "$n" "$version")
        for f in "$dir"/LICENSE* "$dir"/aws-lc/third_party/*/LICENSE* "$dir"/third_party/*/LICENSE*; do
            [ -f "$f" ] || continue
            found=1
            {
                echo
                echo "-- $n $version (${f#"$dir"/}) --"
                cat "$f"
            } >>"$out"
        done
    done <"$tmp/crates.txt"
    [ "$found" -eq 1 ] || fail "no licence file found for $name (is it in the release tree?)"
done

if grep -i -q duckdb "$out"; then
    fail "$out mentions duckdb"
fi
echo "third-party-licenses: ok ($crates crates, $notices notice files) -> $out"
