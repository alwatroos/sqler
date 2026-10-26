#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Read-only CI timing collector. Prints Markdown tables; changes nothing
# (only GET requests and `gh run list/view`), never reads stdin, always ends.
#
# Usage:
#   ci-timings.sh <workflow-file> <limit> [<filter>] [--cache-log]
#       <workflow-file>  e.g. ci.yml, bench.yml, size.yml
#       <limit>          completed runs to read, 1..20 (clamped)
#       <filter>         a branch name (e.g. main) or an event
#                        (pull_request, push, workflow_dispatch); default: all
#       --cache-log      also read each job log for the rust-cache result
#                        (one extra API call per job; bounded by <limit>)
#   ci-timings.sh --caches       cache usage and inventory (GET)
#   ci-timings.sh --protection   branch protection / rulesets reading (GET)
#   ci-timings.sh --commit <sha> every workflow run of one commit (7..40 hex characters) with its wall time, and the
#                                wall time of the pull_request runs (the longest one is what a PR waits for)
#
# Queue = job startedAt - run createdAt; run = job completedAt - startedAt.
# Needs: gh (authenticated), jq.

set -eu

REPO="${CI_TIMINGS_REPO:-alwatroos/sqler}"
MAX_LIMIT=20

die() {
  echo "ci-timings: $*" >&2
  exit 2
}

command -v gh >/dev/null 2>&1 || die "gh is required"
command -v jq >/dev/null 2>&1 || die "jq is required"

caches() {
  echo "## Cache usage"
  echo
  gh api "repos/$REPO/actions/cache/usage" \
    --jq '"- active caches: \(.active_caches_count), size: \(.active_caches_size_in_bytes / 1048576 | floor) MiB of 10240 MiB"'
  echo
  echo "## Cache inventory (up to 100, newest used first)"
  echo
  echo "| ref | key | MiB | last accessed | created |"
  echo "| --- | --- | --: | --- | --- |"
  gh api "repos/$REPO/actions/caches?per_page=100&sort=last_accessed_at&direction=desc" |
    jq -r '.actions_caches[] | "| \(.ref) | `\(.key | .[0:70])` | \(.size_in_bytes / 1048576 | floor) | \(.last_accessed_at) | \(.created_at) |"'
}

protection() {
  echo "## Branch protection (main)"
  echo
  if gh api "repos/$REPO/branches/main/protection" >/dev/null 2>&1; then
    gh api "repos/$REPO/branches/main/protection" \
      --jq '"- required status checks: \(.required_status_checks.contexts // [] | join(", "))"'
  else
    echo "- classic protection: not readable or not set (404/403)"
  fi
  gh api "repos/$REPO/branches/main" --jq '"- branch API: protected=\(.protected), \(.protection | tostring)"' 2>&1 || true
  echo "- rulesets:"
  ids=$(gh api "repos/$REPO/rulesets" --jq '.[].id' 2>/dev/null || true)
  if [ -z "$ids" ]; then
    echo "  - none readable"
  fi
  for id in $ids; do
    gh api "repos/$REPO/rulesets/$id" \
      --jq '"  - \(.name) (\(.enforcement)): rules = \([.rules[].type] | join(", ")); required checks = \([.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[].context] | join(", "))"' \
      2>&1 || echo "  - ruleset $id: not readable"
  done
}

commit() {
  sha="$1"
  case "$sha" in
    '' | *[!0-9a-fA-F]*) die "--commit needs a hex sha (7..40 characters)" ;;
  esac
  [ "${#sha}" -ge 7 ] && [ "${#sha}" -le 40 ] || die "--commit needs a hex sha (7..40 characters)"

  list=$(gh run list --repo "$REPO" --commit "$sha" --limit 50 \
    --json databaseId,workflowName,event,headBranch,conclusion,createdAt,updatedAt)

  echo "## Runs of commit $sha"
  echo
  echo "| run | workflow | event | conclusion | wall s |"
  echo "| --- | --- | --- | --- | --: |"
  printf '%s' "$list" | jq -r '
    def ts: fromdateiso8601;
    .[] | "| \(.databaseId) | \(.workflowName) | \(.event) | \(.conclusion) | \((.updatedAt | ts) - (.createdAt | ts)) |"'
  echo
  printf '%s' "$list" | jq -r '
    def ts: fromdateiso8601;
    [ .[] | select(.event == "pull_request") ] as $p
    | "- pull_request wall: \(if ($p | length) > 0 then ([$p[] | (.updatedAt | ts) - (.createdAt | ts)] | max) else 0 end) s "
      + "(\($p | length) runs: \([$p[].workflowName] | unique | join(", ")))"'
}

runs() {
  wf="$1"
  limit="$2"
  filter="${3:-}"
  cache_log="${4:-}"

  case "$limit" in
    '' | *[!0-9]*) die "limit must be a number" ;;
  esac
  [ "$limit" -ge 1 ] || die "limit must be >= 1"
  [ "$limit" -le "$MAX_LIMIT" ] || limit="$MAX_LIMIT"

  filter_args=""
  case "$filter" in
    '') ;;
    pull_request | push | workflow_dispatch | schedule) filter_args="--event $filter" ;;
    *) filter_args="--branch $filter" ;;
  esac

  # Completed runs only; cancelled and failed ones are kept (they matter).
  # shellcheck disable=SC2086
  list=$(gh run list --repo "$REPO" --workflow "$wf" --limit "$limit" --status completed $filter_args \
    --json databaseId,headSha,event,headBranch,createdAt,startedAt,updatedAt,conclusion)

  echo "## $wf (filter: ${filter:-none}, last $limit completed)"
  echo
  echo "| run | sha | event | branch | conclusion | wall s | first-job queue s | jobs |"
  echo "| --- | --- | --- | --- | --- | --: | --: | --- |"

  detail=""
  for id in $(printf '%s' "$list" | jq -r '.[].databaseId'); do
    run=$(printf '%s' "$list" | jq -c --argjson id "$id" '.[] | select(.databaseId == $id)')
    view=$(gh run view "$id" --repo "$REPO" --json jobs 2>/dev/null || echo '{"jobs":[]}')
    row=$(jq -rn --argjson r "$run" --argjson v "$view" '
      def ts: fromdateiso8601; def ok: (.completedAt // "") > "2000" and (.startedAt // "") > "2000";
      ($r.createdAt | ts) as $c
      | [ $v.jobs[] | select(ok) ] as $j
      | "| \($r.databaseId) | \($r.headSha[0:7]) | \($r.event) | \($r.headBranch) | \($r.conclusion) "
        + "| \((($r.updatedAt | ts) - $c)) "
        + "| \(if ($j | length) > 0 then (([$j[] | (.startedAt | ts) - $c] | min)) else "-" end) "
        + "| \(($j | length)) |"')
    echo "$row"
    jobrows=$(jq -rn --argjson r "$run" --argjson v "$view" '
      def ts: fromdateiso8601; def ok: (.completedAt // "") > "2000" and (.startedAt // "") > "2000";
      ($r.createdAt | ts) as $c
      | $v.jobs[] | select(ok)
      | . as $job
      | [ $job.steps[]? | select(ok)
          | {n: .name, d: ((.completedAt | ts) - (.startedAt | ts))} ]
        | sort_by(-.d) | .[0:3] | map("\(.n) \(.d)s") | join("; ") as $top
      | "| \($r.databaseId) | \($job.name) | \($job.conclusion) | \((($job.startedAt | ts) - $c)) "
        + "| \((($job.completedAt | ts) - ($job.startedAt | ts))) | \($top) |"')
    detail="$detail
$jobrows"
    if [ "$cache_log" = "--cache-log" ]; then
      for jid in $(printf '%s' "$view" | jq -r '.jobs[] | select((.completedAt // "") > "2000") | .databaseId'); do
        res=$(gh run view --repo "$REPO" --job "$jid" --log 2>/dev/null |
          grep -E 'Cache hit occurred|Cache restored|No cache found|Cache not found' | head -3 | tr '\n' ' ' | cut -c1-200 || true)
        [ -z "$res" ] || detail="$detail
| $id | job $jid cache | - | - | - | ${res} |"
      done
    fi
  done

  echo
  echo "### Per job (queue = job start - run created; run = job duration; top-3 steps)"
  echo
  echo "| run | job | conclusion | queue s | run s | top-3 steps |"
  echo "| --- | --- | --- | --: | --: | --- |"
  printf '%s\n' "$detail" | sed '/^$/d'
}

[ "$#" -ge 1 ] || die "usage: ci-timings.sh <workflow-file> <limit> [branch|event] [--cache-log] | --caches | --protection"

case "$1" in
  --caches) caches ;;
  --protection) protection ;;
  --commit)
    [ "$#" -eq 2 ] || die "usage: ci-timings.sh --commit <sha>"
    commit "$2"
    ;;
  -*) die "unknown option $1" ;;
  *)
    [ "$#" -ge 2 ] || die "usage: ci-timings.sh <workflow-file> <limit> [branch|event] [--cache-log]"
    f3="${3:-}"
    f4="${4:-}"
    if [ "$f3" = "--cache-log" ]; then
      f4="$f3"
      f3=""
    fi
    runs "$1" "$2" "$f3" "$f4"
    ;;
esac
