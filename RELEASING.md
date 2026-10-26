# Releasing SQLer

This is the checklist for cutting a release such as `v1.0.0`. The pipeline is `.github/workflows/release.yml`
(cargo-dist, configured in `dist-workspace.toml`); the design is in
[ADR-0064](docs/src/adr/0064-release-pipeline.md).

## Scope

- Releases are published on GitHub Releases only. There is no Homebrew, winget, Scoop or crates.io package (owner
  decision 4).
- The owner cuts the release by running the "Tag release" workflow (see "Tag-time steps (owner)"). The team never
  runs "Tag release", never dispatches it, never pushes a tag and never creates a Release. The team only prepares
  `main` (the readiness items, the dry run) and hands over. The owner announces.
- Nothing outside this repository is posted by the team.
- Use read-only `gh` calls while checking. Never print a secret value; check presence by name only.

## Pre-flight

Tick every item before tagging. Scratch files go in a scratch directory outside the repository and are deleted
afterwards.

- [ ] CI is green on Linux, macOS and Windows for the head of `main`:
  `gh run list --workflow ci.yml --branch main --limit 1` shows `completed success`, and the `CI result` check passed.
- [ ] `cargo deny check` is green (the CI `deny` job).
- [ ] The release feature line in `dist-workspace.toml` is exactly `features = ["plugins", "driver-mssql"]`, with no
  `driver-duckdb`, and `sh .github/scripts/release-guard.sh features` passes (the `guard (features)` job runs the same
  check).
- [ ] The final full Release dry run on `main` is green: `gh workflow run release.yml --ref main` (no inputs). A
  dispatch is always a full dry run and never publishes. Watch it with
  `gh run list --workflow release.yml --event workflow_dispatch --branch main --limit 1` and `gh run view <id>`.
  - [ ] The `update manifest (dry run)` job is green.
  - [ ] Size: each `build (...)` job prints a "Size report" line. The binary must be at most 83,886,080 bytes
    (80 MB) on every OS. Record the binary bytes and the headroom for each of the 5 targets in the release PR or
    issue. If the Linux binary is over 83,886,080 bytes, stop and ask the owner (decision 12). The size step only
    warns, so read the line.
- [ ] The Pending (maintainer) items of AGENTS.md section 9 are cleared or explicitly waived by the owner, including
  the real SQL Server instance run.
- [ ] The environment `release` is restricted to tags `v*`:
  `gh api repos/alwatroos/sqler/environments/release/deployment-branch-policies --jq '.branch_policies[] | [.name, .type] | @tsv'`
  prints `v*` and `tag`.
- [ ] The update signing secret exists in the environment `release`, by name only:
  `gh api repos/alwatroos/sqler/environments/release/secrets --jq '.secrets[].name'` lists `SQLER_UPDATE_SIGNING_KEY`.
  **Blocker if absent.** The `update manifest` job then prints a notice and succeeds, and `host` publishes the release
  without `sqler-update.json`, so clients never see the update. Do not tag until it is present. Never print the value.
- [ ] The compiled-in update key id is `9735ae5e612eaa24` (`crates/sqler-update/keys/9735ae5e612eaa24.pub`). It is
  the key behind the secret; see [update-feed.md](docs/src/releasing/update-feed.md).
- [ ] The six Apple secrets exist in the environment `release`, by name only:
  `gh api repos/alwatroos/sqler/environments/release/secrets --jq '.secrets[].name'` lists `APPLE_CERTIFICATE_P12_BASE64`,
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_TEAM_ID`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID` and
  `APPLE_API_KEY_P8_BASE64`. **Blocker if any is absent.** The tag run then fails in `sign (macOS)` and nothing
  publishes. The variable `SQLER_ALLOW_UNSIGNED_FALLBACK` is no longer read. Never print a value. Details:
  [signing.md](docs/src/releasing/signing.md).
- [ ] Windows is unsigned by owner decision: there is no step and no secret for it.
- [ ] The release notes preview works. cargo-dist is installed in a scratch root at the version in
  `dist-workspace.toml`
  (`cargo install --locked --version <version> --root <scratch> cargo-dist`, never in the repository), then:
  `dist plan --tag=v1.0.0 --output-format=json > <scratch>/plan.json`. `jq -r .announcement_github_body <scratch>/plan.json`
  shows the `1.0.0` section of `CHANGELOG.md`, and `sh .github/scripts/release-guard.sh artifacts <scratch>` passes
  (the notes must not name the excluded engine; the guard fails the tag run otherwise). This publishes nothing.
- [ ] The `CHANGELOG.md` heading `## [1.0.0]` carries the real date (no `YYYY-MM-DD`), and `## [Unreleased]` above it
  is empty.

## Version and tag

- The workflow reads the version from the tag, which must match `v[0-9]+.[0-9]+.[0-9]+*`.
- `release.yml` publishes on a `v*` tag run: a tag push, or a `workflow_dispatch` whose ref is a `refs/tags/v*` tag
  (this is what "Tag release" starts, because a tag created with `GITHUB_TOKEN` does not trigger the push event). A
  dispatch on any branch stays a dry run and never publishes. In both cases the `mode` step takes the tag from
  `GITHUB_REF`, and `plan` passes it to `dist host --tag`.
- The `plan` job runs `dist host --steps=create --tag=<tag>`. It fails when the version of the tag differs from the
  workspace version (`version` under `[workspace.package]` in `Cargo.toml`), so nothing builds or publishes
  (ADR-0064). Bump the workspace version by PR before tagging.
- A `-` suffix (for example `v1.0.0-rc.1`) makes a pre-release. The update feed never serves a pre-release.

## Tag-time steps (owner)

Only the owner does these. The team never runs the "Tag release" workflow.

1. The release readiness changes are merged, with the dated `CHANGELOG.md` heading, and the pre-flight is ticked.
2. The Release dry run gate is strict. "Tag release" refuses to tag unless both of these exist for the exact commit
   being tagged (the head of `main` when `sha` is empty):
   - a successful `Release` run started by `workflow_dispatch` on `main` for that commit, and
   - a successful `CI` run for that commit.

   When `main` has moved since the last dry run (any merge, even docs), start a new one: Actions -> Release -> Run
   workflow, branch `main`, no inputs, then wait for it to turn green before going on. Check it with
   `gh run list --workflow release.yml --event workflow_dispatch --branch main --limit 1 --json status,conclusion,headSha`
   and `gh run list --workflow ci.yml --branch main --event push --commit <sha> --limit 1 --json status,conclusion,headSha`; the `headSha` must be
   the commit to tag.
3. Actions -> "Tag release" -> Run workflow. Branch `main` (the workflow refuses any other ref). Inputs:
   - `version`: `1.0.0` (no `v` prefix, no `+build`; a pre-release suffix such as `1.0.0-rc.1` is accepted).
   - `sha`: leave empty (it then means the current head of `main`). To tag an earlier `main` commit, give 7 to 40
     lowercase hex characters, and that commit needs its own green dry run and CI run.
   - `confirm`: `release v1.0.0`, exactly.
4. The workflow validates, creates the lightweight tag `v1.0.0` with `gh api`, dispatches `release.yml` on the tag and
   writes the tag, the commit and the Release run link to the job summary. Open the link, or find the run with
   `gh run list --workflow release.yml --event workflow_dispatch --branch v1.0.0 --limit 1`.

### What to verify afterwards

- The "Tag release" job is green and its summary names `v1.0.0`, the expected commit and a Release run link.
- The Release run on the ref `v1.0.0` ran `plan`, the builds, `sign (macOS)`, `update manifest` (not the dry-run
  manifest job), `global artifacts` and `host`, and `host` is green. If `host` was skipped, nothing was
  published (a dry run); see Rollback.
- The assets, `sqler-update.json`, the checksums and the notes: run every item of "Post-release verification" below.

### Failure modes

- Validation fails: the job prints one line `tag-release-validate: <reason>` and stops before anything is written.
  Typical reasons: the ref is not `main`; the version is not semver; `confirm` differs from `release vX.Y.Z`; the
  `sha` is unknown or not on `main`; the `Cargo.toml` workspace version differs from `version`; `CHANGELOG.md` has no
  `## [version]` heading or the section names the excluded engine; the tag already exists; the `release.yml` at the
  commit has no `# tag-dispatch:` marker line (the commit is older than this feature); or the dry run or CI run for
  that commit is missing or not successful. Fix the cause (new dry run, a PR to `main`, a corrected input) and run
  "Tag release" again. Nothing was created, so there is nothing to clean up.
- Environment policy risk: the environment `release` allows `v*` tags only. A dispatch on `refs/tags/v1.0.0` should
  match that rule like a push does, but this was never proven before the first release. If it does not, `sign (macOS)`
  fails with "not allowed to deploy", `host` never starts and nothing is published. The tag exists, so follow
  Rollback.
- `GITHUB_TOKEN` ref creation risk: a tag ruleset, or the refusal of workflow-file changes without the `workflows`
  permission, could reject the call that creates the tag. If so the job fails at "Create tag" with nothing created; use the
  fallback below.
- The tag was created but the dispatch failed or no run appeared: the summary says so. The owner starts the run with
  `gh workflow run release.yml --ref v1.0.0`.

### Fallback (owner only)

If "Tag release" cannot be used, the owner tags locally. The team never does this.

1. `git fetch origin main`.
2. Pick the `main` commit the dry run passed on.
3. `git tag v1.0.0 <sha>` (a lightweight tag).
4. `git push origin v1.0.0` (a push by a person triggers `release.yml` through the tag push).
5. Watch the run with bounded waits: `gh run list --workflow release.yml --event push --limit 1`, then
   `gh run view <id>`.

## What the workflow does

Jobs in order:

1. `plan`: `dist host --steps=create`, third-party licences, the build matrix.
2. `guard (features)`: rejects a feature line or dependency tree that could ship the excluded engine.
3. `build (<target>)`, once for each of the 5 targets: the archive `sqler-app-<triple>.tar.xz` (`.zip` on Windows)
   with a `.sha256` file each, the MSI on Windows (installed and checked on the runner), a `.dmg` on each macOS
   target (ad-hoc signed, not notarised) and a `.deb` on each Linux target (installed and checked on the runner), each
   with a `.sha256` file, the guards against the excluded engine, the `--version` smoke check and the size line.
4. `sign (macOS)` (environment `release`): signs and notarises both macOS archives; fails if a secret is missing.
5. In parallel after that:
   - `global artifacts`: `sha256.sum` (which also lists the `.dmg`, `.deb` and `.msi` files; the step fails when one is
     missing), the shell and PowerShell installers, the source archive and one
     `sqler-app_<triple>.cdx.xml` SBOM per target.
   - `update manifest` (environment `release`): signs `sqler-update.json` with `SQLER_UPDATE_SIGNING_KEY` and checks
     it with `xtask release verify`.
6. `host`: `gh release create` with every file and the notes of the `1.0.0` section of `CHANGELOG.md`, then checks
   that `sqler-update.json` is a release asset.

Confirm the exact asset names against the artifacts of the dry run. Do not guess them.

## Post-release verification

Work in a scratch directory and delete it afterwards.

- [ ] `gh release view v1.0.0 --json assets --jq '.assets[].name'` includes `sqler-update.json`, and
  `gh release view v1.0.0 --json assets --jq '.assets[].name' | grep -ic duckdb` prints `0`.
- [ ] The installers are published: `gh release view v1.0.0 --json assets --jq '.assets[].name'` lists
  `sqler-app-aarch64-apple-darwin.dmg`, `sqler-app-x86_64-apple-darwin.dmg`,
  `sqler-app-x86_64-unknown-linux-gnu.deb`, `sqler-app-aarch64-unknown-linux-gnu.deb` and
  `sqler-app-x86_64-pc-windows-msvc.msi`. After downloading the release files into the scratch directory,
  `sha256sum -c --ignore-missing sha256.sum` passes for every line. The `.dmg` is not notarised by design
  ([ADR-0075](docs/src/adr/0075-platform-installers.md)).
- [ ] The feed verifies: `curl -fsSLo <scratch>/sqler-update.json https://github.com/alwatroos/sqler/releases/latest/download/sqler-update.json`,
  then `cargo run -p xtask -- release verify <scratch>/sqler-update.json` prints `ok key_id=9735ae5e612eaa24`, the
  version `1.0.0` and one line for each of the 5 platforms, none naming the excluded engine.
- [ ] `gh release view v1.0.0 --json body --jq .body | grep -ic duckdb` prints `0`.
- [ ] A downloaded archive passes `shasum -a 256 -c <archive>.sha256`, and `sqler --version` prints `sqler 1.0.0`.
- [ ] On the downloaded macOS archive, extracted: `codesign --verify --strict --verbose=2 sqler` passes,
  `codesign -dv --verbose=2 sqler 2>&1 | grep -c 'flags=0x10000(runtime)'` prints `1`,
  `spctl --assess --type open --context context:primary-signature -v sqler` reports `source=Notarized Developer ID`,
  and `shasum -a 256 -c <archive>.sha256` passes. Do not paste the Authority or TeamIdentifier lines anywhere public.
- [ ] On a desktop, the New connection driver list and the Plugins view show SQL Server. Headless proxy:
  `LC_ALL=C grep -a -c 'driver.host:mssql' sqler` prints more than 0.
- [ ] `gh run list --workflow benchmarks.yml --limit 1` shows a run for the tag's commit (it starts by itself after the
  Release run, ADR-0074). Informational: a red run is a follow-up and never a reason to roll the release back. Record
  the figures in `docs/src/benchmarks.md`.

## Rollback

- The tag run failed before `host`: nothing is published. The owner deletes the tag
  (`gh api --method DELETE repos/{owner}/{repo}/git/refs/tags/v1.0.0`, or `git push origin :refs/tags/v1.0.0` and the
  local tag after the fallback route), fixes `main` by PR, repeats the dry run on the new head, then runs "Tag
  release" again.
- "Tag release" created the tag but the dispatch did not start: the owner runs
  `gh workflow run release.yml --ref v1.0.0`.
- A bad release is already published: do not reuse the tag. Mark it a pre-release in the GitHub UI, or delete it with
  `gh release delete v1.0.0 --cleanup-tag --yes`, so `latest` stops serving it. Then fix forward with `v1.0.1`
  (the workspace version bump PR comes first).
- Never rotate the update key as a rollback. See [update-feed.md](docs/src/releasing/update-feed.md).

## Owner step

The owner announces the release (decision 10).

Related pages: [signing](docs/src/releasing/signing.md), [update feed](docs/src/releasing/update-feed.md),
[install](docs/src/user-guide/install.md), [ADR-0064](docs/src/adr/0064-release-pipeline.md),
[ADR-0065](docs/src/adr/0065-macos-signing-and-notarisation.md),
[ADR-0075](docs/src/adr/0075-platform-installers.md),
[ADR-0066](docs/src/adr/0066-update-check-and-signed-manifest.md).
