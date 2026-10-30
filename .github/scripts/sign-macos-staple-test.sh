#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Local test (macOS only, from the repository root, no cargo build) of the stapling step of sign-macos.sh --dmg.
# The real `xcrun stapler` needs a notarised app, so a stand-in `xcrun` first in PATH plays it: `stapler staple <app>`
# writes Contents/CodeResources through a 0600 temporary file (the worst case, which a umask does not change);
# on a dmg it does nothing; `stapler validate` succeeds. Everything else goes to the real xcrun.
#
#   sh .github/scripts/sign-macos-staple-test.sh
#
# Checks: (1) sign-macos.sh --dmg with SQLER_SIGN_LOCAL_STAPLE=1 under its umask 077 leaves a stapled app that
# check-dmg.sh accepts (the 0600 ticket file is 644 in the dmg, codesign --verify --deep --strict passes);
# (2) negative: a dmg that still holds a 0600 Contents/CodeResources is rejected by check-dmg.sh.
# Prints `sign-macos-staple-test: ok`; exit 1 on a failed check. The scratch dir and the mounts are removed on exit.
set -eu

[ "$(uname -s)" = Darwin ] || {
    echo "sign-macos-staple-test: macOS only" >&2
    exit 2
}
[ -f .github/scripts/sign-macos.sh ] || {
    echo "sign-macos-staple-test: run from the repository root" >&2
    exit 2
}

d=$(mktemp -d)
trap 'rm -rf "$d"' EXIT
triple=aarch64-apple-darwin
fail() {
    echo "sign-macos-staple-test: FAIL: $1" >&2
    exit 1
}

# A self-signed code-signing identity.
printf '[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=SQLer local test\n[ext]\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n' >"$d/openssl.cnf"
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 1 -config "$d/openssl.cnf" -extensions ext \
    -keyout "$d/k.pem" -out "$d/c.pem" 2>/dev/null
/usr/bin/openssl pkcs12 -export -inkey "$d/k.pem" -in "$d/c.pem" -out "$d/id.p12" -passout pass:test

# A bundle dir in the dist layout with a tiny Mach-O `sqler` that prints a version.
b="$d/in/sqler-app-$triple"
mkdir -p "$b/licenses"
printf '#include <stdio.h>\nint main(void){puts("sqler 0.0.0");return 0;}\n' >"$d/sqler.c"
cc -o "$b/sqler" "$d/sqler.c"
for f in LICENSE NOTICE THIRD-PARTY-LICENSES.txt; do echo test >"$b/$f"; done
echo test >"$b/licenses/x.txt"
tar -cJf "$d/sqler-app-$triple.tar.xz" -C "$d/in" "sqler-app-$triple"
(cd "$d" && shasum -a 256 "sqler-app-$triple.tar.xz" >"sqler-app-$triple.tar.xz.sha256")
mkdir "$d/out"
cp "$d"/sqler-app-$triple.tar.xz* "$d/out/"
sh packaging/macos/build-dmg.sh "$b" 0.0.0 "$triple" "$d/out" >/dev/null

# The stand-in xcrun.
mkdir "$d/bin"
cat >"$d/bin/xcrun" <<'STUB'
#!/bin/sh
if [ "${1:-}" = stapler ]; then
    case ${2:-} in
    staple)
        if [ -d "$3" ]; then
            t=$(mktemp "$3/Contents/.ticket.XXXXXX") || exit 1
            echo ticket >"$t"
            chmod 600 "$t"
            mv "$t" "$3/Contents/CodeResources"
        fi
        exit 0
        ;;
    validate) exit 0 ;;
    esac
fi
exec /usr/bin/xcrun "$@"
STUB
chmod 755 "$d/bin/xcrun"

# (1) the fix: stapled under umask 077 and still readable by others.
PATH="$d/bin:$PATH" SQLER_SIGN_LOCAL_TEST=1 SQLER_SIGN_LOCAL_STAPLE=1 APPLE_CERTIFICATE_P12_BASE64=$(base64 <"$d/id.p12") \
    APPLE_CERTIFICATE_PASSWORD=test \
    sh .github/scripts/sign-macos.sh --dmg 0.0.0 "$d/out" "$d/out/sqler-app-$triple.tar.xz" >"$d/sign.out" 2>&1 ||
    {
        cat "$d/sign.out" >&2
        fail "sign-macos.sh --dmg failed with the stand-in stapler"
    }
grep -q 'stapled SQLer.app' "$d/sign.out" || fail "the stapling step did not run"
mkdir "$d/mnt"
hdiutil attach -nobrowse -readonly -mountpoint "$d/mnt" "$d/out/sqler-app-$triple.dmg" >/dev/null
trap 'hdiutil detach "$d/mnt" -force >/dev/null 2>&1 || true; rm -rf "$d"' EXIT
cr="$d/mnt/SQLer.app/Contents/CodeResources"
[ -f "$cr" ] || fail "the stand-in ticket is not in the dmg"
case $(stat -f %Lp "$cr") in
*[4-7][4-7][4-7]) ;;
*) fail "Contents/CodeResources is not readable by others: $(stat -f %Lp "$cr")" ;;
esac
codesign --verify --deep --strict "$d/mnt/SQLer.app" || fail "the stapled app no longer verifies"
hdiutil detach "$d/mnt" -force >/dev/null
trap 'rm -rf "$d"' EXIT
sh packaging/macos/check-dmg.sh "$d/out/sqler-app-$triple.dmg" >/dev/null || fail "check-dmg.sh rejects the fixed dmg"

# (2) negative: the 0600 ticket the v1.2.1 run produced is caught by check-dmg.sh.
rm -rf "$d/neg"
mkdir "$d/neg" "$d/negdmg"
sh packaging/macos/build-dmg.sh --app "$b" 0.0.0 "$d/neg" >/dev/null
codesign --force --sign - "$d/neg/SQLer.app" 2>/dev/null
echo ticket >"$d/neg/SQLer.app/Contents/CodeResources"
chmod 600 "$d/neg/SQLer.app/Contents/CodeResources"
sh packaging/macos/build-dmg.sh --pack "$d/neg/SQLer.app" 0.0.0 "$triple" "$d/negdmg" >/dev/null
chmod 600 "$d/neg/SQLer.app/Contents/CodeResources"
if sh packaging/macos/check-dmg.sh "$d/negdmg/sqler-app-$triple.dmg" >"$d/neg.out" 2>&1; then
    fail "check-dmg.sh accepted a 0600 Contents/CodeResources"
fi
grep -q 'cannot read: SQLer.app/Contents/CodeResources' "$d/neg.out" || fail "check-dmg.sh failed for another reason"

echo "sign-macos-staple-test: ok"
