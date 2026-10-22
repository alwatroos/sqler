#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Builds the DuckDB guest for wasm32-wasip2 with the pinned wasi-sdk clang/clang++.
# Usage: build.sh <outdir>   -> <outdir>/duckdb.wasm, or <outdir>/duckdb.build.log + guest_duckdb_build=fail
# Exits 0 in both cases (the outcome is in the SPIKE keys, appended to <outdir>/guests.txt).
# Env: WASI_SDK_PATH (default: installs the pinned SDK under <outdir>/.tools/wasi-sdk),
#      SPIKE_EXTRA_CFLAGS / SPIKE_EXTRA_CXXFLAGS (build configuration experiments, see RESULT.md).
set -u

[ $# -eq 1 ] || { echo "usage: $0 <outdir>" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$1"
out=$(cd "$1" && pwd)
log="$out/duckdb.build.log"
keys="$out/guests.txt"

if [ -z "${WASI_SDK_PATH:-}" ]; then
    WASI_SDK_PATH=$(sh "$here/../../scripts/install-wasi-sdk.sh" "$out/.tools/wasi-sdk" | sed -n 's/^WASI_SDK_PATH=//p')
fi
sdk=$WASI_SDK_PATH
sysroot=$sdk/share/wasi-sysroot
# Configuration 3 (see RESULT.md): wasm exceptions, emulated mman, no DuckDB threads/printing, shims.
cfg="-fwasm-exceptions -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_SIGNAL"
cfg="$cfg -D_WASI_EMULATED_GETPID -DDUCKDB_NO_THREADS -DDUCKDB_DISABLE_PRINT"
cfg="$cfg -I$here/shim -include $here/shim/wasi-compat.h"
cflags="--target=wasm32-wasip2 --sysroot=$sysroot -O2 $cfg ${SPIKE_EXTRA_CFLAGS:-}"
cxxflags="$cflags ${SPIKE_EXTRA_CXXFLAGS:-}"

{
    echo "rustc: $(rustc --version)"
    echo "wasi-sdk clang: $("$sdk/bin/clang" --version | head -n 1)"
    echo "CFLAGS_wasm32_wasip2=$cflags"
    echo "CXXFLAGS_wasm32_wasip2=$cxxflags"
} >"$log"

RUSTFLAGS="-C link-arg=-L$sysroot/lib/wasm32-wasip2/eh -C link-arg=-L$sysroot/lib/wasm32-wasip2 -C link-arg=-lc++abi -C link-arg=-lunwind -C link-arg=-lwasi-emulated-mman -C link-arg=-lwasi-emulated-process-clocks -C link-arg=-lwasi-emulated-signal -C link-arg=-lwasi-emulated-getpid ${SPIKE_EXTRA_RUSTFLAGS:-}" \
CC_wasm32_wasip2="$sdk/bin/clang" \
CXX_wasm32_wasip2="$sdk/bin/clang++" \
AR_wasm32_wasip2="$sdk/bin/llvm-ar" \
CFLAGS_wasm32_wasip2="$cflags" \
CXXFLAGS_wasm32_wasip2="$cxxflags" \
    cargo build --release --locked --manifest-path "$here/Cargo.toml" --target wasm32-wasip2 >>"$log" 2>&1
status=$?

if [ "$status" -eq 0 ] && [ -f "$here/target/wasm32-wasip2/release/duckdb-guest.wasm" ]; then
    cp "$here/target/wasm32-wasip2/release/duckdb-guest.wasm" "$out/duckdb.wasm"
    rm -f "$log"
    {
        echo "SPIKE guest_duckdb_build=ok"
        echo "SPIKE guest_duckdb_wasm_bytes=$(wc -c <"$out/duckdb.wasm" | tr -d ' ')"
    } | tee -a "$keys"
else
    err=$(grep -m 1 -E 'error[:\[]' "$log" | tr -d '\r' | sed 's|/[^ ]*/out/duckdb/||' | cut -c1-200)
    {
        echo "SPIKE guest_duckdb_build=fail"
        echo "SPIKE guest_duckdb_error=$err"
    } | tee -a "$keys"
fi
exit 0
