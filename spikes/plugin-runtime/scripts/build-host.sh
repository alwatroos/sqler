#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build one host variant in release mode and print its build keys.
# Usage: build-host.sh <variant> <outdir>
#   variant: base | cl | cl-default | pulley | pulley-rt | winch
#   Copies plugin-spike-host-<variant>[.exe] to <outdir>.
#   Prints: SPIKE variant os arch wasmtime_version features binary_bytes
#   winch: when it does not build, prints `SPIKE winch=n/a reason=<text>` and exits 0.
set -eu

[ $# -eq 2 ] || { echo "usage: $0 <variant> <outdir>" >&2; exit 2; }
variant=$1
case "$variant" in
    base | cl | cl-default | pulley | pulley-rt | winch) ;;
    *) echo "unknown variant: $variant" >&2; exit 2 ;;
esac
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
mkdir -p "$2"
outdir=$(cd "$2" && pwd)

case "$(uname -s)" in
    Linux*) os=linux ;;
    Darwin*) os=macos ;;
    MINGW* | MSYS* | CYGWIN* | Windows*) os=windows ;;
    *) os=unknown ;;
esac
arch=$(uname -m)
case "$arch" in
    arm64 | aarch64) arch=aarch64 ;;
    amd64 | x86_64 | AMD64) arch=x86_64 ;;
esac
exe=
[ "$os" = windows ] && exe=.exe

log="$outdir/build-$variant.log"
if [ "$variant" = base ]; then
    feat=
else
    feat="--features $variant"
fi
# shellcheck disable=SC2086
if ! cargo build --release --locked --no-default-features $feat \
    --manifest-path "$root/Cargo.toml" -p plugin-spike-host >"$log" 2>&1; then
    if [ "$variant" = winch ]; then
        reason=$(grep -m1 '^error' "$log" | tr -d '\r' | tr '\n' ' ')
        echo "SPIKE winch=n/a reason=${reason:-build failed}"
        exit 0
    fi
    tail -n 40 "$log" >&2
    exit 1
fi

bin="$outdir/plugin-spike-host-$variant$exe"
cp "$root/target/release/plugin-spike-host$exe" "$bin"
info=$("$bin" info | tr -d '\r')
getkey() { printf '%s\n' "$info" | sed -n "s/^SPIKE $1=//p"; }
echo "SPIKE variant=$(getkey variant)"
echo "SPIKE os=$os"
echo "SPIKE arch=$arch"
echo "SPIKE wasmtime_version=$(getkey wasmtime_version)"
echo "SPIKE features=$(getkey features)"
echo "SPIKE binary_bytes=$(wc -c <"$bin" | tr -d ' ')"
