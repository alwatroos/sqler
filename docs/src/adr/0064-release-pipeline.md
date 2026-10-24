# ADR-0064: Release pipeline

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

SQLer 1.0 needs downloads for macOS, Linux and Windows that anyone can verify, built by CI and not by hand. The
release must never contain the DuckDB engine ([ADR-0070](0070-duckdb-engine-and-licence.md)), must stay within the
binary budget ([ADR-0069](0069-binary-size-budget-70-mb.md)) and must follow the CI rules of
[ADR-0068](0068-ci-pipeline-speed.md): path gating, stable job names, no cache saved from PRs. The build runs
unsigned: the owner decided that signing and notarisation come later (R2), and the update manifest (R3, R5) is a
separate work item.

## Decision Drivers

- One tool that produces archives, installers and checksums for five targets from one config file.
- Publishing only on a version tag; every other run is a dry run.
- A mechanical guarantee that no download contains DuckDB, whatever the release feature set turns out to be.
- A PR that touches the release files gets feedback, but not a three-OS build on every push.

## Considered Options and Decision Outcome

Adopted: **cargo-dist 0.32.0** (crate `cargo-dist`, binary `dist`; latest stable on crates.io on 2026-10-23, released
2026-05-22, `MIT OR Apache-2.0`). `dist-workspace.toml` is the single source. `.github/workflows/release.yml` started
from `dist generate` and is maintained by hand (`allow-dirty = ["ci"]`), because the path-gated PR trigger, the
dry-run mode, the GPUI system packages, the DuckDB guard after the build, the `--version` smoke run, the size line and
the secret-gated notice steps cannot be generated. After a dist upgrade, run `dist generate` into a scratch copy and
diff it against `release.yml`.

### Targets, artifacts, checksums

| Target | Runner | Artifacts |
| ------ | ------ | --------- |
| `aarch64-apple-darwin` | `macos-14` | `sqler-app-aarch64-apple-darwin.tar.xz` |
| `x86_64-apple-darwin` | `macos-15-intel` (native Intel, so the Metal shaders are not cross-compiled) | `sqler-app-x86_64-apple-darwin.tar.xz` |
| `x86_64-pc-windows-msvc` | `windows-2022` | `sqler-app-x86_64-pc-windows-msvc.zip` and `.msi` |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | `sqler-app-x86_64-unknown-linux-gnu.tar.xz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` | `sqler-app-aarch64-unknown-linux-gnu.tar.xz` |

Global artifacts: `sqler-app-installer.sh` (macOS and Linux), `sqler-app-installer.ps1` (Windows),
`sha256.sum` (one file over all artifacts), one `<artifact>.sha256` per artifact, `source.tar.gz` and
`sqler-app_<triple>.cdx.xml` (one per target; Amendment 2). Downloads live under `https://github.com/alwatroos/sqler/releases/latest/download/`.
Artifact names keep the package name `sqler-app`; the binary inside is `sqler`.

The installers put the binary in `~/.sqler/bin` (`install-path`), not in the cargo bin directory: a self-contained
directory that is easy to remove, and SQLer is not a cargo-installed tool. `install-updater` is off: the app's own
update flow (R3, R5) replaces the dist updater. The MSI's WiX v3 definition is `crates/sqler-app/wix/main.wxs`
(dist hard-codes the path next to the package manifest), committed as generated.

### Where the config lives

Package-level metadata in `crates/sqler-app/Cargo.toml` (`[package.metadata.dist] dist = true`, the generated
`[package.metadata.wix]` GUIDs and a `repository` line), because dist skips `publish = false` packages unless they opt
in, and a workspace-wide `dist = true` would also ship `sqler-cli`, `xtask` and the `sqler-acp` mock-agent binary.
`precise-builds = true` makes dist run `cargo build --package sqler-app` instead of `--workspace`, which would compile
the bundled DuckDB C++ engine. `[profile.dist]` inherits `release` and nothing else; it adds no LTO because thin LTO
grew the binary (see the comment on `[profile.release]` in `Cargo.toml`).

### The release feature set

The features of the release build are **one line**, `features = [...]` in `dist-workspace.toml`. Its value is
`features = ["plugins", "driver-mssql"]` (owner decision 12: the plugin host and SQL Server ship in 1.0). cargo-dist
adds these features on top of the `default` features of `sqler-app`, so `default` stays as it is. To change the set,
edit that line only. The same line drives the SBOM (`cargo cyclonedx --features`, through the `sed` in `release.yml`).
The guard also reads `features` and `all-features` under `[package.metadata.dist]`
in `crates/sqler-app/Cargo.toml` (cargo-dist accepts them there) and rejects `driver-duckdb` or `all-features = true`
in either place. The DuckDB guard
below does not depend on the value of the line. `driver-duckdb` never belongs in it.

### Triggers and the publish guard

- `pull_request` only when `dist-workspace.toml`, `release.yml`, `release-guard.sh`, `crates/sqler-app/Cargo.toml` or
  `crates/sqler-app/wix/**` changes. It runs `plan`, `guard (features)` and the Linux x86_64 build (about 12 minutes,
  uncached; no `rust-cache`, nothing runs on `main`, so a cache would never be saved). The label `full-ci` (read when
  the run starts) adds every target, `global artifacts` and the SBOM.
- `workflow_dispatch` (no inputs) is always a full dry run. It works once the file is on the default branch.
- `push` of a tag `v[0-9]+.[0-9]+.[0-9]+*` is the only way to publish. `plan` computes
  `publishing = push && ref starts with refs/tags/v`; `host` (the Release) and `announce` need it explicitly, on top of
  dist's own conditions, and alone hold `contents: write`. Everything else has `contents: read`.
- Until the package version is `1.0.0`, a tag run fails dist's tag and version coherence check. That is expected;
  nothing is tagged before the release item.
- Job names are stable: `plan`, `guard (features)`, `build (<target>)`, `sign (macOS)`, `global artifacts`, `host`,
  `announce`.

### The DuckDB guard

`.github/scripts/release-guard.sh` (POSIX sh) runs at four points and prints one `release-guard: <reason>` line per
finding (exit 1) or `release-guard: <mode> ok (<evidence>)`:

| Mode | When | Fails when |
| ---- | ---- | ---------- |
| `features` | `guard (features)`, before any build | `all-features = true`; `features` is not exactly one line; the list holds `driver-duckdb`; or `cargo tree -p sqler-app --target all --locked -e normal,build --features "<list>"` holds `sqler-driver-duckdb`, `duckdb` or `libduckdb-sys` (this catches feature unification) |
| `build-dir` | after `dist build`, per target | `target/<triple>/dist/build/libduckdb-sys-*` exists |
| `binary` | on the `sqler` extracted from the archive | the binary contains a DuckDB engine marker |
| `artifacts` | before every upload (`build`, `global artifacts`, `host`) | a file name, or a text artifact (`*.sh *.ps1 *.json *.xml *.wxs *.sum *.sha256`), mentions DuckDB |

`SQLER_RELEASE_FEATURES` (a comma list) replaces the parsed line, for the negative test. The binary markers are strings
of the DuckDB C++ engine only (`Could not set lock on file`, `preserve_insertion_order`, `checkpoint_threshold`,
`enable_object_cache`; each found in the `libduckdb-sys` 1.20000.0 sources and absent from the repository's own
sources). A plain `duckdb` grep would be wrong: the default binary names DuckDB in UI text. The marker list is a variable
in the script.

### Smoke run, size, SBOM

Each build job extracts its archive and runs `sqler --version`; the output must match `^sqler [0-9]` (on Windows it is
redirected to a file because of the GUI subsystem). The step summary gets the binary and archive bytes next to the
83,886,080-byte budget, with a warning when over; it reports and never gates. cargo-dist produces **one** SBOM per
release (`sqler-app.cdx.xml`, `cargo-cyclonedx`), not one per artifact; per-artifact SBOMs are a follow-up (done in Amendment 2: one SBOM per target).

### Dropped, and why

- Homebrew, winget, Scoop, npm and crates.io publishing: owner decision 4.
- `.deb`, AppImage and Flathub: owner decision 7.
- `driver-duckdb` in any release artifact: owner decision 11 and ADR-0070.
- Signing and notarisation: owner decisions 1 and 2, delivered by R2. `release.yml` has a placeholder step
  `macOS notarisation (R2)` that prints a notice while no signing secret exists (the macOS and Windows downloads are
  unsigned, which the install page explains). R2 replaced it with the gated job `sign (macOS)`
  ([ADR-0065](0065-macos-signing-and-notarisation.md)).
- The updater and the signed update manifest: R3 and R5. `host` has an `Update manifest signing (R5)` placeholder.
- A macOS `.app` bundle: the 1.0 macOS download is the archive with the bare `sqler` binary.
- `github-attestations`: off; revisit with signing.

## Consequences

- A release is a tag push; everything else is a dry run whose artifacts expire after 7 days.
- `release.yml` drifts from `dist generate` over time; upgrading dist is a manual diff. `cargo-dist-version` must equal
  the installed `dist`.
- Changing the feature line changes the size of every binary; check it against the budget in the dry run.
- The first dry run of a target can fail for runner reasons (`macos-15-intel` availability, WiX v3 on `windows-2022`,
  GPUI on `ubuntu-22.04-arm`); a target that is not cheap to fix is dropped from `targets` and recorded here.

## Amendment 1 (2026-10-23, S2): plugin host and SQL Server in every artifact

- Built-in route: the app compiles `plugins/sqlserver/sqler.toml` in (only with `driver-mssql`) and adds it to plugin
  discovery when no `sqlserver` plugin is found on disk (`plugins/dirs.rs`). Nothing is written to `<config>`, and every
  installer works unchanged.
- Lost alternative: shipping the manifest in the archives. It would reach archive users only, because the installers
  install binaries only, and it would still need a manual copy.
- Consent: the built-in plugin goes through the normal grant flow, so the user approves `driver.host:mssql` once in the
  Plugins view (ADR-0060). A manifest on disk (`<config>/plugins` or `--plugin-dir`) replaces the built-in one.
- Runtime needs: wasmtime, Cranelift and aws-lc are linked statically, so no system library is added. `ring` is
  installed as the TLS provider at the start of `main` (ADR-0071). The installers install only the binary; none of them
  creates `<config>`, and uninstalling leaves `<config>` (connections, grants, preferences) alone.
- For R2 (signing): a hardened-runtime signature probably needs a JIT entitlement before a wasm component can compile
  (assumed, not tested; the manifest-only SQL Server plugin does not compile wasm).

Release feature set, S2 dry run (filled in from the `full-ci` run of the S2 pull request):

| OS (target) | Binary bytes | Archive bytes | Budget | Headroom |
| ----------- | ------------ | ------------- | ------ | -------- |
| macOS arm64 | 51,921,008 | 13,725,756 | 83,886,080 | 31,965,072 |
| macOS x86_64 | 58,434,600 | 15,920,380 | 83,886,080 | 25,451,480 |
| Linux x86_64 | 81,176,680 | 20,724,964 | 83,886,080 | 2,709,400 |
| Linux aarch64 | 68,182,120 | 17,832,468 | 83,886,080 | 15,703,960 |
| Windows x86_64 | 71,068,672 | 25,901,525 | 83,886,080 | 12,817,408 |

All five rows come from the same `full-ci` run, on the merge of `main` into the S2 branch. Every binary is under the
budget; Linux x86_64 is the tightest, with 2,709,400 bytes (3.2 percent) of headroom. The `ldd`-style linkage of the
Linux binaries (`dist build --print=linkage`) lists only `libc`, `libm`, `libgcc_s`, `libxcb`, `libxcb-xkb`,
`libxkbcommon`, `libxkbcommon-x11` and their dependencies `libXau`, `libXdmcp`, `libbsd` and `libmd`; the macOS binaries
link system libraries and frameworks only; the Windows binary links no non-system library.

## Measurements

Filled in from the `full-ci` dry run of the release-pipeline PR.

| OS (target) | Binary bytes | Archive bytes | `size.yml` binary | Budget |
| ----------- | ------------ | ------------- | ----------------- | ------ |
| macOS arm64 | 38,557,920 | 10,383,528 | 38,507,056 | 83,886,080 |
| macOS x86_64 | 41,923,104 | 11,810,636 | n/a | 83,886,080 |
| Linux x86_64 | 63,279,336 | 16,640,340 | 63,330,024 | 83,886,080 |
| Linux aarch64 | 54,601,192 | 14,597,448 | n/a | 83,886,080 |
| Windows x86_64 | 51,094,528 | 19,128,405 | 50,760,704 | 83,886,080 |

Engine markers, confirmed against a real `cargo build --release -p sqler-app --features driver-duckdb` binary (96,116,480
bytes): `Could not set lock on file`, `preserve_insertion_order`, `checkpoint_threshold` and `enable_object_cache` are all
present, so `release-guard.sh binary` exits 1 on it; on the default release binary (38,557,248 bytes, macOS arm64) all
four are absent and it exits 0. `release-guard.sh build-dir` exits 1 on the DuckDB build (two `libduckdb-sys-*` build
directories). `SQLER_RELEASE_FEATURES=driver-duckdb release-guard.sh features` exits 1.

All five targets stay under the budget (largest: Linux x86_64 at 63,279,336 bytes). The Windows archive is the `.zip`
(the `.msi` is a separate file). The `full-ci` dry run of the pull request (plan, `guard (features)`, five builds,
`global artifacts`) was green; `host` and `announce` were skipped (`PUBLISHING: false`), so nothing was published. Every
build job printed `sqler 0.1.0` from the smoke run and `release-guard: build-dir ok`, `binary ok (4 markers absent ...)`
and `artifacts ok`; the global job printed `release-guard: artifacts ok (25 files, 18 text artifacts checked)`. The
macOS x86_64 build ran on the native Intel runner and took about 30 minutes, the longest job by a wide margin.

## Signing secrets

When signing lands (R2 for Apple, R5 for the update manifest), the secrets must live in a GitHub environment whose
deployment rules limit it to `v*` tag runs. Repository-wide secrets would be readable by any workflow run on a branch.
The Apple secrets follow this rule in the job `sign (macOS)` ([ADR-0065](0065-macos-signing-and-notarisation.md)).

## Rollback

Delete `.github/workflows/release.yml`, `.github/scripts/release-guard.sh`, `dist-workspace.toml`,
`crates/sqler-app/wix/` and the dist metadata in `crates/sqler-app/Cargo.toml`, and remove `[profile.dist]` from
`Cargo.toml`. Nothing else depends on them.

## Amendment 2 (2026-10-24, R14): licence bundle and SBOM per target

- Licence bundle: `.github/scripts/third-party-licenses.sh` generates `THIRD-PARTY-LICENSES.txt` with cargo-about
  (`about.toml`, `about.hbs`) for the `features` and `targets` lines of `dist-workspace.toml`, appends the upstream
  `NOTICE` files and the verbatim licence files of the crates with bundled C code, and fails on `driver-duckdb` or on
  any DuckDB text. The `plan` job runs it once, before `dist plan`, and uploads the file; `build` and `global` download
  it, so every target ships the same bytes. `include = ["NOTICE", "THIRD-PARTY-LICENSES.txt", "licenses"]` puts it, with
  `LICENSE`, into every archive. Lost alternative: committing the file with a freshness check (every `Cargo.lock` bump
  would need a regenerated file).
- MSI: `crates/sqler-app/wix/main.wxs` is hand-edited to install `LICENSE`, `NOTICE`, `THIRD-PARTY-LICENSES.txt` and
  `licenses/LUCIDE-ISC.txt` next to `bin`. `allow-dirty` is now `["ci", "msi"]`, so cargo-dist does not refuse the
  edited file. After a cargo-dist upgrade, diff the wxs against a scratch `dist generate` as for `release.yml`.
- The shell and PowerShell installers install the binary only; they download the archive, which carries the bundle.
- SBOM: one CycloneDX file per target, `sqler-app_<triple>.cdx.xml` (`cargo cyclonedx --target <triple>
  --target-in-filename`), built in `global` from the `targets` and `features` lines. The Windows `.zip` and `.msi` share
  the `x86_64-pc-windows-msvc` file. This replaces the single release-wide SBOM described above.
- Licences: no new allow-list entry, no `deny.toml` exception and no advisory ignore. The audit is in
  [Licences and SBOM](../licenses.md).

## Amendment 3 (2026-10-24, R18): owner-triggered Tag release

- Trigger rule: `release.yml` publishes on a `v*` tag run, that is a tag push or a `workflow_dispatch` whose ref is
  `refs/tags/v*`. `publishing` is `(event is push or workflow_dispatch) and ref starts with refs/tags/v`. A dispatch on
  a branch, or on a tag without the `v` prefix, stays a dry run. Everywhere ADR-0064, ADR-0065 and ADR-0066 say "tag
  push", read "tag run".
- Why a dispatch: a tag created with `GITHUB_TOKEN` does not trigger `on: push: tags` (GitHub blocks recursive
  triggers), but a `workflow_dispatch` started with `GITHUB_TOKEN` does start a run. The new workflow `tag-release.yml`
  therefore creates the tag with `gh api` and then runs `gh workflow run release.yml --ref <tag>`. Lost alternative: a
  `tag` input on `release.yml` that publishes from a branch dispatch. It would put a branch ref into the `release`
  environment, whose policy is `v*` tags, and widen who can publish.
- Owner only: the owner starts "Tag release" (Actions, Run workflow, branch `main`, inputs `version`, optional `sha`,
  `confirm` = `release vX.Y.Z`). The team never runs it. Anyone with write access technically can, as with pushing a
  tag before; this is a process rule.
- Validator: `.github/scripts/tag-release-validate.sh` fails closed with one reason line before anything is written. It
  checks the ref is `main`, the semver, the confirm string, the commit (an ancestor of the `main` head), the
  `Cargo.toml` workspace version and the `CHANGELOG.md` heading at that commit, no `duckdb` in that section, the tag
  being absent, a successful `Release` dispatch run on `main` for that exact commit and a successful `CI` run on it
  (strict dry-run gate). Its tests run in the CI `docs` job with a fake `gh`, without network.
- Marker: the validator requires a line starting with `# tag-dispatch: ` in `release.yml` at the commit. A commit from
  before this amendment lacks it, and a dispatch there would only be a dry run.
- Concurrency: the group is `release-<ref>` for pull requests and for any tag ref, otherwise the run id. A tag push and
  a tag dispatch on the same tag are serialised, and the second `gh release create` fails on the existing release.
- Explicit tag: `mode` derives the tag once from `GITHUB_REF`, checks it against a strict regex and the workspace
  version, and `plan` passes it to `dist host --tag`. dist's own tag and version check still runs.
- Environment policy: GitHub checks the deployment policy against the ref the run executes on. A dispatch on
  `refs/tags/v1.0.0` runs on that ref, so the `v*` tag rule should match as for a push. Not verified: a branch dry run
  skips the environment jobs. If it fails, `sign (macOS)` fails with "not allowed to deploy", `host` needs it, and
  nothing publishes.
- Also unverified until the owner's run: that `GITHUB_TOKEN` may create the tag ref (a tag ruleset or the
  `workflows` permission rule could refuse it; nothing is created then). The fallback is the owner's local tag and push
  described in `RELEASING.md`.
