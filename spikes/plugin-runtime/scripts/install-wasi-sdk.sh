#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Installs the pinned wasi-sdk (clang + wasi-libc sysroot) into <dir> and prints WASI_SDK_PATH=<dir>.
# Usage: install-wasi-sdk.sh <dir>     (never installs globally; <dir> is created if missing)
# The release asset is verified against a pinned sha256 (GitHub release wasi-sdk-34 digests).
set -eu

[ $# -eq 1 ] || { echo "usage: $0 <dir>" >&2; exit 2; }
ver=34
mkdir -p "$1"
dir=$(cd "$1" && pwd)

arch=$(uname -m)
case "$(uname -s)-$arch" in
    Linux-x86_64) asset=x86_64-linux; sum=b761e3a0721dbae9c09a0059e5fdb2bf917d1b4a8a7b430fb3b5aafb0984b2c4 ;;
    Linux-aarch64 | Linux-arm64) asset=arm64-linux; sum=f7e243dff54d60bcc576e94d6166b69f410f2500ae4a9ceef34315be10e77971 ;;
    Darwin-arm64) asset=arm64-macos; sum=9c59398106b417f8f14913380fdf0097a8cc0ff4af9eb3ce0065a859e88d49e9 ;;
    Darwin-x86_64) asset=x86_64-macos; sum=87d27fa8adc68dee59bfbf2e22a6d34ef717c34d6bf1d8af2a56fc929d9ce0eb ;;
    *) echo "install-wasi-sdk.sh: unsupported platform $(uname -s)-$arch" >&2; exit 1 ;;
esac

name="wasi-sdk-$ver.0-$asset"
if [ ! -x "$dir/$name/bin/clang" ]; then
    tgz="$dir/$name.tar.gz"
    curl -fsSL --retry 3 -o "$tgz" \
        "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-$ver/$name.tar.gz"
    got=$( (sha256sum "$tgz" 2>/dev/null || shasum -a 256 "$tgz") | awk '{print $1}')
    if [ "$got" != "$sum" ]; then
        echo "install-wasi-sdk.sh: sha256 mismatch for $name.tar.gz (got $got, want $sum)" >&2
        rm -f "$tgz"
        exit 1
    fi
    tar -xzf "$tgz" -C "$dir"
    rm -f "$tgz"
fi
echo "WASI_SDK_PATH=$dir/$name"
