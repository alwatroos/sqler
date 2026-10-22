#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build the fixture with `cargo component build` (toolchain comparison).
# Usage: build.sh <outdir>   -> <outdir>/hello-cc.wasm, or <outdir>/hello-cc.build.log on failure.
# cargo-component is installed with `--root <outdir>/.tools`, never into ~/.cargo.
# Exits 0 in both cases; the outcome is in the keys (also appended to <outdir>/toolchain.txt):
#   SPIKE toolchain_cargo_component=<version> hello_cc_build=ok|fail hello_cc_wasm_bytes=<n>
set -eu

[ $# -eq 1 ] || { echo "usage: $0 <outdir>" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$1"
outdir=$(cd "$1" && pwd)
version=0.21.1
log="$outdir/hello-cc.build.log"
keys="$outdir/toolchain.txt"
touch "$keys"
emit() { echo "SPIKE $1" | tee -a "$keys"; }

fail() {
    emit "hello_cc_build=fail"
    emit "hello_cc_error=$(grep -m1 -i 'error' "$log" | tr -d '\r' | cut -c1-200)"
    exit 0
}

: >"$log"
export PATH="$outdir/.tools/bin:$PATH"
if ! cargo install cargo-component --version "$version" --locked --root "$outdir/.tools" >>"$log" 2>&1; then
    fail
fi
emit "toolchain_cargo_component=$(cargo component --version | tr -d '\r' | awk '{print $2}')"
if ! (cd "$here" && cargo component build --release --locked >>"$log" 2>&1); then
    fail
fi
cp "$here/target/wasm32-wasip1/release/hello_cc.wasm" "$outdir/hello-cc.wasm" 2>/dev/null ||
    cp "$here/target/wasm32-wasip2/release/hello_cc.wasm" "$outdir/hello-cc.wasm" || fail
emit "hello_cc_wit_bindgen=$(sed -n '1s/.*`wit-bindgen` \([0-9]*\.[0-9]*\.[0-9]*\).*/\1/p' "$here/src/bindings.rs")"
emit "hello_cc_build=ok"
emit "hello_cc_wasm_bytes=$(wc -c <"$outdir/hello-cc.wasm" | tr -d ' ')"
