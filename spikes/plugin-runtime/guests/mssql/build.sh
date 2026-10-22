#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Builds the tiberius guest for wasm32-wasip2 with the pinned wasi-sdk clang for the C part of the
# crypto crates (ring / aws-lc-sys).
# Usage: build.sh <outdir>   -> <outdir>/mssql.wasm, or <outdir>/mssql.build.log + guest_mssql_build=fail
# Exits 0 in both cases (the outcome is in the SPIKE keys, appended to <outdir>/guests.txt).
# Env: WASI_SDK_PATH (default: installs the pinned SDK under <outdir>/.tools/wasi-sdk),
#      SPIKE_EXTRA_CFLAGS / SPIKE_EXTRA_RUSTFLAGS / SPIKE_CARGO_FLAGS, e.g. --no-default-features (build configuration experiments, see RESULT.md).
set -u

[ $# -eq 1 ] || { echo "usage: $0 <outdir>" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$1"
out=$(cd "$1" && pwd)
log="$out/mssql.build.log"
keys="$out/guests.txt"

if [ -z "${WASI_SDK_PATH:-}" ]; then
    WASI_SDK_PATH=$(sh "$here/../../scripts/install-wasi-sdk.sh" "$out/.tools/wasi-sdk" | sed -n 's/^WASI_SDK_PATH=//p')
fi
sdk=$WASI_SDK_PATH
sysroot=$sdk/share/wasi-sysroot
cflags="--target=wasm32-wasip2 --sysroot=$sysroot -O2 ${SPIKE_EXTRA_CFLAGS:-}"

{
    echo "rustc: $(rustc --version)"
    echo "wasi-sdk clang: $("$sdk/bin/clang" --version | head -n 1)"
    echo "CFLAGS_wasm32_wasip2=$cflags"
} >"$log"

RUSTFLAGS="${SPIKE_EXTRA_RUSTFLAGS:-}" \
CC_wasm32_wasip2="$sdk/bin/clang" \
AR_wasm32_wasip2="$sdk/bin/llvm-ar" \
CFLAGS_wasm32_wasip2="$cflags" \
    cargo build --release ${SPIKE_CARGO_LOCKED---locked} --manifest-path "$here/Cargo.toml" --target wasm32-wasip2 ${SPIKE_CARGO_FLAGS:-} >>"$log" 2>&1
status=$?

if [ "$status" -eq 0 ] && [ -f "$here/target/wasm32-wasip2/release/mssql-guest.wasm" ]; then
    cp "$here/target/wasm32-wasip2/release/mssql-guest.wasm" "$out/mssql.wasm"
    rm -f "$log"
    {
        echo "SPIKE guest_mssql_build=ok"
        echo "SPIKE guest_mssql_wasm_bytes=$(wc -c <"$out/mssql.wasm" | tr -d ' ')"
    } | tee -a "$keys"
else
    err=$(grep -m 1 -E 'error[:\[]' "$log" | tr -d '\r' | cut -c1-200)
    {
        echo "SPIKE guest_mssql_build=fail"
        echo "SPIKE guest_mssql_error=$err"
    } | tee -a "$keys"
fi

# Experiment (RESULT.md, configuration 3): the same guest with `rustls-native-certs` replaced by a stub
# crate (it has no wasm32-wasip2 backend). Never replaces a plain result; written as mssql-stub.wasm.
if [ ! -f "$out/mssql.wasm" ]; then
    cp "$here/Cargo.lock" "$out/Cargo.lock.orig"
    CC_wasm32_wasip2="$sdk/bin/clang" AR_wasm32_wasip2="$sdk/bin/llvm-ar" CFLAGS_wasm32_wasip2="$cflags" \
        cargo build --release --manifest-path "$here/Cargo.toml" --target wasm32-wasip2 \
        --config "patch.crates-io.rustls-native-certs.path=\"$here/experiments/native-certs-stub\"" \
        >"$out/mssql-stub.build.log" 2>&1
    stub_status=$?
    cp "$out/Cargo.lock.orig" "$here/Cargo.lock"
    rm -f "$out/Cargo.lock.orig"
    if [ "$stub_status" -eq 0 ] && [ -f "$here/target/wasm32-wasip2/release/mssql-guest.wasm" ]; then
        cp "$here/target/wasm32-wasip2/release/mssql-guest.wasm" "$out/mssql-stub.wasm"
        rm -f "$out/mssql-stub.build.log"
        {
            echo "SPIKE guest_mssql_stub_build=ok"
            echo "SPIKE guest_mssql_stub_wasm_bytes=$(wc -c <"$out/mssql-stub.wasm" | tr -d ' ')"
        } | tee -a "$keys"
    else
        echo "SPIKE guest_mssql_stub_build=fail" | tee -a "$keys"
    fi
fi
exit 0
