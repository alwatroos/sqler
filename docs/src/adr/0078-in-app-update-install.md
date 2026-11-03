# ADR-0078: In-app update download and install

- Status: accepted
- Date: 2026-11-02
- Deciders: SQLer maintainers
- Amends: [ADR-0066](0066-update-check-and-signed-manifest.md) (the client is no longer check-only) and
  [ADR-0075](0075-platform-installers.md) (the manifest no longer covers archives only)

## Context and Problem Statement

[ADR-0066](0066-update-check-and-signed-manifest.md) lets SQLer find out, from one Ed25519-signed manifest, that a newer
release exists, and stops there: it never downloads a binary and never installs one. The user must open GitHub, fetch the
right file and replace SQLer by hand. We want the app to download the newer release itself, verify it against the signed
manifest and install it when the user chooses, on Linux, macOS and Windows. The signed manifest is still the only
integrity gate. The difficult cases are macOS, where SQLer runs as a `SQLer.app` bundle that must be replaced as a whole,
and installs that must not replace themselves (package managers, folders SQLer cannot write to, development builds).

## Decision Drivers

- The ADR-0066 rules stay: compiled-in keys, a signed manifest as the only gate, the request contents, the redirect
  policy, the replay limits, and checks that are off by default with no request while off.
- macOS must update itself. A bare binary inside a signed bundle may not be swapped; the whole `SQLer.app` is replaced.
- A failed swap must leave the old version runnable. The running app stays usable until the user asks to restart.
- No unsafe code (the workspace forbids it), no system `tar`, no new release job, no change to the signing scripts.
- Release binary under 80 MB (ADR-0069); every new crate verified on crates.io and recorded.

## Considered Options and Decision Outcome

`sqler-update` (L1, headless) gains a download part and an install part. `sqler-app` owns the schedule, runs the work off
the UI thread and starts a detached relauncher. The UI receives plain strings and never links `sqler-update`.

### Download

- It starts only while update checks are on, in the background, after a check that returned `Available` with a verified
  signature. It is skipped when the check was automatic and that version is dismissed. A bad signature never reaches the
  download step.
- The artifact is the `platforms` entry (an archive: `sqler-app-<triple>.zip` on Windows, `.tar.xz` elsewhere) or, on
  macOS, the `bundles` entry (`sqler-app-<triple>.dmg`). A missing entry, an unknown platform, an untrusted URL or a size
  above the cap (`MAX_ARTIFACT_BYTES`, 256 MiB) ends in a refusal with no request.
- **Trusted URL.** No userinfo, query or fragment; the last path segment is the expected file name for the kind; and the
  URL is either `https://github.com/alwatroos/sqler/releases/download/<tag>/<name>` or, only when the feed itself is a
  loopback address (tests), the feed origin. The signature already covers the URL; this rule is a second check.
- **Request.** One `GET` with the same headers as the check (ADR-0066 "Request"): no cookies, no identifiers, no extra
  data. The redirect policy is the check's. A `Content-Length` that differs from the signed `size` is refused before the
  body is read. The body streams into `<name>.part` while SHA-256 (`ring`) is computed; reading stops once more bytes than
  `size` have arrived; each chunk has a 60 s stall timeout and there is no whole-request timeout, so a slow link works but
  a stalled one fails. Size and hash are compared, then `.part` is renamed. Every error removes `.part`.
- **Where.** `<config dir>/updates/<version>/<name>`. An already present file that passes size and hash is reused with no
  request; other entries of that directory are removed. Turning checks off aborts a running download and deletes
  `updates/`.

### Install plan

`plan_install(exe)` decides from paths only, in this order: (a) an ancestor `target/debug` or `target/release` is a
development build (manual); (b) an executable at `<B>.app/Contents/MacOS/<file>` makes the target the bundle `B.app` and
the kind `MacBundle`, otherwise the target is the executable and the kind `Archive`; (c) a target under a package
manager directory (`/usr/bin`, `/usr/lib`, `/usr/libexec`, `/usr/share`, `/opt/homebrew`, `/usr/local/Cellar`,
`/nix/store`, `/snap`; on Windows `ProgramFiles`, `ProgramFiles(x86)`, `ProgramW6432`) is a package-manager install
(manual); (d) otherwise SQLer updates itself. A folder SQLer cannot create a directory in is a manual outcome too
("not writable"). The manual outcome reports the verified file's path and a short reason, and the file is downloaded
also in that case, so the user has something to open.

### Install

1. The stage directory `.sqler-update/` is created (mode 0700) beside the target: the executable's directory, or the
   parent of the `.app`. A leftover stage is removed first, and again at the next start.
2. The artifact is copied into the stage and **re-hashed** there, so a file changed in the user-writable `updates/`
   directory after verification is caught (`ChangedAfterVerification`; the file is then discarded).
3. **Archive.** Exactly one regular-file entry named `sqler` (`.tar.xz`, read with `lzma-rs` and `tar`) or `sqler.exe`
   (`.zip`) is extracted, with at most two normal path components and at most 512 MiB. Zero or two such entries, a link,
   a path escape or an oversized entry refuse the archive; nothing else is written.
4. **Bundle (macOS).** The verified `.dmg` is attached read-only with `hdiutil attach -nobrowse -readonly -noautoopen`
   on a private mount point, `SQLer.app` is copied out with `ditto` (which keeps the code signature, the stapled ticket
   and extended attributes), and the image is always detached. Then `Contents/Info.plist` and a regular
   `Contents/MacOS/sqler` (neither a symlink) are required, and `codesign --verify --deep --strict` must pass.
   All tools are called by absolute path under `/usr/bin` with a 120 s kill timeout.
5. **`--version` guard.** The new program is run with `--version`; its output must contain the signed version as a
   whitespace-separated token (`VersionMismatch` otherwise). It is killed after 10 s.
6. **Swap with rollback.** `rename(target, stage/previous)` then `rename(new, target)`, all on one volume. If the second
   rename fails, the first is undone and the result is `Swap` ("the current version is unchanged"). For a bundle the whole
   `SQLer.app` moves, never the binary inside it. The old copy is left in the stage until the next start.
7. **Relaunch.** `sqler-app` starts a detached helper (unix: `/bin/sh` waiting for the pid, then `open -n <App.app>` for a
   bundle or `exec <exe>`; Windows: a hidden `powershell.exe` with `Wait-Process` and `Start-Process`, pid and path in
   environment variables) and then quits through the normal quit path, which keeps the unsaved-edit prompt. If the helper
   cannot start, the app says to quit and reopen.

The user chooses the moment: the notice and Settings show "Install and restart" once the file is verified. Failed installs
keep the verified file so a retry works.

### Manifest: `bundles`

The schema stays 1. A new optional map `bundles` lists, per `*-apple-darwin` triple, the release `.dmg` (url, sha256,
size). The `.dmg` already exists (ADR-0075), is in `sha256.sum` and, on tag runs, is Developer ID signed, notarised and
stapled (ADR-0065 amendment 2). `xtask release manifest` hashes it and checks its `.sha256` sidecar; it is required unless
`--partial`. `.deb` and `.msi` never appear. Old clients ignore unknown fields, so 1.2.x keeps working. Releases up to
1.2.2 have no `bundles`, so only builds that include this ADR can update themselves; the first such release is installed
by hand.

### Gatekeeper

The app downloads the file itself, so no `com.apple.quarantine` attribute is set on it. The copy made with `ditto` out
of the mounted image carries none either. Without the attribute there is no app translocation and no first-launch
prompt. Tag-run bundles are notarised and stapled anyway. Ad-hoc signed dry-run bundles are never listed in a signed
manifest, so they are never offered. `codesign --verify --deep --strict` does not pin the team identity; the sha256 from the
signed manifest is the gate, the signature check only catches a damaged bundle.

### Dependencies

| Crate | Use | Notes |
| ----- | --- | ----- |
| `lzma-rs` 0.3.0 | decode the `.tar.xz` stream | pure Rust, `MIT`, no unsafe in the path we use, small |
| `tar` 0.4.46 | read the single entry | `MIT OR Apache-2.0`; `default-features = false` (no `xattr`) |
| `zip` 9.3.0 (9.0.5 when chosen; bumped 2026-11-03, T5) | read `sqler.exe` from the Windows archive | `MIT`; already in `Cargo.lock` through an existing dependency; only the decoder features we need |
| `tokio` features `fs`, `io-util`, `time`, `process` | streaming file write, stall timeout, tool timeouts | already linked; features only |

The binary growth is measured with one capped release build and written in the pull request (the budget is ADR-0069);
the table with versions, dates and licences is in [Dependencies](../dependencies.md).

### Rejected options

1. **A new zipped `.app` asset.** It needs changes to `sign-macos.sh` and `build-dmg.sh` and a third notary submission;
   the existing `.dmg` already carries the signed, stapled bundle.
2. **`renamex_np(RENAME_SWAP)`.** An atomic exchange, but it needs unsafe FFI, and the workspace forbids `unsafe_code`.
   Two same-volume renames with rollback are enough because the user is told to restart.
3. **The system `tar` for archives.** GNU tar needs `xz` and we would lose control of which entries are written.
   (`hdiutil` and `ditto` are used only on macOS, since no pure-Rust UDIF reader exists.)

## Consequences

- Positive: a user on any platform gets the next release without visiting GitHub; every step is bound to the signed
  manifest; a failed swap leaves the old version in place; macOS is covered with the existing, notarised `.dmg`.
- Negative: three new crates (and a larger binary); the macOS flow depends on `hdiutil`, `ditto` and `codesign`; the first
  release with this feature cannot be reached by self-update; a standard (non-admin) macOS user cannot write
  `/Applications` and gets the manual outcome; the Windows rename and relaunch run only in CI on `main`.
- Neutral: the stage directory is visible beside the target until the next start (only that exact name is removed);
  `RELEASING.md` still describes an archives-only manifest and needs a follow-up.
