# ADR-0001: UI framework spike plan

- Status: accepted
- Superseded in part by [ADR-0069](0069-binary-size-budget-70-mb.md) (2026-10-23): the binary size budget is now 80 MB (83,886,080 bytes, amendment of ADR-0069, decision of 2026-10-23); the 60 MB figures below are the budget at the time of writing.
- Date: 2026-10-20
- Deciders: SQLer maintainers

## Context and Problem Statement

SQLer needs a native, GPU-accelerated Rust UI with a results grid that handles very large result
sets and a SQL editor with syntax highlighting, on Linux, macOS and Windows. The framework choice
is hard to reverse, so it is validated by a spike before the UI is built. This ADR is the spike
plan; the spike itself is M1.

## Decision Drivers

- Rust everywhere; no web view as the primary UI (AGENTS.md section 1, non-negotiables).
- Lightweight: cold start < 500 ms, idle RSS < 150 MB, release binary < 60 MB.
- A virtualised grid for 1M rows and a highlighted editor must work on all three OSes.
- Component availability, to avoid building a widget library from scratch.

## Considered Options

Versions from the 2026-10-20 survey in dependencies.md; re-verify at spike start.

- GPUI + gpui-component (default). Which crates.io distribution, `gpui` 0.2.2 or the `gpui-ce` fork
  0.2.2, is decided by the spike.
- iced 0.14.0 (fallback).
- Tauri / Dioxus-webview.

## Decision Outcome

Chosen option: GPUI + gpui-component. The M1 spike passed on all four targets, so iced is not used
and no superseding ADR is needed.

Distribution. The spike used the crates that the latest stable `gpui-component` depends on, so
the tree holds exactly one GPUI:

- `gpui-component` 0.8.0 (2026-10-19, Apache-2.0, repository `longbridge/gpui-kit`), feature
  `tree-sitter-sql` only.
- `gpui-pre` =0.3.9 (2026-10-12), a snapshot of Zed's GPUI (`zed@93d7456`), which `gpui-component`
  0.8.0 requires under that exact version. It is not `gpui` and not `gpui-ce`.
- `gpui-pre-platform` =0.3.9 (Zed's `gpui_platform`), listed directly because GPUI has no
  `application()` of its own and `gpui-component` does not re-export the platform crate.
- Not chosen: `gpui` 0.2.2 (2025-10-22) and `gpui-ce` 0.2.2 (2026-08-28). Neither can be unified with
  `gpui-component` 0.8.0 without a second GPUI in the tree. `cargo tree -i gpui-pre` succeeds, while
  `cargo tree -i gpui` and `cargo tree -i gpui-ce` match no package.
- All dependencies come from crates.io; there are no git dependencies and no fork.

Consequence of the name: the literal check `cargo tree -i gpui` from the original plan can never
succeed; use `cargo tree -i gpui-pre`.

Pass criteria were: both the grid and the editor work on Linux (X11 and Wayland), macOS and Windows.
Cold start, RSS and binary size are recorded as data in [Benchmarks](../benchmarks.md#ui-framework-spike),
not as pass/fail criteria.

Spike code is not merged into `crates/`. It stays in `spikes/ui-framework/` as a throwaway.

### Consequences

- The UI crate (`sqler-ui`) may start. It must pin `gpui-component` and `gpui-pre` together
  (`gpui-pre` is an exact `=` requirement of `gpui-component`) and bump them as one unit.
- `gpui-pre` is a snapshot of Zed's GPUI published under a different name; its release cadence and
  stability are outside our control. Re-check the pair at every bump (dependencies.md version policy).
- On macOS the build needs the Metal toolchain (`xcodebuild -downloadComponent MetalToolchain`, about
  840 MB) when it is not installed.
- Wayland needs a compositor that advertises `wl_seat`; GPUI panics without one. Real sessions have it.
- The first UI PR must carry the interactive macOS check that is still open (see below).

### Confirmation

Confirmed by the `SPIKE` report of `spike-gpui --smoke` on four CI targets and one manual macOS run
(`.github/workflows/spike.yml`, `spikes/ui-framework/`). Each target had to show a grid that
requested at most 500 rows per frame out of 1,000,000 and rendered the last row, and an editor whose
SQL highlighter produced at least 3 kinds of highlight.

## Pros and Cons of the Options

### GPUI + gpui-component

- Good: GPU-accelerated; ships a data table, a rope-based text editor and resizable panels.
- Bad: young ecosystem; distribution on crates.io must be chosen; platform maturity is unproven, hence the spike.

### iced

- Good: pure Rust, Elm-style, cross-platform.
- Bad: fewer ready-made components for a grid and an editor.

### Tauri / Dioxus-webview

- Rejected: a web view as the primary UI violates the non-negotiable in AGENTS.md section 1 (Rust everywhere, lightweight).

## More Information

- [Dependencies](../dependencies.md)
- Benchmark data: [Benchmarks, UI framework spike](../benchmarks.md#ui-framework-spike)
- Spike code and raw results: `spikes/ui-framework/` (`README.md` has the full results and quirks)
- Time box: 2026-10-20 to 2026-10-27 (start plus 7 days). The spike finished on 2026-10-20.
- Decision date: 2026-10-20.

### Spike results

CI run: [https://github.com/alwatroos/sqler/actions/runs/39418467982](https://github.com/alwatroos/sqler/actions/runs/39418467982).
Commit measured: 3c59a65 (CI rows, run 39418467982) and 4d3d5d1 (manual macOS row); spike code is the same, later commits changed the workflow, scripts and README only.

| Target | How obtained | Run | Grid | Editor |
| ------ | ------------ | --- | ---- | ------ |
| Linux X11 | CI, ubuntu-latest, xvfb, llvmpipe (software Vulkan) | run 39418467982 | pass (26 rows/frame max, last row rendered) | pass (12 highlight kinds) |
| Linux Wayland | CI, ubuntu-latest, headless cage, llvmpipe | run 39418467982 | pass (26 rows/frame max, last row rendered) | pass (12 highlight kinds) |
| macOS | CI, macos-latest arm64 VM, Metal | run 39418467982 | pass (21 rows/frame max, last row rendered) | pass (12 highlight kinds) |
| macOS | manual, Apple M4 Pro, macOS 27.0.1, Metal | commit 4d3d5d1 | pass (26 rows/frame max, last row rendered) | pass (12 highlight kinds) |
| Windows | CI, windows-latest, WARP (software D3D) | run 39418467982 | pass (24 rows/frame max, last row rendered) | pass (12 highlight kinds) |

Build passed on every target. Two earlier failures were environment only and were fixed: headless
`weston` has no `wl_seat` (replaced by headless `cage`), and `hyperfine` on Windows needed `-N` and a
quoted path. A first-run macOS stall (two of about twelve manual runs, one hit the 120 s watchdog) did
not recur on CI or in later runs.

Proven:

- A column-stored 1M-row source behind the gpui-component `DataTable` renders only the visible rows
  (at most 26 per frame) and can be scrolled to the last row, on all four targets.
- The gpui-component tree-sitter `SyntaxHighlighter` in SQL mode produces highlight kinds for the
  sample text, with no fork and no external grammar crate. The kinds come from a separate
  `SyntaxHighlighter` run over the sample text, not from a query of the live editor state.
- The same binary builds with `--locked` and starts on Linux (X11, Wayland), macOS and Windows.
- Build, cold start and binary size on all targets: cold start 158 to 206 ms and binary 26 to 58 MB,
  both inside the targets in Benchmarks. On the macOS manual run first frame was about 192 ms.

Not proven:

- An unexplained macOS stall: one manual run took 84 s and one hit the 120 s watchdog. About ten
  later runs were fast. The macOS console was found screen-locked on a later run, which is the
  probable cause, but this is unconfirmed.
- The interactive macOS check (2 minutes: trackpad scroll from top to bottom, typing and editing SQL,
  resizing the split) has NOT been done. An agent cannot do it. It is open and is a follow-up for the
  first UI PR. The pass on macOS rests on the scripted smoke test, not on a person looking at pixels.
- Rendering quality: no screenshot was inspected (macOS `screencapture` failed in the agent session).
- Real GPU behaviour on Linux and Windows. CI used software renderers (llvmpipe, WARP) and a VM
  Metal device, so first-frame and GPU numbers are indicative only. First frame varied up to about 3.6x
  between runs on the shared VMs (macOS 934 vs 2982 ms).
- Wayland was tested only under the `cage` compositor, not under GNOME or KDE.
- Real input (keyboard, IME, mouse wheel), accessibility, clipboard and drag-and-drop were not
  exercised.
- Idle RSS: 151 MB on the manual macOS run with 1M rows and the editor, no connections. The 150 MB
  target counts three open connections and needs the real UI, so it stays open until M1 data exists.
  Linux CI measured 264 to 268 MB, partly the software renderer.
