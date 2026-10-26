# ADR-0074: PR checks and post-release benchmarks

- Status: accepted
- Date: 2026-10-26
- Deciders: SQLer maintainers

## Context and Problem Statement

`ci.yml` alone (fmt, clippy, tests, docs, deny, spdx) measured 9 to 13 minutes on pull requests
([benchmarks.md](../benchmarks.md#ci-timings)). A PR still waited 15 to 24 minutes, because the workflows that were
triggered beside it ran longer: `Bench` (1134 s), `Binary size` (734 s), the release build on a PR touching a release
file, and the two spikes. Nothing in them blocks a merge, yet they occupy the same runner pools as the required checks,
`check (macos-latest)` in particular, and they make a PR look slower than its checks are.

## Decision Drivers

- A PR runs the format, lint and test checks only. Nothing on a PR builds a release bundle, runs benchmarks or
  measures the binary size.
- The measurements stay available: on `main`, on demand, and after a release is published.
- No gate may depend on a benchmark, and the release pipeline keeps only what publishes.

## Considered Options and Decision Outcome

Adopted:

1. **Trigger matrix.** Only `ci.yml` has a `pull_request` trigger.

   | Workflow | pull_request | push `main` | dispatch | other |
   | -------- | ------------ | ----------- | -------- | ----- |
   | `ci.yml` | yes | yes | yes (`full`) | - |
   | `bench.yml` | no | yes | yes | - |
   | `benchmarks.yml` | no | no | yes | `workflow_run` of `Release`, publishing runs only |
   | `size.yml` | no | yes | yes | - |
   | `spike.yml`, `plugin-spike.yml` | no | yes | yes | - |
   | `release.yml` | no | tags `v*` | yes (dry run; publishes on a `refs/tags/v*` ref) | - |

   `ci.yml` also stops running the advisory `screenshots (macos)` on PRs: it competes with `check (macos-latest)` for
   the macOS pool. It still runs on `main` and on dispatch.
2. **`bench.yml`** keeps absolute budgets on push to `main`. A dispatch on a branch compares the branch with its
   merge-base with `origin/main`, interleaved as before; this is the way to get the before/after numbers that
   AGENTS.md section 4 asks for.
3. **`size.yml`** always builds the three OSes (the PR-only Linux subset and the `full-ci` label are gone).
4. **`benchmarks.yml`** runs after a successful *publishing* `Release` run (`workflow_run`, head ref `vX.Y.Z[-pre]`,
   the GitHub Release exists) or by hand against such a run. A run on a branch is refused. It never gates `release.yml`
   or Tag release; a red run is a follow-up.
5. **`release.yml`** drops the `pull_request` trigger and everything that served it: the `full` flag, the
   `pr_run_mode` build condition, the Linux-only plan filter, `--partial` on the dry-run manifest and the `announce`
   job (it did nothing). Every dry run now builds all five targets. `dist-workspace.toml` sets `pr-run-mode = "skip"`
   to match. The `tag-dispatch` marker and the guards, signing, manifest and `host` jobs are untouched.
6. Each non-CI workflow gets one concurrency group per run and is never cancelled.

Amends: [ADR-0068](0068-ci-pipeline-speed.md) option 7 (`Bench` and `Binary size` on PRs) and
[ADR-0064](0064-release-pipeline.md) (the path-gated PR trigger of `release.yml`).

Rejected:

- `on: release: published` for `benchmarks.yml`. `host` creates the release with `GITHUB_TOKEN`, and an event raised
  by that token never starts another workflow.
- `workflow_run` chaining of Bench and Size behind CI (as in ADR-0068): it runs in the base-repository context, close
  to `pull_request_target`.
- Keeping a Linux release build on PRs: it is a bundle build, which the PR checks must not contain.

## Consequences

- A PR that changes a release file or `crates/sqler-update/keys/**` is not built on the PR. Dry-run it with
  `gh workflow run release.yml --ref <branch>`. Tag release requires a green dry run on `main` anyway, so the DuckDB
  guard and the update-manifest dry run still run before every release, and now always over all five targets.
- Benchmark regressions and size growth surface on a branch dispatch or after the merge on `main`, not on the PR.
- Release artifacts last 7 days, so a manual `benchmarks.yml` dispatch later than that fails with a clear error.
- `pr-run-mode = "skip"` was checked with dist 0.32.0: `dist plan --output-format=json` still lists all 5 entries in
  `.ci.github.artifacts_matrix.include` (the plan the dry run builds from) and reports `pr_run_mode` as `skip`.

## Rollback

Revert the commit: the previous workflows and `dist-workspace.toml` are restored whole.
