# ADR-0075: Platform installers

- Status: accepted
- Date: 2026-10-26
- Deciders: SQLer maintainers
- Amends: [ADR-0064](0064-release-pipeline.md), sections "Targets, artifacts, checksums" and "Dropped, and why"

## Context and Problem Statement

[ADR-0064](0064-release-pipeline.md) ships an archive per target, a shell and a PowerShell installer, and a Windows MSI.
macOS has no installer in the usual sense (only a tar archive with a bare binary), Linux has no package, and the MSI
puts `sqler.exe` in `Program Files` but adds no Start menu entry. Intake item 4 asks for a per-platform installer, with
the `sqler` command on `PATH` where that makes sense. Unsigned installers are acceptable. This ADR reverses two lines
of ADR-0064: "`.deb` ... dropped (owner decision 7)" and "A macOS `.app` bundle" (not built).

## Decision Drivers

- One installer per OS in the form users expect: drag and drop on macOS, a native package on Linux, an MSI on Windows.
- No new tool, crate or pinned download in the release pipeline.
- The binary inside every installer is the one that already passed the DuckDB guard and the `--version` smoke check.
- Every new file gets a checksum like the existing artifacts.

## Considered Options and Decision Outcome

| OS | Installer | Built by |
| -- | --------- | -------- |
| macOS | `sqler-app-<triple>.dmg` with `SQLer.app` and an `Applications` link (drag and drop) | `packaging/macos/build-dmg.sh`, in `build (<target>)` |
| Windows | the cargo-dist MSI, now with a Start menu shortcut | cargo-dist (`crates/sqler-app/wix/main.wxs`) |
| Linux | `sqler-app-<triple>.deb` for x86_64 (`amd64`) and aarch64 (`arm64`) | `packaging/linux/build-deb.sh`, in `build (<target>)` |

The scripts are POSIX sh and use only what the runner image has: `hdiutil`, `codesign`, `plutil`, `dpkg-deb` and
`dpkg-shlibdeps` (package `dpkg-dev`, installed through apt). They start from the directory that holds the guarded
`sqler` binary and the licence bundle (R14), run both guard modes on the staged tree, and are each followed by a check
script that installs or mounts the result and runs `sqler --version`. The check scripts for the `.deb` and the MSI
install on the host, so they run on CI runners only.

- **macOS.** `SQLer.app` carries `Info.plist`, `Contents/MacOS/sqler` and the licence bundle in `Resources`. The
  `.dmg` has no `/usr/local/bin` step; the install page shows an optional `ln -s` for the command line.
- **Windows.** The MSI is still cargo-dist's, with the same `UpgradeCode`, the install directory
  `Program Files\sqler-app` and the optional `PATH` feature. The new component adds a machine-wide Start menu shortcut,
  keyed by a registry value under `HKCU`. `check-msi.ps1` installs it silently and asserts the executable, the
  shortcut and the machine `PATH`, then uninstalls it.
- **Linux.** The `.deb` installs `/usr/bin/sqler`, a desktop entry and the licences under `/usr/share/doc/sqler`.
  `Depends` comes from `dpkg-shlibdeps` and `Recommends` lists the libraries the binary loads at run time. The
  version has its first `-` turned into `~`, so a pre-release sorts before the release.
- **Still dropped:** AppImage, rpm and Flathub (owner decision 7, unchanged). `.deb` is the only Linux package.

### The `.dmg` is ad-hoc signed on every run

The bundle is signed with `codesign --force --deep --sign -` (no identity, no hardened runtime, no entitlements), which
keeps an arm64 build runnable and avoids the "damaged" message. It is **not notarised**, on dry runs and on tag runs.
The installer is built inside `build (<target>)` and not in `sign (macOS)`: that job is skipped on dry runs, so the
`.dmg` could never be checked before a release, and a bundle made there from the already notarised binary would still
need its own signature and notarisation. Signing and notarising the `.dmg` is a follow-up. The tar archives stay signed
and notarised as in [ADR-0065](0065-macos-signing-and-notarisation.md). The install page tells users how to open the
`.dmg` (Open Anyway, or `xattr -dr com.apple.quarantine`).

### Checksums and the update manifest

Each new file has a `<file>.sha256` sidecar in the `shasum`/`sha256sum` format. Both go into the build upload. In
`global artifacts` a step checks that all four `.dmg`/`.deb` names and the `.msi` exist, runs `sha256sum -c` on each
sidecar and appends their lines to `sha256.sum`; `host` publishes them through its existing `artifacts/*` glob. The
signed update manifest ([ADR-0066](0066-update-check-and-signed-manifest.md)) reads only the `executable-zip` entries of
the plan, so the installers do not change it. A test in `xtask` pins that.

### Rejected

- The cargo-dist `pkg` installer: it makes a `.pkg`, not a `.dmg`.
- `cargo-deb`: another pinned tool that builds or relocates the binary, where `dpkg-deb` is already on the runner.
- Building the `.dmg` in `sign (macOS)`: see above.

## Consequences

- A tag run publishes two `.dmg`, two `.deb` and the `.msi` next to the archives. The `.dmg` is not notarised; the
  Windows MSI and `.exe` stay unsigned (ADR-0064 owner decision 2).
- Release time grows by the installer steps (about a minute per target) and by the `.deb` check on both Linux runners.
- `dpkg-dev` is a new, unpinned system package in the Linux build jobs (see `docs/src/dependencies.md`).
- The `.deb` `Depends` line pins a glibc version at least that of the Ubuntu 22.04 runners.
- The `.deb` and MSI checks change the CI host; they must not be run on a developer machine.
- Assumed and recorded in the task file: bundle id `io.github.alwatroos.sqler`, no icon key (no artwork exists).

## Amendment 1 (2026-10-30, A): the macOS `.dmg` is Developer ID signed on tag runs

- Dry runs (and the `build` job on every run) keep building, checking and uploading the ad-hoc signed `.dmg`
  (`build-dmg.sh`, `check-dmg.sh`) without secrets.
- On `v*` tag runs `sign (macOS)` rebuilds the `.dmg` from the signed binary, signs, notarises and staples the app and the
  `.dmg` ([ADR-0065](0065-macos-signing-and-notarisation.md) amendment 2) and replaces the ad-hoc `.dmg` and its
  `.sha256` in the artifact before `global` runs, so `sha256.sum` and the sidecars hash the final files. The update
  manifest still covers archives only. `build-dmg.sh` gains `--app` and `--pack` modes for this; its default mode is
  unchanged.
