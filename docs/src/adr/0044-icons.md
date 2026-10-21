# 0044 Icons

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

No `AssetSource` is registered in `run`, so every `gpui-component` icon (menus, notifications, tree) renders blank, and SQLer has no icons of its own: chevrons are text glyphs and the tab close is a "×". The workbench needs a consistent icon set at 14, 16 and 20 px. A new dependency or vendored files need an ADR (AGENTS.md section 8).

## Decision drivers

- Icons render in both themes at HiDPI, are tinted by the text colour, and are cheap to embed.
- Licence bookkeeping is simple; no network at run time.
- Binary budget: +300 KiB for the whole of M4.5.

## Considered options

1. `gpui-kit-assets` 0.8.0 as a direct dependency, with `icon_assets!` for a selected Lucide set plus its component `Assets`.
2. `gpui-kit-assets` `AllAssets`.
3. Hand-copied SVG files in `assets/`.

## Decision outcome

Option 1. `gpui-kit-assets =0.8.0` is already resolved through `gpui-component`, so no new crate enters the tree; it becomes a direct dependency of `sqler-ui` at the same pinned version. The `icon_assets!` macro embeds only the selected Lucide icons (42 SVGs, +20 KiB), composed with `gpui_kit_assets::Assets` (the 106 component icons, which fixes today's blank component icons) behind `theme::icons::AppAssets`. `AppIcon` maps to `IconName`. The set is listed in [Workbench design](../architecture/workbench.md), section 3.

Licensing: the crate is Apache-2.0; Lucide icons are ISC with Feather-derived parts under MIT. The licence text is copied to `licenses/LUCIDE-ISC.txt` and listed in `docs/src/dependencies.md`. `cargo deny` must stay green.

Rejected:

- `AllAssets`: about +1 MiB for icons we do not use.
- Hand-copied SVGs: drift from upstream and manual licence bookkeeping.

## Consequences

- One line in `run()` registers `AppAssets` (owned by the visual system item, U1); the screenshot harness uses the same source.
- Adding an icon means adding one `AppIcon` variant and one name to the macro list; an icon outside the list does not render.
- Assets are referenced lazily, so cold start is unaffected (measured in U1).
