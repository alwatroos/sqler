#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Starts SQL Server 2022 with a TLS certificate (encryption FORCED) for the DB tests of
# sqler-driver-mssql and prints the test environment as KEY=value lines on stdout (diagnostics go
# to stderr). Shape follows crates/sqler-driver-mysql/tests/docker/start-mysql.sh.
#
#   local:  eval "$(sh crates/sqler-driver-mssql/tests/docker/start-mssql.sh /tmp/mssql | sed 's/^/export /')"
#   remove: docker rm -f sqler-test-mssql      (no named volumes are created)
#
# Usage: start-mssql.sh <outdir>
#   <outdir> receives a throwaway CA (ca.crt, ca.key) and the server certificate for DNS:localhost
#   only, so a connect to 127.0.0.1 fails host name verification.
# Container sqler-test-mssql on 127.0.0.1:14333. Idempotent: an earlier container is replaced.
#
# Licence and size: the image is Microsoft's SQL Server 2022 and runs here as the Developer edition
# (MSSQL_PID=Developer, ACCEPT_EULA=Y: running it means you accept the Developer edition EULA,
# which allows non-production use only). It is about 1.5 GB. On Apple Silicon it runs as amd64
# under emulation and is slow to start. Needs docker and openssl.
set -eu

out=${1:?usage: start-mssql.sh <outdir>}
name=sqler-test-mssql
port=14333
# Tag verified live on mcr.microsoft.com (tags/list) on 2026-10-22.
image=mcr.microsoft.com/mssql/server:2022-CU27-ubuntu-22.04
password='Sqler-Test-1pw'

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

# A CA, and a server certificate for DNS:localhost only.
ossl req -x509 -newkey rsa:2048 -nodes -days 3 -subj "/CN=sqler-test-ca" \
    -keyout "$out/ca.key" -out "$out/ca.crt"
ossl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
    -keyout "$out/mssql.key" -out "$out/mssql.csr"
printf 'subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' >"$out/mssql.ext"
ossl x509 -req -in "$out/mssql.csr" -CA "$out/ca.crt" -CAkey "$out/ca.key" \
    -CAcreateserial -days 3 -extfile "$out/mssql.ext" -out "$out/mssql.pem"
printf '[network]\ntlscert = /certs/mssql.pem\ntlskey = /certs/mssql.key\ntlsprotocols = 1.2\nforceencryption = 1\n' >"$out/mssql.conf"
# The server runs as the unprivileged mssql user (uid 10001); the key belongs to a throwaway CA that
# lives 3 days in <outdir>, so it is made world-readable.
chmod 644 "$out/mssql.key" "$out/mssql.pem" "$out/ca.crt" "$out/mssql.conf"
chmod 600 "$out/ca.key"

docker run -d --name "$name" --platform linux/amd64 \
    -p "127.0.0.1:$port:1433" \
    -e ACCEPT_EULA=Y -e MSSQL_PID=Developer -e "MSSQL_SA_PASSWORD=$password" \
    -v "$out/mssql.conf:/var/opt/mssql/mssql.conf:ro" \
    -v "$out/mssql.pem:/certs/mssql.pem:ro" \
    -v "$out/mssql.key:/certs/mssql.key:ro" \
    "$image" >&2

# Readiness: ask over TCP inside the container. sqlcmd lives under tools18 (-C trusts the server
# certificate, only for this readiness probe) or tools.
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

echo "SQLER_TEST_MSSQL_URL=sqlserver://sa:$password@localhost:$port/master"
echo "SQLER_TEST_MSSQL_CA=$out/ca.crt"
