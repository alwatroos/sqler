# ADR-0068: CI pipeline speed

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

A pull request waited 15 to 24 minutes for `CI` (median 22.7 minutes on PRs, 20.6 on `main`), with `Bench`, `Binary
size` and the two spike workflows starting beside it (about 16 jobs per push). `check (windows-latest)` was the critical
path (median 1362 s: tests 744 s, clippy 283 s, cache save 186 s). Measurements are in
[benchmarks.md](../benchmarks.md#ci-timings); the raw collector is `.github/scripts/ci-timings.sh`.

Three causes, from the data:

- Cache churn. Every job saved its own cache on every run, so PR runs wrote about 10 GB and evicted `main`'s caches
  (the repository limit is 10 GB; the full set is 13 to 14 GB). A new PR started cold; `clippy (windows)` ranged from
  312 s (cache hit) to 1696 s (miss).
- Everything ran on every push: docs-only PRs built the workspace three times, `Binary size` built three release
  trees, the spikes fired on the push and on the pull request of the same SHA, and `ci.yml` cancelled runs on `main`.
- The slowest OS did the most work: the whole Windows suite and clippy in one job.

## Decision Drivers

- Required PR feedback in about 8 to 10 minutes on a typical PR.
- No loss of coverage on `main`: every job that ran on a push to `main` still runs there, on all three OSes.
- No change to repository settings (the owner's). No check is required today (no classic protection, a ruleset without
  required status checks), but every check name stays byte-identical in case one is added.
- Workflow security: minimal permissions, no `pull_request_target`, pinned major versions, only `actions/*`,
  `Swatinem`, `taiki-e`, `dtolnay` and `dorny` actions.

## Considered Options and Decision Outcome

Adopted (each with its number from the baseline):

1. Caches are saved on `main` only (`save-if`), PRs restore. Keys drop the rustc-version hash (an extra preinstalled
   1.99.0 on some images split one cache into two keys) and use `rust-toolchain.toml` instead. `check`, `screenshots`,
   `db` and `docs` share the key `debug`; only `check` saves it. `deny` has no cache (it compiles nothing).
   `CARGO_PROFILE_DEV_DEBUG=line-tables-only` shrinks the caches (Linux 2025 to 1693 MiB, macOS 1579 to 1354 MiB,
   Windows about 1700 to 1079 MiB) and keeps panic locations.
2. A `changes` job (`dorny/paths-filter`) computes `rust`, `ui`, `plugins`; everything is true on `main`, on
   `workflow_dispatch` and on a PR labelled `full-ci`. Non-matrix jobs use a job-level `if` (a skipped job counts as
   passed). Matrix jobs (`check`, `plugins`) are never skipped with `if`, because a skipped matrix job reports the
   unexpanded name `check (${{ matrix.os }})`; they run, skip their steps and fall back to an ubuntu runner so a no-op
   costs no macOS or Windows minutes.
3. `CI result`: one extra aggregate job that fails when any CI job failed or was cancelled (skipped passes).
   `screenshots (macos)` is advisory and left out.
4. Windows clippy is its own job, `clippy (windows-latest)`, so it runs beside the tests. Linux and macOS keep clippy
   in `check` (145 s and 98 s cold, not on the critical path). `check (windows-latest)` keeps its name.
5. PR subsets, full matrix on `main`, on dispatch and with `full-ci`:
   - Windows: build and test only the crates with Windows-specific history by `-p` (`sqler-plugin-host`,
     `sqler-plugin-api`, `sqler-config`, `xtask`, `sqler-acp`, `sqler-mcp`, `sqler-cli`, `sqler-tunnel`) plus the
     `agent_flow` and `guard_suite` groups of `sqler-ui`. Basis: 5 of the 9 failed `check` jobs in the last 40 runs
     were Windows-only, in exactly these crates; none was macOS-only.
   - macOS: clippy and nextest for the workspace minus `MAC_SKIP` (SQL, catalog, completion, drivers, exporters, AI
     clients). The macOS-specific code is in `sqler-config` and `sqler-ui` only. Linux always runs everything.
6. Concurrency cancels PR runs only; `main` and dispatch runs have their own group. (Before, 2 of 20 `main` runs of
   `ci.yml` and 1 of 20 of `size.yml` were cancelled: a coverage hole.)
7. `Bench` also runs on push to `main` (it had no run on `main`) and on PRs that touch the benched crates; base builds
   reuse the workspace `target/`. `Binary size` builds Linux only on PRs (all OSes on `main`, dispatch and `full-ci`),
   with `timeout-minutes: 45`; the crates table moved to its own `top crates` job (main only). `spike.yml` and
   `plugin-spike.yml` trigger on `push` to `main` only, so a PR branch no longer runs them twice.
8. apt steps use `Acquire::Retries=3` and an HTTP timeout of 20 s: a mirror stall cost 111 to 477 s in one run
   against a 16 s median.

Rejected, with the number:

- nextest sharding with archives: execution is 185 to 261 s, the build (355 to 376 s) is not shardable, and an archive
  needs an upload, a download and a second runner.
- `workflow_run` chaining of Bench and Size behind CI: it runs in the base-repository context, close to
  `pull_request_target`.
- sccache (needs a storage backend), rust-lld and `CARGO_INCREMENTAL=0` (already the default or already exported),
  caching the toolchain (10 to 14 s) and apt packages (median 16 s), a macOS or Windows split of `check` into build
  and test jobs (the build artifacts would have to travel between runners; the jobs are one build deep).
- The 70-minute `Binary size` run (39824753263) was a cancelled run whose jobs ended after 5 minutes; nothing to fix.

## Consequences

- A docs-only PR runs `changes`, `docs` (mdBook), `spdx` and `CI result`.
- The first run on `main` after a `Cargo.lock` or toolchain change is cold and fills the caches; a PR that changes
  them is cold too. A PR run never saves, so it cannot evict `main`'s caches.
- A bug visible only on macOS in a platform-agnostic crate, or on Windows outside the listed crates, is caught on
  `main` after the merge, not on the PR. The mitigation is the `full-ci` label, and `main` stays the safety net. A crate
  that gains OS-specific code must leave `MAC_SKIP` and join the Windows list.
- Windows clippy is not part of `check (windows-latest)`. If a required-checks rule is added later, require
  `CI result` (or both `check (windows-latest)` and `clippy (windows-latest)`).
- `sqler-ui::guard_suite::s8_every_call_leaves_one_equal_audit_entry` failed once on Windows in the measurement runs
  (a 15.9 s test): it is in the Windows PR subset, so a flake shows up on PRs. Recorded as a follow-up.

## Rollback

Revert the commit: the previous workflows are restored whole. A single lever: set `full` to `true` in the `Resolve
outputs` step of `ci.yml` to run the full matrix everywhere; add `save-if: true` to a rust-cache step to save from PRs.
