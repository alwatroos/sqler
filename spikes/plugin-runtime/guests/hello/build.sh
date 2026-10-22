#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build the fixture (wit-bindgen variant) for wasm32-wasip2.
# Usage: build.sh <outdir>   -> <outdir>/hello.wasm   (needs the wasm32-wasip2 target)
# Prints (and appends to <outdir>/toolchain.txt): toolchain_rustc toolchain_wit_bindgen hello_wasm_bytes
set -eu
[ $# -eq 1 ] || { echo "usage: $0 <outdir>" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$1"
out=$(cd "$1" && pwd)
cargo build --release --locked --manifest-path "$here/Cargo.toml" --target wasm32-wasip2
cp "$here/target/wasm32-wasip2/release/hello.wasm" "$out/hello.wasm"
wb=$(sed -n '/^name = "wit-bindgen"$/{n;s/^version = "\(.*\)"$/\1/p;}' "$here/Cargo.lock" | head -n 1)
{
    echo "SPIKE toolchain_rustc=$(rustc --version | tr -d '\r' | awk '{print $2}')"
    echo "SPIKE toolchain_wit_bindgen=$wb"
    echo "SPIKE hello_wasm_bytes=$(wc -c < "$out/hello.wasm" | tr -d ' ')"
} | tee -a "$out/toolchain.txt"
