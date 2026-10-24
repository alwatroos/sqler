#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Validation of the owner-dispatched "Tag release" workflow (R18, ADR-0064 amendment 3). Fails closed: the first
# failed check prints "tag-release-validate: <reason>" and exits 1. It never writes anything through the API; the
# three `gh` calls are read-only and sit behind the functions below (tests put a fake `gh` first on PATH).
#
# Run from the repository checkout (full history of main). Inputs are environment variables only:
#   VERSION    e.g. 1.0.0 (no "v" prefix)
#   SHA        commit to tag, 7-40 lowercase hex; empty means MAIN_HEAD
#   CONFIRM    must be exactly "release v<VERSION>"
#   REF        github.ref, must be refs/heads/main
#   MAIN_HEAD  github.sha, the head of main
#   REPO       github.repository
#   GH_TOKEN   token for gh (read by gh itself)
# On success it appends tag=, sha= and version= to $GITHUB_OUTPUT (stdout when unset).
#
# Release gate (strict, team decision): a successful `Release` workflow_dispatch run on main AND a successful `CI`
# push run on main must both have head_sha equal to the commit being tagged. A dry run on another commit fails.
set -eu

: "${VERSION:=}" "${SHA:=}" "${CONFIRM:=}" "${REF:=}" "${MAIN_HEAD:=}" "${REPO:=}"

fail() {
    printf 'tag-release-validate: %s\n' "$1"
    exit 1
}

# Live checks. A non-zero return is a validation failure. The jq programs are fixed strings.
tag_exists() {
    refs=$(gh api "repos/$REPO/git/matching-refs/tags/$1" --jq '.[].ref') || return 1
    printf '%s\n' "$refs" | grep -xF "refs/tags/$1" || true
}
latest_release_dry_run() {
    gh run list --repo "$REPO" --workflow release.yml --event workflow_dispatch --branch main --limit 1 --json status,conclusion,headSha --jq '.[] | [.status,.conclusion,.headSha] | @tsv'
}
ci_run_on() {
    gh run list --repo "$REPO" --workflow ci.yml --event push --branch main --commit "$1" --limit 1 --json status,conclusion --jq '.[] | [.status,.conclusion] | @tsv'
}

nl='
'
tab=$(printf '\t')

# 1. ref
[ "$REF" = "refs/heads/main" ] || fail "must run from refs/heads/main (got a different ref)"

# 2. version
case $VERSION in
    *"$nl"*) fail "version must be a single line" ;;
esac
[ -n "$VERSION" ] || fail "version is empty"
[ "${#VERSION}" -le 64 ] || fail "version is longer than 64 characters"
printf '%s\n' "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$' ||
    fail "version is not semver X.Y.Z[-pre] (no v prefix, no +build)"

# 3. confirm
[ "$CONFIRM" = "release v$VERSION" ] || fail "confirm must be exactly: release v$VERSION"

# 4. commit
printf '%s\n' "$MAIN_HEAD" | grep -Eq '^[0-9a-f]{40}$' || fail "MAIN_HEAD is not a 40-hex commit"
if [ -z "$SHA" ]; then
    SHA=$MAIN_HEAD
else
    case $SHA in
        *"$nl"*) fail "sha must be a single line" ;;
    esac
    printf '%s\n' "$SHA" | grep -Eq '^[0-9a-f]{7,40}$' || fail "sha must be 7-40 lowercase hex characters"
    SHA=$(git rev-parse --verify --quiet "$SHA^{commit}") || fail "sha is unknown or ambiguous in this checkout"
fi
printf '%s\n' "$SHA" | grep -Eq '^[0-9a-f]{40}$' || fail "sha did not resolve to a full commit id"

# 5. ancestor of main
git merge-base --is-ancestor "$SHA" "$MAIN_HEAD" || fail "commit $SHA is not on main (not an ancestor of the head of main)"

# 6. Cargo.toml workspace version at the commit
cargo=$(git show "$SHA:Cargo.toml") || fail "cannot read Cargo.toml at $SHA"
cargo_version=''
in_ws=0
while IFS= read -r line; do
    case $line in
        '[workspace.package]'*) in_ws=1 ;;
        '['*) in_ws=0 ;;
        'version = "'*)
            if [ "$in_ws" = 1 ]; then
                v=${line#version = \"}
                cargo_version=${v%%\"*}
                break
            fi
            ;;
    esac
done <<EOF
$cargo
EOF
[ -n "$cargo_version" ] || fail "no [workspace.package] version in Cargo.toml at $SHA"
[ "$cargo_version" = "$VERSION" ] || fail "Cargo.toml workspace version at $SHA is $cargo_version, not $VERSION"

# 7. CHANGELOG heading and no duckdb in the section
changelog=$(git show "$SHA:CHANGELOG.md") || fail "cannot read CHANGELOG.md at $SHA"
found=0
in_sec=0
section=''
while IFS= read -r line; do
    if [ "$in_sec" = 1 ]; then
        case $line in
            '## '*) break ;;
        esac
        section="$section$line$nl"
    elif [ "$line" = "## [$VERSION]" ]; then
        found=1
        in_sec=1
    else
        case $line in
            "## [$VERSION] "*)
                found=1
                in_sec=1
                ;;
        esac
    fi
done <<EOF
$changelog
EOF
[ "$found" = 1 ] || fail "CHANGELOG.md at $SHA has no '## [$VERSION]' heading"
if printf '%s' "$section" | grep -qi 'duckdb'; then
    fail "CHANGELOG.md section [$VERSION] mentions duckdb (DuckDB is out of 1.0)"
fi

# 8. release.yml at the commit must know the tag dispatch
git show "$SHA:.github/workflows/release.yml" | grep -q '^# tag-dispatch: ' ||
    fail "release.yml at $SHA predates R18 (no tag-dispatch marker); a dispatch on the tag would be a dry run"

# 9. tag absent
out=$(tag_exists "v$VERSION") || fail "could not list tags through the API"
[ -z "$out" ] || fail "tag v$VERSION already exists"

# 10. Release dry run on exactly this commit
out=$(latest_release_dry_run) || fail "could not read the latest Release dry run"
[ -n "$out" ] || fail "no Release dry run found on main; run the Release workflow on main first"
[ "$out" != "${out#"completed${tab}success${tab}"}" ] ||
    fail "the latest Release dry run on main did not complete successfully; run the Release workflow on main first"
dry_sha=${out#"completed${tab}success${tab}"}
[ "$dry_sha" = "$SHA" ] ||
    fail "the latest successful Release dry run is for commit $dry_sha, not $SHA; run the Release workflow on main first"

# 11. CI on exactly this commit
out=$(ci_run_on "$SHA") || fail "could not read the CI run for $SHA"
[ "$out" = "completed${tab}success" ] || fail "CI has no successful push run on main for commit $SHA"

result_out=${GITHUB_OUTPUT:-/dev/stdout}
{
    printf 'tag=v%s\n' "$VERSION"
    printf 'sha=%s\n' "$SHA"
    printf 'version=%s\n' "$VERSION"
} >>"$result_out"
printf 'tag-release-validate: ok (tag v%s, commit %s)\n' "$VERSION" "$SHA"
