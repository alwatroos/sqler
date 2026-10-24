#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Fixture cases for tag-release-validate.sh (R18). No network: a throwaway git repository holds the commits and a
# fake `gh` (tag-release-fake-gh.sh) is put first on PATH. Prints "ok N cases" and exits 0, or the failing case and
# exits 1.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
validate=$here/tag-release-validate.sh
fake=$here/tag-release-fake-gh.sh

real_release=$here/../workflows/release.yml
# The marker line the validator greps (check 8) must exist in the REAL release.yml; fixtures reuse that very line.
marker_line=$(grep '^# tag-dispatch: ' "$real_release" | head -n 1) || {
    echo "FAIL: the real release.yml has no line starting with '# tag-dispatch: '"
    exit 1
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
mkdir "$tmp/bin" "$tmp/repo"
cp "$fake" "$tmp/bin/gh"
chmod +x "$tmp/bin/gh"
PATH=$tmp/bin:$PATH
export PATH

tab=$(printf '\t')
nl='
'
log=$tmp/gh.log
: >"$log"

cd "$tmp/repo"
git init -q -b main
git config user.name test
git config user.email test@example.invalid
git config commit.gpgsign false

# commit <cargo-version> <changelog-file-content> <with-marker 0|1> <message>
commit() {
    printf '[package]\nversion = "9.9.9"\n\n[workspace.package]\nversion = "%s"\nedition = "2024"\n' "$1" >Cargo.toml
    printf '%s' "$2" >CHANGELOG.md
    mkdir -p .github/workflows
    if [ "$3" = 1 ]; then
        printf 'name: Release\n%s\n' "$marker_line" >.github/workflows/release.yml
    else
        printf 'name: Release\n' >.github/workflows/release.yml
    fi
    git add -A
    git commit -q -m "$4"
    git rev-parse HEAD
}

good_log='# Changelog

## [Unreleased]

## [1.0.0] - 2026-10-30

### Added

- Things.

## [0.9.0] - 2026-01-01

- Older DuckDB note.
'
a=$(commit 0.9.0 "$good_log" 1 cargo-mismatch)
b=$(commit 1.0.0 '## [Unreleased]

## [0.9.0]
' 1 no-heading)
c=$(commit 1.0.0 '## [1.0.0-rc.1] - 2026-10-01
- rc
' 1 only-rc)
d=$(commit 1.0.0 '## [1.0.0] - 2026-10-30
- Adds DuckDB support.
' 1 duckdb-in-section)
e=$(commit 1.0.0 "$good_log" 1 duckdb-other-section)
f=$(commit 1.0.0 "$good_log" 0 no-marker)
h=$(commit 1.0.0-rc.1 '## [1.0.0-rc.1] - 2026-10-01
- rc
' 1 prerelease)
g=$(commit 1.0.0 "$good_log" 1 good)
git checkout -q -b side "$a"
x=$(commit 1.0.0 "$good_log" 1 off-main)
git checkout -q main

pass=0
# check <label> <expected-rc> <reason-substring> [VAR=value ...]
check() {
    label=$1
    want=$2
    sub=$3
    shift 3
    rc=0
    out=$(env \
        VERSION=1.0.0 "SHA=$g" "CONFIRM=release v1.0.0" REF=refs/heads/main "MAIN_HEAD=$g" REPO=o/r GH_TOKEN=x \
        "GITHUB_OUTPUT=$tmp/out" "FAKE_GH_LOG=$log" FAKE_GH_TAG_REFS= \
        "FAKE_GH_RELEASE_RUN=completed${tab}success${tab}$g" "FAKE_GH_CI_RUN=completed${tab}success" \
        "$@" sh "$validate" 2>&1) || rc=$?
    if [ "$rc" != "$want" ]; then
        printf 'FAIL %s: exit %s, wanted %s\n%s\n' "$label" "$rc" "$want" "$out"
        exit 1
    fi
    case $out in
        *"$sub"*) ;;
        *)
            printf 'FAIL %s: output lacks "%s"\n%s\n' "$label" "$sub" "$out"
            exit 1
            ;;
    esac
    pass=$((pass + 1))
}

short=$(printf '%s' "$g" | cut -c1-7)

# Happy paths.
check happy-full 0 "ok (tag v1.0.0, commit $g)"
check happy-short 0 "ok (tag v1.0.0" "SHA=$short"
check happy-empty-sha 0 "ok (tag v1.0.0" SHA=
check happy-prerelease 0 "ok (tag v1.0.0-rc.1" VERSION=1.0.0-rc.1 "CONFIRM=release v1.0.0-rc.1" "SHA=$h" "FAKE_GH_RELEASE_RUN=completed${tab}success${tab}$h"
check happy-duckdb-other-section 0 "ok (tag" "SHA=$e" "FAKE_GH_RELEASE_RUN=completed${tab}success${tab}$e" "FAKE_GH_CI_RUN=completed${tab}success"
check happy-other-tag-only 0 "ok (tag" "FAKE_GH_TAG_REFS=refs/tags/v1.0.0-rc.1"

# Outputs.
: >"$tmp/out"
check outputs 0 "ok"
grep -qx "tag=v1.0.0" "$tmp/out" || { echo "FAIL outputs: tag"; exit 1; }
grep -qx "sha=$g" "$tmp/out" || { echo "FAIL outputs: sha"; exit 1; }
grep -qx "version=1.0.0" "$tmp/out" || { echo "FAIL outputs: version"; exit 1; }

# Confirm.
check confirm-no-v 1 "confirm must be" "CONFIRM=release 1.0.0"
check confirm-case 1 "confirm must be" "CONFIRM=Release v1.0.0"
check confirm-trailing-space 1 "confirm must be" "CONFIRM=release v1.0.0 "
check confirm-empty 1 "confirm must be" CONFIRM=

# Ref.
check ref-branch 1 "refs/heads/main" REF=refs/heads/x
check ref-tag 1 "refs/heads/main" REF=refs/tags/v1.0.0

# Version.
check version-v 1 "version is not semver" VERSION=v1.0.0 "CONFIRM=release vv1.0.0"
check version-two-parts 1 "version is not semver" VERSION=1.0 "CONFIRM=release v1.0"
check version-leading-zero 1 "version is not semver" VERSION=01.0.0 "CONFIRM=release v01.0.0"
check version-build 1 "version is not semver" VERSION=1.0.0+b "CONFIRM=release v1.0.0+b"
check version-inject 1 "version is not semver" 'VERSION=1.0.0;id' 'CONFIRM=release v1.0.0;id'
check version-subst 1 "version is not semver" 'VERSION=$(id)' 'CONFIRM=release v$(id)'
check version-newline 1 "single line" "VERSION=1.0.0${nl}2.0.0"
check version-empty 1 "version is empty" VERSION=
check version-long 1 "longer than 64" "VERSION=1.0.0-$(printf 'a%.0s' $(seq 1 70))"

# Sha.
check sha-name 1 "sha must be" SHA=main
check sha-option 1 "sha must be" SHA=--help
check sha-nonhex 1 "sha must be" SHA=xyz
check sha-upper 1 "sha must be" "SHA=$(printf '%s' "$short" | tr a-f A-F)A"
check sha-unknown 1 "unknown or ambiguous" SHA=deadbeefdeadbeef
check sha-off-main 1 "not on main" "SHA=$x"
check sha-newline 1 "single line" "SHA=$short${nl}$short"
check main-head-bad 1 "MAIN_HEAD" MAIN_HEAD=abc

# Files at the commit.
check cargo-mismatch 1 "workspace version at $a is 0.9.0" "SHA=$a"
check cargo-mismatch-head 1 "is 1.0.0, not 1.0.1" VERSION=1.0.1 "CONFIRM=release v1.0.1"
check changelog-no-heading 1 "no '## [1.0.0]' heading" "SHA=$b"
check changelog-only-rc 1 "no '## [1.0.0]' heading" "SHA=$c"
check changelog-duckdb 1 "mentions duckdb" "SHA=$d"
check marker-missing 1 "predates R18" "SHA=$f"

# Live checks.
check tag-exists 1 "tag v1.0.0 already exists" "FAKE_GH_TAG_REFS=refs/tags/v1.0.0"
check gh-api-fails 1 "could not list tags" FAKE_GH_FAIL=api
check gh-release-fails 1 "could not read the latest Release dry run" FAKE_GH_FAIL=release
check gh-ci-fails 1 "could not read the CI run" FAKE_GH_FAIL=ci
check dry-run-failure 1 "did not complete successfully" "FAKE_GH_RELEASE_RUN=completed${tab}failure${tab}$g"
check dry-run-in-progress 1 "did not complete successfully" "FAKE_GH_RELEASE_RUN=in_progress${tab}${tab}$g"
check dry-run-none 1 "no Release dry run found" FAKE_GH_RELEASE_RUN=
check dry-run-other-commit 1 "run the Release workflow on main first" "FAKE_GH_RELEASE_RUN=completed${tab}success${tab}$a"
check dry-run-other-commit-names-sha 1 "not $g" "FAKE_GH_RELEASE_RUN=completed${tab}success${tab}$a"
check ci-failure 1 "CI has no successful" "FAKE_GH_CI_RUN=completed${tab}failure"
check ci-none 1 "CI has no successful" FAKE_GH_CI_RUN=

# The validator is read-only.
if grep -q -e '--method' -e 'workflow run' "$log"; then
    echo "FAIL: the validator made a write call"
    exit 1
fi

echo "ok $pass cases"
