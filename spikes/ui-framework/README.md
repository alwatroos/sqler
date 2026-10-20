# UI framework spike (ADR-0001)

Throwaway spike, not product code. It lives in its own Cargo workspace, outside the root one, so
`ci.yml`, `cargo deny` and the root `Cargo.lock` never see it. It answers one question for
ADR-0001: does GPUI + gpui-component deliver a 1M-row virtualised grid and a SQL editor with syntax
highlighting on Linux (X11 and Wayland), macOS and Windows?

`## Results` is the hand-off table: the ADR, `docs/src/benchmarks.md` and `docs/src/dependencies.md`
are written from it.

## How to run

```sh
cd spikes/ui-framework
cargo build --release --locked
./target/release/spike-gpui                          # interactive window (manual check)
./target/release/spike-gpui --exit-after-first-frame # cold start only
./target/release/spike-gpui --smoke                  # scripted checks, SPIKE report, exit code
RESULTS_DIR=$PWD/results sh scripts/measure.sh $PWD/target/release/spike-gpui   # Linux, macOS
./scripts/measure.ps1 target/release/spike-gpui.exe                              # Windows
```

`--smoke` scrolls the grid to row 500 000 and to row 999 999, counts the rows the table delegate
renders per frame, runs the component's SQL highlighter over a ~40-line sample query, waits 1 s idle,
prints the report below and exits (`0` pass, `1` a check failed, `2` watchdog after 120 s):

```
SPIKE backend=gpui
SPIKE display=x11|wayland|macos|windows
SPIKE renderer=<adapter / driver, "metal" on macOS, else "unknown">
SPIKE first_frame_ms=...   # main() start -> first frame presented (excludes exec and loader time)
SPIKE grid_rows=1000000
SPIKE grid_max_rows_rendered_per_frame=...   # pass: <= 500
SPIKE grid_rendered_last_row=...             # pass: true
SPIKE editor_highlight_kinds=...             # pass: >= 3
SPIKE rss_mb=...                             # memory-stats physical_mem at idle, 1M rows loaded
SPIKE result=pass | result=fail reason=<slug>
```

`SPIKE_TRACE=1` prints phase timestamps on stderr. CI runs the same steps in
`.github/workflows/spike.yml` (X11 under `xvfb-run`, Wayland under headless `weston`, macOS, Windows).

What the code is: `gpui-spike/src/data.rs` (column-stored `SyntheticRows`, cells formatted on
demand), `grid.rs` (gpui-component `DataTable` with a `TableDelegate`), `editor.rs` (gpui-component
`Editor` with `EditorState::language("sql")`), `smoke.rs` (report and watchdog), `main.rs`.
`editor_highlight_kinds` comes from `gpui_component::highlighter::SyntaxHighlighter::new("sql")`, the
type the editor's own tree-sitter adapter wraps (`highlighter/input_adapter.rs`), with the grammar
from gpui-component's `tree-sitter-sql` feature (`tree-sitter-sequel`). No fork, no external grammar
crate registered by the spike.

## Time box

| | Date |
| - | ---- |
| Start | 2026-10-20 (first work on the branch; replace with the first commit date if it differs) |
| End | 2026-10-27 (start + 7 days) |

## Distribution choice (AC4)

Rule from the spec, in order: crates.io only; use the crate the latest stable `gpui-component`
actually depends on; on a tie prefer the newer release with recent commits, then upstream `gpui`.

- The latest stable `gpui-component` is 0.8.0 (published 2026-10-19). Its dependency list names
  neither `gpui` nor `gpui-ce`. It names `gpui-pre` `=0.3.9` (renamed to `gpui` inside the crate with
  `package = "gpui-pre"`), plus `gpui-pre-macros`, `gpui-pre-sum-tree` and `gpui-base 0.8.0`.
- `gpui-pre` 0.3.9 is a snapshot of Zed's GPUI (`zed@93d7456`, repository `zed-industries/zed`,
  published 2026-10-12). Upstream `gpui` on crates.io is at 0.2.2 from 2025-10-22; `gpui-ce` 0.2.2 is
  from 2026-08-28 (community edition). Neither can be unified with `gpui-component` 0.8.0 without
  two GPUIs in the tree.
- Chosen: `gpui-pre =0.3.9` (the crate `gpui-component` pulls) plus `gpui-pre-platform =0.3.9`
  (Zed's `gpui_platform`, needed to create the application on every OS). The platform crate is not
  re-exported by `gpui-component`, so the spike lists it directly at the same pinned version.
- Check in the spike workspace (output under `## Versions`): `cargo tree -i gpui-pre` succeeds,
  `cargo tree -i gpui` and `cargo tree -i gpui-ce` both report "did not match any packages". The
  literal `cargo tree -i gpui` from the spec therefore can never succeed here, because the crate is
  named `gpui-pre`; the `spike.yml` check uses `gpui-pre` and fails if `gpui-ce` appears.

## Versions

Verified live on 2026-10-20 with `UA="sqler-spike (github.com/sqler)"`:
`curl -s -A "$UA" https://crates.io/api/v1/crates/<name> | jq -r '.crate.max_stable_version, .crate.updated_at, .crate.repository'`

```
gpui           0.2.2  2025-10-22T03:56:29Z  https://github.com/zed-industries/zed
gpui-ce        0.2.2  2026-08-28T23:15:07Z  https://github.com/gpui-ce/gpui-ce
gpui-component 0.8.0  2026-10-19T13:06:26Z  https://github.com/longbridge/gpui-kit
iced           0.14.0 2025-12-07T20:51:51Z  https://github.com/iced-rs/iced
tree-sitter    0.27.1 2026-10-08T17:45:20Z  https://github.com/tree-sitter/tree-sitter
memory-stats   1.2.0  2024-06-26T19:33:36Z  https://github.com/Arc-blroth/memory-stats
hyperfine      2.0.0  2026-10-07T21:17:20Z  https://github.com/sharkdp/hyperfine
```

Extra crates found while resolving the GPUI tree (same command):

```
gpui-pre          0.3.9  2026-10-12T00:44:57Z  https://github.com/zed-industries/zed   (Zed's GPUI snapshot of zed@93d7456)
gpui-pre-platform 0.3.9  2026-10-12T00:45:05Z  https://github.com/zed-industries/zed   (Zed's gpui_platform)
gpui-base         0.8.0  2026-10-19T13:05:00Z  https://github.com/longbridge/gpui-kit
gpui-fast         0.1.10 2026-10-19T12:34:42Z  https://github.com/longbridge/gpui-kit/gpui-fast (optional feature, not used)
tree-sitter-sequel 0.3.11 2025-10-01          https://github.com/derekstride/tree-sitter-sql.git
```

GPUI dependency of the latest `gpui-component`
(`curl -s -A "$UA" https://crates.io/api/v1/crates/gpui-component/0.8.0/dependencies | jq -r '.dependencies[] | select(.crate_id|test("gpui")) | "\(.crate_id) \(.req) \(.optional)"'`):

```
gpui-base ^0.8.0 false
gpui-component-macros ^0.8.0 false
gpui-fast ^0.1.10 false          (dev dependency)
gpui-kit-assets ^0.8.0 false
gpui-pre =0.3.9 false
gpui-pre =0.3.9 false            (dev dependency)
gpui-pre-macros =0.3.9 false
gpui-pre-sum-tree =0.3.9 false
gpui-fast ^0.1.10 true           (optional)
```

`cargo info gpui-component@0.8.0`: license Apache-2.0, `rust-version: unknown`, 44 deactivated
features. SQL support: feature `tree-sitter-sql = ["tree-sitter", "dep:tree-sitter-sequel"]`
(also part of `tree-sitter-languages`). The spike enables only `tree-sitter-sql`. Resolved in
`Cargo.lock`: `tree-sitter 0.26.13` (gpui-component requires `^0.26.13`; the newest release is 0.27.1),
`tree-sitter-sequel 0.3.11`, `wgpu 29.0.4`.

GitHub Actions (`gh api repos/<action>/releases/latest --jq '"\(.tag_name) \(.published_at)"'`):

```
actions/upload-artifact   v7.0.2   2026-10-07T10:08:14Z
actions/checkout          v7.0.1   2026-07-20T15:10:05Z
Swatinem/rust-cache       v2.9.2   2026-08-06T06:26:27Z
taiki-e/install-action     v2.87.29 2026-10-16T16:10:45Z
sharkdp/hyperfine (GitHub release)  v2.0.0 2026-10-07T21:13:36Z   (taiki-e manifest hyperfine.json exists)
```

Runner images (`actions/runner-images` README): `ubuntu-latest` = Ubuntu 24.04 x64,
`macos-latest` = macOS 26 arm64, `windows-latest` = Windows Server 2025.

Local toolchain for the manual run: Homebrew `cargo 1.99.0` / `rustc 1.99.0` (no rustup, so
`rust-toolchain.toml` 1.99.1 is not applied locally; CI installs 1.99.1), Xcode 27.0, macOS 27.0.1,
Apple M4 Pro (`Mac16,8`), 48 GB.

## Results

"pending CI" means the cell can only be produced by `spike.yml` on a GitHub runner, which needs the
branch to be pushed. Fill these from `gh run download <id>` (`results.txt`, `cold.json`) and replace
`Run` with the Actions run URL. Policy for reading them: a target fails only if GPUI itself fails
there. A failure caused only by the CI environment (for example no software Vulkan) is worked around
where feasible, and otherwise reported as `inconclusive (environment)`, not as a failure. The manual
macOS run weighs most.

| Target | How obtained | Run | Build | Grid | Editor | Cold start ms (hyperfine mean) | first_frame_ms | rss_mb | Binary MB | Renderer | Notes |
| ------ | ------------ | --- | ----- | ---- | ------ | ------------------------------ | -------------- | ------ | --------- | -------- | ----- |
| macOS | manual (Apple M4 Pro, macOS 27.0.1, Xcode 27.0) | base 9b45788 + uncommitted W1 tree (replace with the commit hash) | pass | pass (smoke) | pass (smoke) | 157.8 (3 runs: 157.9, 158.4, 157.2) | 191.7 (3 runs: 178.6, 202.4, 194.2) | 151.4 (3 runs: 151.7, 151.7, 150.9) | 26.4 | metal | `scripts/measure.sh`, release `--locked`, unstripped. Peak RSS (`/usr/bin/time -l`, `--smoke`) 151.2 to 152.2 MB. The 2-minute interactive check (trackpad scroll, type and edit SQL, resize the split) is NOT done: an agent cannot do it. Pending the developer. |
| Linux X11 | CI (ubuntu-latest, xvfb) | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | workflow written, never run: apt list and Vulkan (lavapipe) are untested |
| Linux Wayland | CI (ubuntu-latest, headless weston) | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | workflow written, never run: weston flags are untested |
| macOS | CI (macos-latest) | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | VM Metal; Metal toolchain step untested |
| Windows | CI (windows-latest) | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | pending CI | WARP or no GPU; untested |

Reading the macOS numbers: `first_frame_ms` is main() start to the first frame callback and leaves out
exec and dynamic-loader time. Cold start is the hyperfine mean of the whole process
(`--exit-after-first-frame`, 3 warm-ups, 20 runs) and includes teardown. Idle RSS of 151 MB is with
1M rows loaded and the editor open, no database connections; it is not the "three connections"
metric of `docs/src/benchmarks.md`.

Verdict so far (AC5): not decidable. GPUI passes on the one target that could be run (macOS). The
other three targets are `pending CI`; by the spec's rule the verdict needs Build, Grid and Editor to
pass on all four targets.

## Quirks

- **Metal toolchain (macOS).** The first `cargo build` failed in `gpui-pre-apple`'s build script:
  `cannot execute tool 'metal' due to missing Metal Toolchain`. Fix:
  `xcodebuild -downloadComponent MetalToolchain` (about 840 MB). `spike.yml` runs
  `xcrun metal --version || xcodebuild -downloadComponent MetalToolchain` on macOS.
- **Resolved GPUI crate is `gpui-pre`, not `gpui` (all targets).** See the distribution choice.
  `cargo tree -i gpui` cannot match; use `cargo tree -i gpui-pre`.
- **`gpui_platform` is a separate crate (all targets).** GPUI has no `application()` itself; the
  spike depends on `gpui-pre-platform` with features `font-kit`, `x11`, `wayland`.
- **hyperfine 2.0 JSON schema (all targets).** `cold.json` no longer has `results[0].mean`; the mean
  is `results[0].summary.time_wall_clock.mean`. The scripts read the new path and fall back to the old.
- **Stalled smoke runs on macOS (unexplained).** Two of the first runs on the manual machine ran for
  84 s and for the full 120 s watchdog (the watchdog printed `result=fail reason=watchdog`, exit 2)
  while frames were not being presented; every later run (about ten, with the same binary) took 1.3 to 2.5 s. Probable cause: the display link does not fire while the screen is idle or the window is
  occluded. Not reproduced after the first two runs; the watchdog exists so CI cannot hang. Treat a
  watchdog failure on a CI target as inconclusive (environment) until it repeats.
- **Screenshots (macOS).** `screencapture` failed with "could not create image from display" in the
  agent's session, so the rendered pixels were never looked at; the pass rests on the SPIKE report
  (rows requested from the table delegate, highlight names from the component's highlighter).
- **Linux packages (untested).** Starting list: `libfontconfig-dev libwayland-dev libxkbcommon-dev
  libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev libvulkan1 mesa-vulkan-drivers vulkan-tools
  fonts-dejavu-core xvfb xauth weston pkg-config`. Trim or extend after the first CI run and record
  the final list here.
