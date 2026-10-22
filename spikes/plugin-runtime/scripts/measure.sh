#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build one host variant, then run the fixture and the bench against it.
# Usage: measure.sh <variant> <guests-dir> <outdir>
#   <guests-dir> holds hello.wasm (from guests/hello/build.sh).
#   Writes <outdir>/results.txt (one `SPIKE k=v` per line, LF only) and the host binary.
#   base only builds (no engine, nothing to run).
#   pulley-rt first builds `pulley` and precompiles hello.cwasm with it.
set -eu

[ $# -eq 3 ] || { echo "usage: $0 <variant> <guests-dir> <outdir>" >&2; exit 2; }
variant=$1
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$3"
guests=$(cd "$2" && pwd)
outdir=$(cd "$3" && pwd)
results="$outdir/results.txt"
: >"$results"

exe=
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN* | Windows*) exe=.exe ;; esac

# Keep only SPIKE lines, LF only.
keep() { tr -d '\r' | grep '^SPIKE ' || true; }

fixture="$guests/hello.wasm"
if [ "$variant" = pulley-rt ]; then
    helper="$outdir/pulley-helper"
    sh "$here/build-host.sh" pulley "$helper" >/dev/null
    "$helper/plugin-spike-host-pulley$exe" precompile "$guests/hello.wasm" "$outdir/hello.cwasm" --pulley
    rm -rf "$helper"
    fixture="$outdir/hello.cwasm"
fi

sh "$here/build-host.sh" "$variant" "$outdir" | keep >>"$results"
if grep -q '^SPIKE winch=n/a' "$results"; then
    cat "$results"
    exit 0
fi

if [ "$variant" = base ]; then
    # No engine: the build keys (binary size) are all there is to measure.
    cat "$results"
    exit 0
fi

bin="$outdir/plugin-spike-host-$variant$exe"
status=0
"$bin" run-fixture "$fixture" | keep >>"$results" || status=1
if [ "$status" -eq 0 ]; then
    "$bin" bench "$fixture" | keep >>"$results" || status=1
fi
cat "$results"
exit "$status"
