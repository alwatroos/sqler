#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Starts SQL Server 2022 with a TLS certificate and prints the spike environment as KEY=value lines on
# stdout (diagnostics go to stderr). Shape follows crates/sqler-driver-mysql/tests/docker/start-mysql.sh.
#
#   local: eval "$(sh start-mssql.sh results/mssql | sed 's/^/export /')"
#   remove: docker rm -f sqler-spike-mssql      (no named volumes are created)
#
# Usage: start-mssql.sh <outdir> [plain]
#   default: own CA + a server certificate for DNS:localhost only, TLS FORCED (forceencryption = 1).
#   plain:   no certificate, encryption not forced (only for the transport-only experiment in RESULT.md).
# Container sqler-spike-mssql on 127.0.0.1:14339. Idempotent: an earlier container is replaced.
# The image runs as amd64 (emulated on Apple Silicon). Needs docker and openssl.
set -eu

out=${1:?usage: start-mssql.sh <outdir> [plain]}
mode=${2:-tls}
name=sqler-spike-mssql
port=14339
# Tag verified live on mcr.microsoft.com (tags/list) on 2026-10-22.
image=mcr.microsoft.com/mssql/server:2022-CU27-ubuntu-22.04
password='Sqler-Spike-1pw'

case "$mode" in
tls | plain) ;;
*)
    echo "start-mssql.sh: unknown mode '$mode' (tls or plain)" >&2
    exit 1
    ;;
esac

# Runs openssl with its diagnostics on stderr; a failure says so instead of exiting silently.
ossl() {
    openssl "$@" >&2 || {
        echo "start-mssql.sh: 'openssl $1' failed (is openssl installed?)" >&2
        exit 1
    }
}

mkdir -p "$out"
out=$(cd "$out" && pwd)

docker rm -f "$name" >/dev/null 2>&1 || true

if [ "$mode" = tls ]; then
    # A CA, and a server certificate for DNS:localhost only (so 127.0.0.1 fails verification).
    ossl req -x509 -newkey rsa:2048 -nodes -days 3 -subj "/CN=sqler-spike-ca" \
        -keyout "$out/ca.key" -out "$out/ca.crt"
    ossl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
        -keyout "$out/mssql.key" -out "$out/mssql.csr"
    printf 'subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' >"$out/mssql.ext"
    ossl x509 -req -in "$out/mssql.csr" -CA "$out/ca.crt" -CAkey "$out/ca.key" \
        -CAcreateserial -days 3 -extfile "$out/mssql.ext" -out "$out/mssql.pem"
    printf '[network]\ntlscert = /certs/mssql.pem\ntlskey = /certs/mssql.key\ntlsprotocols = 1.2\nforceencryption = 1\n' >"$out/mssql.conf"
    # The server runs as the unprivileged mssql user (uid 10001); the key belongs to a throwaway CA
    # that lives 3 days in the (git-ignored) results directory, so it is made world-readable.
    chmod 644 "$out/mssql.key" "$out/mssql.pem" "$out/ca.crt" "$out/mssql.conf"
    chmod 600 "$out/ca.key"
    # shellcheck disable=SC2086
    docker run -d --name "$name" --platform linux/amd64 \
        -p "127.0.0.1:$port:1433" \
        -e ACCEPT_EULA=Y -e "MSSQL_SA_PASSWORD=$password" \
        -v "$out/mssql.conf:/var/opt/mssql/mssql.conf:ro" \
        -v "$out/mssql.pem:/certs/mssql.pem:ro" \
        -v "$out/mssql.key:/certs/mssql.key:ro" \
        "$image" >&2
else
    docker run -d --name "$name" --platform linux/amd64 \
        -p "127.0.0.1:$port:1433" \
        -e ACCEPT_EULA=Y -e "MSSQL_SA_PASSWORD=$password" \
        "$image" >&2
fi

# Readiness: ask over TCP inside the container. sqlcmd lives under tools18 (-C trusts the server cert,
# only for this readiness probe; the guest verifies the chain itself) or tools.
i=0
until docker exec "$name" sh -c \
    "for t in /opt/mssql-tools18/bin/sqlcmd /opt/mssql-tools/bin/sqlcmd; do [ -x \$t ] && exec \$t -C -S 127.0.0.1 -U sa -P '$password' -Q 'select 1' -b; done; exit 1" \
    >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -ge 180 ]; then
        echo "$image did not become ready in 180 s" >&2
        docker logs "$name" >&2 || true
        exit 1
    fi
    sleep 1
done

echo "SPIKE_MSSQL_HOST=127.0.0.1"
echo "SPIKE_MSSQL_PORT=$port"
echo "SPIKE_MSSQL_USER=sa"
echo "SPIKE_MSSQL_PASSWORD=$password"
if [ "$mode" = tls ]; then
    echo "SPIKE_MSSQL_CA=$out/ca.crt"
fi
