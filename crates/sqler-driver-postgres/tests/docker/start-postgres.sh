#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Starts a PostgreSQL server with TLS for the DB tests and prints the test environment as
# KEY=value lines on stdout (everything else goes to stderr).
#
#   CI:    sh crates/sqler-driver-postgres/tests/docker/start-postgres.sh "$RUNNER_TEMP/pg" >> "$GITHUB_ENV"
#   local: eval "$(sh crates/sqler-driver-postgres/tests/docker/start-postgres.sh target/pg | sed 's/^/export /')"
#
# Idempotent: an earlier container of the same name is replaced and the certificates are remade.
# Needs docker and openssl.
set -eu

out=${1:?usage: start-postgres.sh <outdir>}
name=sqler-test-postgres
image=postgres:18-alpine
port=54329

# Runs openssl with its diagnostics on stderr; a failure says so instead of exiting silently.
ossl() {
    openssl "$@" >&2 || {
        echo "start-postgres.sh: 'openssl $1' failed (is openssl installed?)" >&2
        exit 1
    }
}

mkdir -p "$out"
out=$(cd "$out" && pwd)

# A CA, and a server certificate for DNS:localhost only (so 127.0.0.1 fails verify-full).
ossl req -x509 -newkey rsa:2048 -nodes -days 3 -subj "/CN=sqler-test-ca" \
    -keyout "$out/ca.key" -out "$out/ca.crt"
ossl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
    -keyout "$out/server.key" -out "$out/server.csr"
printf 'subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\n' >"$out/server.ext"
ossl x509 -req -in "$out/server.csr" -CA "$out/ca.crt" -CAkey "$out/ca.key" \
    -CAcreateserial -days 3 -extfile "$out/server.ext" -out "$out/server.crt"
chmod 644 "$out/server.key" "$out/server.crt" "$out/ca.crt"

docker rm -f "$name" >/dev/null 2>&1 || true

# Postgres rejects a key it does not own, so the key is copied to /tmp inside the container and
# chowned there. The official entrypoint then runs as usual, with the extra server flags.
docker run -d --name "$name" \
    -p "127.0.0.1:$port:5432" \
    -e POSTGRES_PASSWORD=sqler \
    -v "$out/server.crt:/certs/server.crt:ro" \
    -v "$out/server.key:/certs/server.key.src:ro" \
    --entrypoint sh "$image" -c '
        cp /certs/server.key.src /tmp/server.key
        chown postgres /tmp/server.key
        chmod 600 /tmp/server.key
        exec docker-entrypoint.sh postgres -c ssl=on \
            -c ssl_cert_file=/certs/server.crt -c ssl_key_file=/tmp/server.key
    ' >&2

# The init server only listens on the socket, so ask over TCP.
i=0
until docker exec "$name" pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -ge 60 ]; then
        echo "PostgreSQL did not become ready in 60 s"
        docker logs "$name" >&2 || true
        exit 1
    fi
    sleep 1
done

echo "SQLER_TEST_POSTGRES_URL=postgres://postgres:sqler@127.0.0.1:$port/postgres"
echo "SQLER_TEST_POSTGRES_CA=$out/ca.crt"
