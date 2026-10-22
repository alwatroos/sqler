#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Runs the DuckDB guest under the spike host and appends its keys to <outdir>/results.txt.
# Usage: run.sh <host-bin> <guests-dir> <outdir>      (<guests-dir>/duckdb.wasm comes from build.sh)
# Keys: guest_duckdb_select1 (1|fail) and, on failure, guest_duckdb_error (first error line).
set -u

[ $# -eq 3 ] || { echo "usage: $0 <host-bin> <guests-dir> <outdir>" >&2; exit 2; }
host=$1
wasm=$2/duckdb.wasm
mkdir -p "$3"
res="$3/results.txt"
log="$3/duckdb.run.log"

if [ ! -f "$wasm" ]; then
    echo "SPIKE guest_duckdb_select1=fail" >>"$res"
    echo "SPIKE guest_duckdb_error=no duckdb.wasm (the build failed or was skipped)" >>"$res"
    exit 0
fi

"$host" run-command "$wasm" >"$log" 2>&1
status=$?
v=$(sed -n 's/^SPIKE guest_duckdb_select1=//p' "$log" | tr -d '\r' | tail -n 1)
if [ "$status" -eq 0 ] && [ "$v" = 1 ]; then
    echo "SPIKE guest_duckdb_select1=1" >>"$res"
else
    err=$(grep -m 1 -E '^(SPIKE error=|duckdb error:|error:)' "$log" | tr -d '\r' | sed 's/^SPIKE error=//; s/^SPIKE //' | cut -c1-200)
    echo "SPIKE guest_duckdb_select1=fail" >>"$res"
    echo "SPIKE guest_duckdb_error=${err:-exit status $status}" >>"$res"
fi
exit 0
