#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# The release guard (ADR-0064, ADR-0070): no 1.0 download may contain the DuckDB engine.
#
#   release-guard.sh features            dist config (dist-workspace.toml and [package.metadata.dist] of sqler-app) and the resolved dependency tree of the release build
#   release-guard.sh binary <file>       the binary holds no DuckDB engine marker
#   release-guard.sh build-dir <dir>     the cargo build dir holds no libduckdb-sys build output
#   release-guard.sh artifacts <dir>     no DuckDB file name, no DuckDB text in a text artifact
#
# Exit 0: pass. Exit 1: a violation (one `release-guard: <reason>` line per finding on stderr).
# Exit 2: usage or parse error. A pass prints `release-guard: <mode> ok (<evidence>)` on stdout.
# `SQLER_RELEASE_FEATURES` (comma list) replaces the feature list parsed from dist-workspace.toml; the negative
# test uses it (`SQLER_RELEASE_FEATURES=driver-duckdb` must exit 1).
set -eu

config=dist-workspace.toml
app_manifest=crates/sqler-app/Cargo.toml

# Strings of the DuckDB C++ engine only (verified absent from the default sqler binary). A plain `duckdb` is wrong:
# the UI text of the default binary names DuckDB. One marker per line (a marker may hold spaces).
markers='Could not set lock on file
preserve_insertion_order
checkpoint_threshold
enable_object_cache'

failures=0

fail() {
    echo "release-guard: $1" >&2
    failures=$((failures + 1))
}

usage() {
    echo "usage: release-guard.sh features | binary <file> | build-dir <dir> | artifacts <dir>" >&2
    exit 2
}

finish() {
    # $1 = mode, $2 = evidence of a pass
    if [ "$failures" -ne 0 ]; then
        exit 1
    fi
    echo "release-guard: $1 ok ($2)"
    exit 0
}

mode_features() {
    [ -f "$config" ] || {
        echo "release-guard: $config not found (run from the repository root)" >&2
        exit 2
    }
    if grep -E '^[[:space:]]*all-features[[:space:]]*=[[:space:]]*true' "$config" >/dev/null; then
        fail "$config sets all-features = true"
    fi

    if [ "${SQLER_RELEASE_FEATURES+set}" = set ]; then
        list=$(printf '%s' "$SQLER_RELEASE_FEATURES" | tr -d '\r' | tr ',' ' ')
        source_of="SQLER_RELEASE_FEATURES"
    else
        lines=$(grep -c -E '^[[:space:]]*features[[:space:]]*=' "$config" || true)
        if [ "$lines" -ne 1 ]; then
            echo "release-guard: $config must hold exactly one features line, found $lines" >&2
            exit 2
        fi
        line=$(grep -E '^[[:space:]]*features[[:space:]]*=' "$config" | tr -d '\r' | sed 's/#.*//')
        case $line in
        *\[*\]*) ;;
        *)
            echo "release-guard: the features list in $config must open and close on one line" >&2
            exit 2
            ;;
        esac
        list=$(printf '%s' "$line" | sed 's/^[^[]*\[//; s/\].*$//' | tr -d '" ' | tr ',' ' ')
        source_of="$config"
    fi

    # cargo-dist also accepts `features` and `all-features` under [package.metadata.dist] of the package manifest.
    if [ -f "$app_manifest" ]; then
        meta=$(awk '/^[[:space:]]*\[/ { s = ($0 ~ /^[[:space:]]*\[package\.metadata\.dist\]/) } s' "$app_manifest" | tr -d '\r' | sed 's/#.*//')
        if printf '%s\n' "$meta" | grep -E '^[[:space:]]*all-features[[:space:]]*=[[:space:]]*true' >/dev/null; then
            fail "$app_manifest sets all-features = true under [package.metadata.dist]"
        fi
        if printf '%s\n' "$meta" | grep -E '^[[:space:]]*features[[:space:]]*=' >/dev/null; then
            mline=$(printf '%s\n' "$meta" | grep -E '^[[:space:]]*features[[:space:]]*=')
            case $mline in
            *\[*\]*) ;;
            *)
                echo "release-guard: the features list in $app_manifest must open and close on one line" >&2
                exit 2
                ;;
            esac
            list="$list $(printf '%s' "$mline" | sed 's/^[^[]*\[//; s/\].*$//' | tr -d '" ' | tr ',' ' ')"
            source_of="$source_of and $app_manifest"
        fi
    fi

    csv=''
    for f in $list; do
        if [ "$f" = "driver-duckdb" ]; then
            fail "the release feature list ($source_of) contains driver-duckdb"
        fi
        csv="${csv:+$csv,}$f"
    done

    # The resolved tree catches feature unification: a feature that pulls DuckDB in through another one.
    if [ -n "$csv" ]; then
        tree=$(cargo tree -p sqler-app --target all --locked -e normal,build --prefix none --features "$csv") || {
            echo "release-guard: cargo tree failed" >&2
            exit 2
        }
    else
        tree=$(cargo tree -p sqler-app --target all --locked -e normal,build --prefix none) || {
            echo "release-guard: cargo tree failed" >&2
            exit 2
        }
    fi
    hits=$(printf '%s\n' "$tree" | tr -d '\r' | grep -E '^(sqler-driver-duckdb|duckdb|libduckdb-sys) ' | sed 's/ (.*//' | sort -u || true)
    if [ -n "$hits" ]; then
        fail "the resolved dependency tree of sqler-app (features: ${csv:-default only}) holds: $(printf '%s' "$hits" | tr '\n' ';')"
    fi
    finish features "features: ${csv:-none, default only}; no sqler-driver-duckdb, duckdb or libduckdb-sys in the tree"
}

mode_binary() {
    [ $# -eq 1 ] || usage
    [ -f "$1" ] || {
        echo "release-guard: $1 is not a file" >&2
        exit 2
    }
    count=0
    # The marker list goes through a temp file so the loop runs in the current shell (POSIX sh, no here-strings).
    list=$(mktemp)
    trap 'rm -f "$list"' EXIT
    printf '%s\n' "$markers" >"$list"
    while IFS= read -r m; do
        [ -n "$m" ] || continue
        count=$((count + 1))
        if LC_ALL=C grep -a -F -q -- "$m" "$1"; then
            fail "$1 contains the DuckDB engine marker '$m'"
        fi
    done <"$list"
    finish binary "$count markers absent from $(wc -c <"$1" | tr -d ' ') bytes"
}

mode_build_dir() {
    [ $# -eq 1 ] || usage
    [ -d "$1" ] || {
        echo "release-guard: $1 is not a directory" >&2
        exit 2
    }
    found=0
    for d in "$1"/build/libduckdb-sys-*; do
        if [ -e "$d" ]; then
            found=$((found + 1))
            fail "$d exists: the DuckDB engine was compiled"
        fi
    done
    finish build-dir "no libduckdb-sys build output under $1/build"
}

mode_artifacts() {
    [ $# -eq 1 ] || usage
    [ -d "$1" ] || {
        echo "release-guard: $1 is not a directory" >&2
        exit 2
    }
    names=0
    texts=0
    list=$(mktemp)
    trap 'rm -f "$list"' EXIT
    find "$1" -type f >"$list"
    while IFS= read -r f; do
        names=$((names + 1))
        base=$(basename "$f")
        if printf '%s' "$base" | grep -i -q duckdb; then
            fail "artifact file name mentions duckdb: $f"
        fi
        case $base in
        *.sh | *.ps1 | *.json | *.xml | *.wxs | *.sum | *.sha256)
            texts=$((texts + 1))
            if grep -a -i -q duckdb "$f"; then
                fail "text artifact mentions duckdb: $f"
            fi
            ;;
        esac
    done <"$list"
    finish artifacts "$names files, $texts text artifacts checked"
}

[ $# -ge 1 ] || usage
mode=$1
shift
case $mode in
features) mode_features ;;
binary) mode_binary "$@" ;;
build-dir) mode_build_dir "$@" ;;
artifacts) mode_artifacts "$@" ;;
*) usage ;;
esac
