#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Starts a MySQL 8.4 or MariaDB 11.8 server with TLS for the DB tests and prints the test
# environment as KEY=value lines on stdout (everything else goes to stderr).
#
#   CI:    sh crates/sqler-driver-mysql/tests/docker/start-mysql.sh "$RUNNER_TEMP/my" mysql >> "$GITHUB_ENV"
#   local: eval "$(sh crates/sqler-driver-mysql/tests/docker/start-mysql.sh target/my mariadb | sed 's/^/export /')"
#
# The second argument is `mysql` (default) or `mariadb`. Both variants use the container name
# sqler-test-mysql and 127.0.0.1:33069, so one replaces the other. Idempotent: an earlier container
# is replaced and the certificates are remade. Needs docker and openssl.
set -eu

out=${1:?usage: start-mysql.sh <outdir> [mysql|mariadb]}
variant=${2:-mysql}
name=sqler-test-mysql
port=33069
password=sqler

case "$variant" in
mysql)
    image=mysql:8.4
    client=mysql
    envs="-e MYSQL_ROOT_PASSWORD=$password -e MYSQL_DATABASE=sqler"
    ;;
mariadb)
    image=mariadb:11.8
    client=mariadb
    envs="-e MARIADB_ROOT_PASSWORD=$password -e MARIADB_DATABASE=sqler"
    ;;
*)
    echo "start-mysql.sh: unknown variant '$variant' (mysql or mariadb)" >&2
    exit 1
    ;;
esac

# Runs openssl with its diagnostics on stderr; a failure says so instead of exiting silently.
ossl() {
    openssl "$@" >&2 || {
        echo "start-mysql.sh: 'openssl $1' failed (is openssl installed?)" >&2
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
chmod 600 "$out/server.key" "$out/ca.key"
chmod 644 "$out/server.crt" "$out/ca.crt"

docker rm -f "$name" >/dev/null 2>&1 || true

# The key stays mode 600 on the host (the container starts as root, which can read it). The server
# runs as the unprivileged mysql user, so the key is copied to /tmp inside the container and
# chowned there. The official entrypoint then runs as usual, with the extra server flags.
# shellcheck disable=SC2086
docker run -d --name "$name" \
    -p "127.0.0.1:$port:3306" \
    $envs \
    -v "$out/ca.crt:/certs/ca.crt:ro" \
    -v "$out/server.crt:/certs/server.crt:ro" \
    -v "$out/server.key:/certs/server.key.src:ro" \
    --entrypoint sh "$image" -c '
        cp /certs/server.key.src /tmp/server.key
        chown mysql /tmp/server.key
        chmod 600 /tmp/server.key
        exec docker-entrypoint.sh "$@"
    ' sh --ssl-ca=/certs/ca.crt --ssl-cert=/certs/server.crt --ssl-key=/tmp/server.key >&2

# The init server has no TCP listener, so ask over TCP.
i=0
until docker exec "$name" "$client" -h127.0.0.1 -uroot "-p$password" -e 'select 1' >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -ge 90 ]; then
        echo "$image did not become ready in 90 s" >&2
        docker logs "$name" >&2 || true
        exit 1
    fi
    sleep 1
done

echo "SQLER_TEST_MYSQL_URL=mysql://root:$password@127.0.0.1:$port/sqler"
echo "SQLER_TEST_MYSQL_CA=$out/ca.crt"
