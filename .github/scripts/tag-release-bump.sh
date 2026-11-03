#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Version bump of the "Tag release" workflow (ADR-0064 amendment 4). Validates everything first and writes nothing until
# every check passed; a failed check prints one line "::error::<reason>" and exits 1. Inputs are environment variables:
#   BUMP     patch|minor|major, used only when VERSION is empty
#   VERSION  optional exact X.Y.Z[-pre] (no "v"), takes precedence over BUMP
#   ROOT     repo root, default "."
#   GITHUB_OUTPUT  optional; when set, appends old=, version=, tag=, changed=
# Bump arithmetic follows npm: a pre-release is "finished" by the bump that would reach its base version.
# Newline and CR are matched against literal variables, never `$(printf '\n')`: command substitution strips trailing
# newlines, which would turn the pattern into `**` and match every value.
set -eu

: "${BUMP:=}" "${VERSION:=}" "${ROOT:=.}"

nl='
'
cr=$(printf '\r')
semver='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'

fail() {
    printf '::error::%s\n' "$1"
    exit 1
}

is_semver() {
    case $1 in
        *"$nl"* | *"$cr"*) return 1 ;;
    esac
    [ "${#1}" -le 64 ] && printf '%s\n' "$1" | grep -Eq "$semver"
}

# The same expression as release.yml (`mode` step).
read_version() {
    sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\([^"]*\)".*/\1/p;}' "$ROOT/Cargo.toml" | head -n 1
}

# Number of internal manifest entries that are not at version $1 (Cargo.toml path deps, Cargo.lock workspace crates).
count_bad() {
    bad_toml=$(awk -v v="$1" '
        /^sqler-[a-z-]+ = \{ path = "crates\// { if (index($0, "version = \"" v "\"") == 0) n++ }
        END { print n + 0 }' "$ROOT/Cargo.toml")
    bad_lock=$(awk -v v="$1" '
        want { if ($0 != "version = \"" v "\"") n++; want = 0 }
        /^name = "(sqler-[a-z-]+|xtask)"$/ { want = 1 }
        END { print n + 0 }' "$ROOT/Cargo.lock")
    echo $((bad_toml + bad_lock))
}

[ -f "$ROOT/Cargo.toml" ] && [ -f "$ROOT/Cargo.lock" ] || fail "Cargo.toml or Cargo.lock not found"

# 1. current version
old=$(read_version)
is_semver "$old" || fail "the workspace version in Cargo.toml is not semver"

# 2. target version
if [ -n "$VERSION" ]; then
    is_semver "$VERSION" || fail "version must be semver X.Y.Z[-pre], single line, no v prefix, at most 64 chars"
    new=$VERSION
else
    case $BUMP in
        patch | minor | major) ;;
        *) fail "bump must be patch, minor or major" ;;
    esac
    core=${old%%-*}
    pre=
    [ "$core" = "$old" ] || pre=${old#*-}
    major=${core%%.*}
    rest=${core#*.}
    minor=${rest%%.*}
    patch=${rest#*.}
    case $BUMP in
        patch)
            if [ -n "$pre" ]; then new=$core; else new=$major.$minor.$((patch + 1)); fi ;;
        minor)
            if [ -n "$pre" ] && [ "$patch" = 0 ]; then new=$core; else new=$major.$((minor + 1)).0; fi ;;
        major)
            if [ -n "$pre" ] && [ "$minor" = 0 ] && [ "$patch" = 0 ]; then new=$core; else new=$((major + 1)).0.0; fi ;;
    esac
    is_semver "$new" || fail "computed version $new is not semver"
fi

# 3. tag absent
if git -C "$ROOT" rev-parse -q --verify "refs/tags/v$new" >/dev/null 2>&1; then
    fail "the tag v$new already exists"
fi

# 4. release notes heading
[ -f "$ROOT/CHANGELOG.md" ] && awk -v v="$new" 'index($0, "## [" v "]") == 1 { f = 1 } END { exit !f }' "$ROOT/CHANGELOG.md" ||
    fail "CHANGELOG.md has no heading starting with: ## [$new]"

# 5. manifests consistent
[ "$(count_bad "$old")" -eq 0 ] || fail "manifests are not all at $old"

# 6. edit, only when the version changes
changed=false
if [ "$new" != "$old" ]; then
    changed=true
    tmp_toml=$(mktemp "$ROOT/.bump-toml.XXXXXX")
    tmp_lock=$(mktemp "$ROOT/.bump-lock.XXXXXX")
    trap 'rm -f "$tmp_toml" "$tmp_lock"' EXIT
    awk -v old="$old" -v new="$new" '
        /^\[/ { pkg = ($0 == "[workspace.package]") }
        pkg && !done && /^version *= *"/ { print "version = \"" new "\""; done = 1; next }
        /^sqler-[a-z-]+ = \{ path = "crates\// {
            i = index($0, "version = \"" old "\"")
            if (i > 0) $0 = substr($0, 1, i - 1) "version = \"" new "\"" substr($0, i + length("version = \"" old "\""))
        }
        { print }' "$ROOT/Cargo.toml" >"$tmp_toml"
    awk -v old="$old" -v new="$new" '
        want { if ($0 == "version = \"" old "\"") $0 = "version = \"" new "\""; want = 0 }
        /^name = "(sqler-[a-z-]+|xtask)"$/ { want = 1 }
        { print }' "$ROOT/Cargo.lock" >"$tmp_lock"
    mv "$tmp_toml" "$ROOT/Cargo.toml"
    mv "$tmp_lock" "$ROOT/Cargo.lock"
    trap - EXIT

    # 7. re-read
    [ "$(read_version)" = "$new" ] || fail "Cargo.toml version is not $new after the bump"
    [ "$(count_bad "$new")" -eq 0 ] || fail "manifests are not all at $new after the bump"
fi

if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
        printf 'old=%s\n' "$old"
        printf 'version=%s\n' "$new"
        printf 'tag=v%s\n' "$new"
        printf 'changed=%s\n' "$changed"
    } >>"$GITHUB_OUTPUT"
fi
echo "tag-release-bump: $old -> $new (changed=$changed)"
