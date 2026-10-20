#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Starts an OpenSSH server in a container for the RSA key tests of sqler-tunnel and prints the test
# environment as KEY=value lines on stdout (everything else goes to stderr).
#
#   CI:    sh crates/sqler-tunnel/tests/docker/start-sshd.sh "$RUNNER_TEMP/sshd" >> "$GITHUB_ENV"
#   local: eval "$(sh crates/sqler-tunnel/tests/docker/start-sshd.sh target/sshd | sed 's/^/export /')"
#
# The container (Alpine, name sqler-test-sshd) runs three sshd processes that only accept public
# keys: one on 22 inside the container (the target the tests forward to), and two published on
# 127.0.0.1 that accept only rsa-sha2-256 (33256) or only rsa-sha2-512 (33512) signatures. The test
# keys (RSA-3072, in OpenSSH, PKCS#1 PEM, PKCS#8 PEM and passphrase-encrypted form) are generated
# into <outdir> with ssh-keygen; none is committed. Idempotent: an earlier container is replaced
# and the keys are remade. Needs docker and ssh-keygen.
set -eu

out=${1:?usage: start-sshd.sh <outdir>}
name=sqler-test-sshd
image=alpine:3.22
user=sqler
port256=33256
port512=33512
passphrase=sqler-test-passphrase

mkdir -p "$out"
out=$(cd "$out" && pwd)

# Runs ssh-keygen with its output on stderr; a failure says so instead of exiting silently.
keygen() {
    ssh-keygen -q "$@" >&2 || {
        echo "start-sshd.sh: 'ssh-keygen $*' failed (is OpenSSH installed?)" >&2
        exit 1
    }
}

rm -f "$out/id_rsa" "$out/id_rsa.pub" "$out/id_rsa_pem" "$out/id_rsa_pkcs8" "$out/id_rsa_enc" \
    "$out/known_hosts"
keygen -t rsa -b 3072 -m RFC4716 -N '' -C sqler-test -f "$out/id_rsa"
# The same key in the other formats. -p rewrites a copy in place.
cp "$out/id_rsa" "$out/id_rsa_pem"
keygen -p -m PEM -P '' -N '' -f "$out/id_rsa_pem"
cp "$out/id_rsa" "$out/id_rsa_pkcs8"
keygen -p -m PKCS8 -P '' -N '' -f "$out/id_rsa_pkcs8"
cp "$out/id_rsa" "$out/id_rsa_enc"
keygen -p -m RFC4716 -P '' -N "$passphrase" -f "$out/id_rsa_enc"
chmod 600 "$out/id_rsa" "$out/id_rsa_pem" "$out/id_rsa_pkcs8" "$out/id_rsa_enc"

docker rm -f "$name" >/dev/null 2>&1 || true

# The account needs a password entry that is not the locked "!" (sshd refuses a locked account
# even for public keys); password logins are off. Host key: ed25519 only. PerSourcePenalties is
# off so that repeated test runs from one address are not throttled.
docker run -d --name "$name" \
    -p "127.0.0.1:$port256:2256" -p "127.0.0.1:$port512:2512" \
    -e "SSH_USER=$user" \
    "$image" sh -c '
        set -e
        apk add --no-cache openssh >&2
        ssh-keygen -A >&2
        adduser -D -s /bin/sh "$SSH_USER"
        echo "$SSH_USER:sqler-unlocked-account" | chpasswd
        mkdir -p "/home/$SSH_USER/.ssh"
        : > "/home/$SSH_USER/.ssh/authorized_keys"
        chmod 700 "/home/$SSH_USER/.ssh"
        chmod 600 "/home/$SSH_USER/.ssh/authorized_keys"
        chown -R "$SSH_USER" "/home/$SSH_USER/.ssh"
        common="-h /etc/ssh/ssh_host_ed25519_key -o PasswordAuthentication=no -o KbdInteractiveAuthentication=no -o AllowTcpForwarding=yes -o PerSourcePenalties=no"
        /usr/sbin/sshd -p 2256 -o PubkeyAcceptedAlgorithms=rsa-sha2-256 $common
        /usr/sbin/sshd -p 2512 -o PubkeyAcceptedAlgorithms=rsa-sha2-512 $common
        exec /usr/sbin/sshd -D -e -p 22 $common
    ' >&2

# Wait until all three sshd answer with their banner.
banner() {
    docker exec "$name" sh -c "echo | nc -w 2 127.0.0.1 $1 2>/dev/null | grep -q '^SSH-2.0-'" >/dev/null 2>&1
}
i=0
until banner 22 && banner 2256 && banner 2512; do
    i=$((i + 1))
    if [ "$i" -ge 90 ]; then
        echo "sshd did not become ready in 90 s" >&2
        docker logs "$name" >&2 || true
        exit 1
    fi
    sleep 1
done

# authorized_keys (mode 600, owned by the account; the file already exists) and known_hosts for
# both published ports.
docker exec -i "$name" sh -c "cat > /home/$user/.ssh/authorized_keys" <"$out/id_rsa.pub" >&2
host_key=$(docker exec "$name" cut -d' ' -f1,2 /etc/ssh/ssh_host_ed25519_key.pub)
printf '[127.0.0.1]:%s %s\n[127.0.0.1]:%s %s\n' "$port256" "$host_key" "$port512" "$host_key" \
    >"$out/known_hosts"

echo "SQLER_TEST_SSH_HOST=127.0.0.1"
echo "SQLER_TEST_SSH_PORT_SHA256=$port256"
echo "SQLER_TEST_SSH_PORT_SHA512=$port512"
echo "SQLER_TEST_SSH_USER=$user"
echo "SQLER_TEST_SSH_KNOWN_HOSTS=$out/known_hosts"
echo "SQLER_TEST_SSH_KEY_RSA=$out/id_rsa"
echo "SQLER_TEST_SSH_KEY_RSA_PEM=$out/id_rsa_pem"
echo "SQLER_TEST_SSH_KEY_RSA_PKCS8=$out/id_rsa_pkcs8"
echo "SQLER_TEST_SSH_KEY_RSA_ENC=$out/id_rsa_enc"
echo "SQLER_TEST_SSH_KEY_PASSPHRASE=$passphrase"
