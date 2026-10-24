# Releasing SQLer

This is the checklist for cutting a release such as `v1.0.0`. The pipeline is `.github/workflows/release.yml`
(cargo-dist, configured in `dist-workspace.toml`); the design is in
[ADR-0064](docs/src/adr/0064-release-pipeline.md).

## Scope

- Releases are published on GitHub Releases only. There is no Homebrew, winget, Scoop or crates.io package (owner
  decision 4).
- The team tags and publishes, after the release readiness items are merged. The owner announces.
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
  `gh run list --workflow release.yml --event workflow_dispatch --limit 1` and `gh run view <id>`.
  - [ ] The `update manifest (dry run)` job is green.
  - [ ] The `benchmarks.yml` run that the dry run triggers (`workflow_run`, after a successful dispatch run on `main`)
    is green: `gh run list --workflow benchmarks.yml --limit 1`.
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
- [ ] macOS is unsigned by owner decision, so no `APPLE_*` secret or `SQLER_ALLOW_UNSIGNED_FALLBACK` variable is set:
  `gh secret list --env release` and `gh secret list` show no `APPLE_*` name, and `gh variable list` shows no
  `SQLER_ALLOW_UNSIGNED_FALLBACK`. What the job does:
  - With no `APPLE_CERTIFICATE` and `APPLE_CERTIFICATE_PASSWORD`, `sign (macOS)` only prints the notices
    "Unsigned macOS build" and "Not notarised", uploads nothing and succeeds. The team sets nothing.
  - `SQLER_ALLOW_UNSIGNED_FALLBACK` matters only when the secrets exist and signing fails (the job fails closed). It
    must stay unset.
  - A partial set (certificate and password present) makes the job attempt signing, so do not add part of the set.
  - Details: [signing.md](docs/src/releasing/signing.md).
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
- The `plan` job runs `dist host --steps=create --tag=<tag>`. It fails when the version of the tag differs from the
  workspace version (`version` under `[workspace.package]` in `Cargo.toml`), so nothing builds or publishes
  (ADR-0064). Bump the workspace version by PR before tagging.
- A `-` suffix (for example `v1.0.0-rc.1`) makes a pre-release. The update feed never serves a pre-release.

## Tag-time steps (team)

1. The release readiness changes are merged, with the dated `CHANGELOG.md` heading.
2. `git fetch origin main`.
3. Pick the `main` commit the dry run passed on.
4. `git tag v1.0.0 <sha>` (a lightweight tag).
5. `git push origin v1.0.0`.
6. Watch the run with bounded waits: `gh run list --workflow release.yml --event push --limit 1`, then
   `gh run view <id>`.

## What the workflow does

Jobs in order:

1. `plan`: `dist host --steps=create`, third-party licences, the build matrix.
2. `guard (features)`: rejects a feature line or dependency tree that could ship the excluded engine.
3. `build (<target>)`, once for each of the 5 targets: the archive `sqler-app-<triple>.tar.xz` (`.zip` on Windows)
   with a `.sha256` file each, the MSI on Windows, the guards against the excluded engine, the `--version` smoke
   check and the size line.
4. `sign (macOS)` (environment `release`): notices only while no `APPLE_*` secret exists.
5. In parallel after that:
   - `global artifacts`: `sha256.sum`, the shell and PowerShell installers, the source archive and one
     `sqler-app_<triple>.cdx.xml` SBOM per target.
   - `update manifest` (environment `release`): signs `sqler-update.json` with `SQLER_UPDATE_SIGNING_KEY` and checks
     it with `xtask release verify`.
6. `host`: `gh release create` with every file and the notes of the `1.0.0` section of `CHANGELOG.md`, then checks
   that `sqler-update.json` is a release asset.
7. `announce`: does nothing.

Confirm the exact asset names against the artifacts of the dry run. Do not guess them.

## Post-release verification

Work in a scratch directory and delete it afterwards.

- [ ] `gh release view v1.0.0 --json assets --jq '.assets[].name'` includes `sqler-update.json`, and
  `gh release view v1.0.0 --json assets --jq '.assets[].name' | grep -ic duckdb` prints `0`.
- [ ] The feed verifies: `curl -fsSLo <scratch>/sqler-update.json https://github.com/alwatroos/sqler/releases/latest/download/sqler-update.json`,
  then `cargo run -p xtask -- release verify <scratch>/sqler-update.json` prints `ok key_id=9735ae5e612eaa24`, the
  version `1.0.0` and one line for each of the 5 platforms, none naming the excluded engine.
- [ ] `gh release view v1.0.0 --json body --jq .body | grep -ic duckdb` prints `0`.
- [ ] A downloaded archive passes `shasum -a 256 -c <archive>.sha256`, and `sqler --version` prints `sqler 1.0.0`.
- [ ] On a desktop, the New connection driver list and the Plugins view show SQL Server. Headless proxy:
  `LC_ALL=C grep -a -c 'driver.host:mssql' sqler` prints more than 0.

## Rollback

- The tag run failed before `host`: nothing is published. Delete the remote tag (`git push origin :refs/tags/v1.0.0`)
  and the local tag, fix on `main` by PR, repeat the dry run, tag again.
- A bad release is already published: do not reuse the tag. Mark it a pre-release in the GitHub UI, or delete it with
  `gh release delete v1.0.0 --cleanup-tag --yes`, so `latest` stops serving it. Then fix forward with `v1.0.1`
  (the workspace version bump PR comes first).
- Never rotate the update key as a rollback. See [update-feed.md](docs/src/releasing/update-feed.md).

## Owner step

The owner announces the release (decision 10).

Related pages: [signing](docs/src/releasing/signing.md), [update feed](docs/src/releasing/update-feed.md),
[install](docs/src/user-guide/install.md), [ADR-0064](docs/src/adr/0064-release-pipeline.md),
[ADR-0065](docs/src/adr/0065-macos-signing-and-notarisation.md),
[ADR-0066](docs/src/adr/0066-update-check-and-signed-manifest.md).
