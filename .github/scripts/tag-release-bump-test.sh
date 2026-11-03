#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Cases for tag-release-bump.sh (ADR-0064 amendment 4). A temp git repo, no network. Prints "ok N cases" and exits 0,
# or the failing case and exits 1.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
bump=$here/tag-release-bump.sh
release_yml=$here/../workflows/release.yml

nl='
'
cr=$(printf '\r')
long=1.0.0-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
n=0

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
repo=$work/repo
mkdir "$repo"

die() {
    printf 'FAIL %s\n' "$*"
    exit 1
}

# fixture <version>: a minimal workspace at <version>, an initial commit, no tags.
fixture() {
    rm -rf "$repo"
    mkdir "$repo"
    cat >"$repo/Cargo.toml" <<TOML
[workspace]
members = ["crates/*"]

[workspace.package]
version = "$1"
edition = "2024"

[workspace.dependencies]
sqler-a = { path = "crates/sqler-a", version = "$1" }
sqler-b = { path = "crates/sqler-b", version = "$1", default-features = false }

# external
foo = "$1"
TOML
    cat >"$repo/Cargo.lock" <<LOCK
[[package]]
name = "foo"
version = "$1"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "sqler-a"
version = "$1"

[[package]]
name = "xtask"
version = "$1"
dependencies = [
 "foo",
]
LOCK
    cat >"$repo/CHANGELOG.md" <<LOG
# Changelog

## [Unreleased]

## [1.3.1] - x
## [1.4.0] - x
## [2.0.0] - x
## [1.3.0] - x
## [1.4.0-rc.2] - x
## [9.9.9] - x
LOG
    git -C "$repo" init -q
    git -C "$repo" add -A
    git -C "$repo" -c user.name=t -c user.email=t@example.invalid commit -q -m init
}

# run <bump> <version>: sets out and rc; GITHUB_OUTPUT goes to $work/out.
run() {
    : >"$work/out"
    if out=$(ROOT=$repo BUMP=$1 VERSION=$2 GITHUB_OUTPUT=$work/out sh "$bump" 2>&1); then rc=0; else rc=$?; fi
}

# expect_ok <name> <start> <bump> <version> <new>
expect_ok() {
    n=$((n + 1))
    fixture "$2"
    run "$3" "$4"
    [ "$rc" -eq 0 ] || die "$1: rc=$rc: $out"
    grep -qx "version = \"$5\"" "$repo/Cargo.toml" || die "$1: workspace version"
    grep -q "^sqler-a = .*version = \"$5\"" "$repo/Cargo.toml" || die "$1: path dep a"
    grep -q "^sqler-b = .*version = \"$5\", default-features" "$repo/Cargo.toml" || die "$1: path dep b"
    grep -q "^foo = \"$2\"" "$repo/Cargo.toml" || die "$1: external dep moved"
    [ "$(grep -c "^version = \"$5\"" "$repo/Cargo.lock")" -eq 2 ] || die "$1: lock workspace entries"
    [ "$(grep -c "^version = \"$2\"" "$repo/Cargo.lock")" -eq 1 ] || die "$1: registry package moved"
    grep -qx "tag=v$5" "$work/out" || die "$1: GITHUB_OUTPUT tag"
    grep -qx "version=$5" "$work/out" || die "$1: GITHUB_OUTPUT version"
    grep -qx "old=$2" "$work/out" || die "$1: GITHUB_OUTPUT old"
    grep -qx "changed=true" "$work/out" || die "$1: GITHUB_OUTPUT changed"
    case $out in
        *"tag-release-bump: $2 -> $5 (changed=true)") ;;
        *) die "$1: last line: $out" ;;
    esac
    # The tag check of release.yml must pass on the bumped manifest (expression read from the real file).
    expr=$(grep "version=\$(sed -n '/^\\\\\[workspace" "$release_yml" | head -n 1 | sed "s/^[^']*'\([^']*\)'.*/\1/")
    [ -n "$expr" ] || die "$1: release.yml version expression not found"
    got=$(sed -n "$expr" "$repo/Cargo.toml" | head -n 1)
    [ "$got" = "$5" ] || die "$1: release.yml sed gave '$got'"
}

# expect_fail <name> <start> <bump> <version> [pre-step command]
expect_fail() {
    n=$((n + 1))
    fixture "$2"
    if [ -n "${5:-}" ]; then eval "$5"; fi
    cp "$repo/Cargo.toml" "$work/toml.0"
    cp "$repo/Cargo.lock" "$work/lock.0"
    run "$3" "$4"
    [ "$rc" -eq 1 ] || die "$1: expected rc=1, got rc=$rc: $out"
    case $out in
        "::error::"?*) ;;
        *) die "$1: no error message: $out" ;;
    esac
    case $out in
        *"$nl"*) die "$1: more than one line: $out" ;;
    esac
    cmp -s "$repo/Cargo.toml" "$work/toml.0" || die "$1: Cargo.toml modified"
    cmp -s "$repo/Cargo.lock" "$work/lock.0" || die "$1: Cargo.lock modified"
    [ ! -s "$work/out" ] || die "$1: GITHUB_OUTPUT written"
}

expect_ok patch-from-release 1.3.0 patch "" 1.3.1
expect_ok minor-from-release 1.3.0 minor "" 1.4.0
expect_ok major-from-release 1.3.0 major "" 2.0.0
expect_ok patch-from-pre 1.4.0-rc.1 patch "" 1.4.0
expect_ok minor-from-pre 1.4.0-rc.1 minor "" 1.4.0
expect_ok major-from-pre 1.4.0-rc.1 major "" 2.0.0
expect_ok version-beats-bump 1.3.0 major 1.4.0-rc.2 1.4.0-rc.2
expect_ok version-without-bump 1.3.0 "" 9.9.9 9.9.9

# Already at the target version: nothing is written, changed=false.
n=$((n + 1))
fixture 1.3.0
cp "$repo/Cargo.toml" "$work/toml.0"
cp "$repo/Cargo.lock" "$work/lock.0"
run patch 1.3.0
[ "$rc" -eq 0 ] || die "same-version: rc=$rc: $out"
cmp -s "$repo/Cargo.toml" "$work/toml.0" || die "same-version: Cargo.toml modified"
cmp -s "$repo/Cargo.lock" "$work/lock.0" || die "same-version: Cargo.lock modified"
grep -qx "changed=false" "$work/out" || die "same-version: changed output"
grep -qx "tag=v1.3.0" "$work/out" || die "same-version: tag output"
[ -z "$(ls -A "$repo" | grep '^\.bump-' || true)" ] || die "same-version: temp files left"

expect_fail v-prefix 1.3.0 patch v1.0.0
expect_fail two-part 1.3.0 patch 1.0
expect_fail leading-zero 1.3.0 patch 01.0.0
expect_fail build-metadata 1.3.0 patch 1.0.0+b
expect_fail injection 1.3.0 patch '1.0.0;id'
expect_fail newline 1.3.0 patch "1.0.0${nl}x"
expect_fail trailing-newline 1.3.0 patch "1.0.0${nl}"
expect_fail carriage-return 1.3.0 patch "1.0.0${cr}"
expect_fail too-long 1.3.0 patch "$long$long"
expect_fail existing-tag 1.3.0 patch 9.9.9 'git -C "$repo" tag v9.9.9'
expect_fail existing-tag-by-bump 1.3.0 patch "" 'git -C "$repo" tag v1.3.1'
expect_fail missing-changelog 1.3.0 patch 7.7.7
expect_fail bad-bump 1.3.0 foo ""
expect_fail empty-bump 1.3.0 "" ""
expect_fail inconsistent-lock 1.3.0 patch "" 'sed "s/^version = \"1.3.0\"\$/&/;/name = \"xtask\"/{n;s/1.3.0/1.2.0/;}" "$repo/Cargo.lock" >"$work/l" && mv "$work/l" "$repo/Cargo.lock"'
expect_fail inconsistent-toml 1.3.0 patch "" 'sed "s/^\(sqler-b = .*version = \)\"1.3.0\"/\1\"1.2.0\"/" "$repo/Cargo.toml" >"$work/t" && mv "$work/t" "$repo/Cargo.toml"'

echo "ok $n cases"
