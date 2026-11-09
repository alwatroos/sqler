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
8. A `workflow_dispatch` input `full` (default true) lets a dispatch run the PR subsets; it is how the warm PR path is
   measured before a merge, since a PR cannot read caches saved by its own branch.
9. apt steps use `Acquire::Retries=3` and an HTTP timeout of 20 s: a mirror stall cost 111 to 477 s in one run
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
  (a 15.9 s test): it is in the Windows PR subset, so a flake shows up on PRs. On Linux,
  `sqler-ui::workspace::testing::ai_session_flow::cancel_while_a_statement_runs_asks_the_database_to_stop` failed once
  (run 39909579173, `testing.rs:2789`) and passed on the next run. Neither is caused by this change; both are follow-ups.

## Rollback

Revert the commit: the previous workflows are restored whole. A single lever: set `full` to `true` in the `Resolve
outputs` step of `ci.yml` to run the full matrix everywhere; add `save-if: true` to a rust-cache step to save from PRs.

## Amendment 1 (2026-11-09, ci-speedup): `full=false` dispatches skip `duckdb`; macOS clippy is its own job

- Reason: PR runs stayed slow after the first round of this ADR (median 1474 s, max 2678 s over the last 10 full
  `pull_request` runs of `ci.yml`), and `check (macos-latest)` was the last job to finish in 6 of those 10 runs.
  Its `Clippy` step took a median of 260 s (164 to 373 s).
- Rule L0: a `workflow_dispatch` with `full=false` no longer runs the `duckdb` job (up to 120 min, not part of
  `CI result`). A default or `full=true` dispatch still runs it. The timing dispatches below need this: without it every
  one would hold a runner for the DuckDB build.
- Lever L2, kept provisionally: `Clippy` and `Clippy (macOS PR subset)` moved from `check (macos-latest)` to a new job
  `clippy (macos-latest)`, the same shape as `clippy (windows-latest)` (option 4 above). The commands and the `full`
  conditions are unchanged, `check`'s `Clippy` step now runs on Linux only, and `CI result` needs the new job. The
  rust-cache key is `clippy` and is saved on `main` only (no change to `save-if`). The keep rule requires a last check
  on the PR's own run (not slower than the 1474 s median); see the PR description.
- Lever L1 (a Windows Dev Drive under `target`): not applied. Windows was the last job in 2 of the 10 runs and
  `clippy (windows-latest)` in none.

Baseline (`pull_request` runs of `ci.yml`, 2026-11-09; created until `CI result`, wall seconds). Median of the 10 full
runs 1474 s, max 2678 s; `check (macos-latest)` was the critical job in 6 of 10, Windows in 2, `plugins (macos-latest)` in 1,
a runner wait in 1. Representative runs:

| run | wall | critical job | its top steps |
| --- | ---: | ------------ | ------------- |
| 41691781645 | 1162 s (min) | `check (macos-latest)` | Test 502 s; Clippy 199 s |
| 41708984646 | 1338 s | `check (macos-latest)` | Test macOS subset 489 s; Clippy macOS subset 290 s |
| 41677566822 | 1546 s | `check (windows-latest)` | Test 809 s |
| 41696427677 | 2678 s (max) | `check (macos-latest)` | Test 516 s; Clippy 373 s |

Measurements (`workflow_dispatch`, `full=false`, branch `ci-speedup`, one after the other; created until `CI result`
completed):

| run | commit | change | created to `CI result` | last job (done at) | `check (macos-latest)` | `clippy (macos-latest)` |
| --- | ------ | ------ | ---------------------: | ------------------ | ---------------------: | ----------------------: |
| 42610348541 | 19be883 | before (L0 only) | 1368 s | `check (ubuntu-latest)` 1363 s | 225 s (clippy in it) | n/a |
| 42613367016 | 1d708eb | L2 | 866 s | `check (windows-latest)` / `plugins (windows-latest)` 861 s | 346 s | 421 s (cold cache) |
| 42615363661 | 1d708eb | L2 | 1061 s | `check (ubuntu-latest)` 1056 s | 590 s | 347 s (cold cache) |

- Both after-runs beat the before-run by at least 60 s and 10 % (502 s, 37 %; 307 s, 22 %).
- The gain is not caused by the macOS split. In all three runs `check (macos-latest)` finished long before the end,
  and `check (ubuntu-latest)` was the last job in two. The Linux, Windows and `clippy (windows-latest)` caches were
  cold in every run (the cache store is at its 10 GB limit, with only `release-*` and one macOS `debug` set on
  `main`), so their times swing by several hundred seconds: `check (ubuntu-latest)` ran 1354, 438 (it failed early on
  a flaky `sqler-config::chat_history` test) and 1049 s. The split removes the clippy step from the macOS test job,
  which is the critical job only when the macOS caches are warm and the Linux and Windows ones are too.
  Treat the 37 % and 22 % as an upper bound.
- `CI result` was red in all three dispatches because `cargo deny` reports RUSTSEC-2026-0459 (`libsqlite3-sys`); the
  lockfile is the one on `main`, so it is not part of this change.
- Consequences: one more macOS runner per Rust PR competes for the shared pool (ADR-0074). `clippy (macos-latest)` is
  cold until the first run on `main` saves its cache; check that it saves after the merge.
- Rollback: L0, set the `duckdb` `if` back to `github.event_name == 'workflow_dispatch'`. L2, revert the commit that adds
  `clippy-macos`: the two Clippy steps return to `check`, and the amendment above no longer holds.
