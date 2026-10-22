#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Starts the SQL Server container, runs the tiberius guest under the spike host and appends the keys to
# <outdir>/results.txt. The container is always removed (trap).
# Usage: run.sh <host-bin> <guests-dir> <outdir>
#   <guests-dir>/mssql.wasm (plain build) or mssql-stub.wasm (rustls-native-certs stub, see RESULT.md).
# Keys: guest_mssql_select1 (1|fail), guest_mssql_tls (verified|trust-only|none),
#       guest_mssql_denied_ok (1|0), guest_mssql_variant (plain|stub), guest_mssql_error on failure.
set -u

[ $# -eq 3 ] || { echo "usage: $0 <host-bin> <guests-dir> <outdir>" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
host=$1
mkdir -p "$3"
out=$(cd "$3" && pwd)
res="$out/results.txt"
log="$out/mssql.run.log"
variant=plain
wasm=$2/mssql.wasm
if [ ! -f "$wasm" ]; then
    variant=stub
    wasm=$2/mssql-stub.wasm
fi

fail() {
    echo "SPIKE guest_mssql_select1=fail" >>"$res"
    echo "SPIKE guest_mssql_error=$1" >>"$res"
    exit 0
}

[ -f "$wasm" ] || fail "no mssql wasm (the build failed or was skipped)"
command -v docker >/dev/null 2>&1 || fail "docker not available"

trap 'docker rm -f sqler-spike-mssql >/dev/null 2>&1 || true' EXIT INT TERM
if ! sh "$here/start-mssql.sh" "$out/mssql-env" >"$out/mssql.env" 2>"$out/mssql.start.log"; then
    fail "container did not start (see mssql.start.log)"
fi
# shellcheck disable=SC1091
. "$out/mssql.env"
# The guest gets a read-only view of the CA certificate only (not the CA key).
mkdir -p "$out/ca-only"
cp "$SPIKE_MSSQL_CA" "$out/ca-only/ca.crt"

"$host" run-command "$wasm" \
    --env "SPIKE_MSSQL_HOST=$SPIKE_MSSQL_HOST" --env "SPIKE_MSSQL_PORT=$SPIKE_MSSQL_PORT" \
    --env "SPIKE_MSSQL_USER=$SPIKE_MSSQL_USER" --env "SPIKE_MSSQL_PASSWORD=$SPIKE_MSSQL_PASSWORD" \
    --env "SPIKE_MSSQL_CA=/ca/ca.crt" --dir "$out/ca-only::/ca" \
    --allow-tcp "$SPIKE_MSSQL_HOST:$SPIKE_MSSQL_PORT" >"$log" 2>&1
status=$?

for k in guest_mssql_tls guest_mssql_denied_ok; do
    v=$(sed -n "s/^SPIKE $k=//p" "$log" | tr -d '\r' | tail -n 1)
    echo "SPIKE $k=${v:-n/a}" >>"$res"
done
echo "SPIKE guest_mssql_variant=$variant" >>"$res"
v=$(sed -n 's/^SPIKE guest_mssql_select1=//p' "$log" | tr -d '\r' | tail -n 1)
if [ "$status" -eq 0 ] && [ "$v" = 1 ]; then
    echo "SPIKE guest_mssql_select1=1" >>"$res"
else
    err=$(grep -m 1 -E '^(SPIKE error=|mssql error:|error:)' "$log" | tr -d '\r' | sed 's/^SPIKE error=//; s/^SPIKE //' | cut -c1-200)
    echo "SPIKE guest_mssql_select1=fail" >>"$res"
    echo "SPIKE guest_mssql_error=${err:-exit status $status}" >>"$res"
fi
exit 0
