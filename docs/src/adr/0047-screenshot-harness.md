# 0047 Screenshot harness

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

The UI work of M4.5 is visual, and CI has no display. Reviewers and the owner need to see the result in pull requests and in the iteration report. Until now GUI behaviour was verified headless only, without images.

## Decision drivers

- Runs in CI on Linux without a window manager; runs locally on macOS.
- Captures the real `WorkspaceView` rendering, in both themes.
- Does not become a flaky merge gate through font or driver differences.
- Images are reviewable in pull requests and in the book.

## Considered options

1. GPUI `HeadlessAppContext` with the platform headless renderer.
2. Xvfb plus an OS screenshot tool.
3. Pixel-diff golden images.
4. An orphan `screenshots` branch for storing images.

## Decision outcome

Option 1. Details are in [Workbench design](../architecture/workbench.md), section 6.

- **Harness.** `crates/sqler-ui/tests/screenshots.rs` with `harness = false`, `required-features = ["screenshots"]`, and feature `screenshots = ["gui", "gpui/test-support", "gpui_platform/test-support"]`. It runs on the main thread, builds `HeadlessAppContext::with_platform(...)` with an empty asset source (U6 merges before U1; U1 switches it to `AppAssets`) and the platform headless renderer, opens a 1280x800 window with `Root` and `WorkspaceView` from a scene script, then calls `run_until_parked`, draws, and `capture_screenshot`, saving PNG (dev-dependency `image` with `png`).
- **Renderers.** macOS: Metal headless. Linux: wgpu headless on lavapipe (`mesa-vulkan-drivers libvulkan1 fonts-dejavu-core`). Windows: no headless renderer, so the harness prints `skipped: no headless renderer on windows` and exits 0.
- **Checks.** A scene fails on a wrong image size or fewer than 16 distinct colours. Optional layout checks use `window.painted_quads()`.
- **Scenes.** Twelve scenes (welcome with recent connections, welcome empty, editor with results, prod editor, tab overflow, split editors, object DDL, palette, messages error, settings, collapsed panels, connection dialog), each in both themes. Menus, hover and drag cannot be scripted and go to the owner checklist.
- **Output.** `$SQLER_SCREENSHOT_DIR` (default `target/screenshots/`) as `<scene>-<dark|light>.png`. Reviewed images are committed to `docs/src/screenshots/` by the tester on macOS and embedded in PR bodies by commit URL and in the book.
- **CI.** A required Linux `screenshots` job uploads `target/screenshots` as an artifact; a macOS job with `continue-on-error: true` runs the same.

Rejected:

- Xvfb plus OS tools: flaky and needs a window manager.
- An orphan `screenshots` branch: extra push handling, and the images stay out of the book.
- Pixel-diff goldens: font rasterisation differs per OS, so screenshots are for judging, not for comparison.

## Consequences

- Linux screenshots use DejaVu fonts and look different from macOS; they are not goldens.
- A scene that needs a real database falls back to the synthetic grid and fixture state if tokio work proves flaky; the scene list does not change.
- The harness adds a dev-dependency and a feature but no production code.
