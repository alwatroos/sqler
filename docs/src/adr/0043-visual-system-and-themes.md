# 0043 Visual system and themes

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

Sizes are hard-coded in 157 `px(..)`/`text_*` call sites in 14 view files, there is no spacing or type scale, and the theme is the built-in `gpui-component` light/dark pair plus a font size. Environment badges reuse `danger` and `border`. The owner wants a Zed-like look in dark and light, and a production connection that is clearly marked.

## Decision drivers

- One source of truth for spacing, type, radius, states and colours; no literal colours in views.
- `theme`, `theme:toggle-mode`, `settings:set-theme` and `settings:set-ui-font-size` keep working; `settings.toml` is unchanged.
- Cold start and binary size budgets (+300 KiB total).
- Legible text in both themes.

## Considered options

1. Keep the built-in themes and tweak per view.
2. Embed two own `ThemeConfig` JSON themes plus a small Rust token module.
3. Also embed fonts (IBM Plex, OFL).
4. A custom title bar, and a user theme directory now.

## Decision outcome

Option 2. Details are in [Workbench design](../architecture/workbench.md), section 3.

- **Scales.** Spacing `[2, 4, 6, 8, 12, 16, 24, 32]`; sizes activity 44, tab bar 32, toolbar 32, panel header 30, status 24, row 24; type scale from `ui_font_size` U (xs U-3, sm U-1, md U, lg U+2, xl U+6, mono U-1); radius 4/6/8; flat 1 px borders, no shadows.
- **Themes.** `crates/sqler-ui/assets/themes/sqler.json` is a `gpui-component` `ThemeSet` with "SQLer Dark" and "SQLer Light", modelled on Zed One Dark and One Light. `ThemePreference` (`light`/`dark`/`system`) now maps to them. `apply` installs them before `Theme::change(mode)` and keeps the font-size override after it. The look changes for everybody (intended).
- **Rust extras.** `theme/tokens.rs` holds the tokens that are not in `ThemeConfig` (activity bar colours, dirty dot, preview style) and `env_accent` (chip = foreground at alpha 0.18; prod chip solid with `on_solid`).
- **Contrast.** A headless test requires at least 4.5:1 for the listed text/background pairs and for prod `on_solid` on `solid`, in both themes.
- **Prod status bar (Q4, default taken; owner may overrule by follow-up).** When the focused tab's connection is `prod`, the whole status bar turns solid red with white text, in addition to the tab top border and the PROD chips. Colour is never the only signal: the chip carries the text.
- **States.** Hover, pressed, selected, focus (`ring`) and disabled (50 % opacity) are fixed in the design page.
- **Fonts.** System UI font and platform mono.

Rejected:

- Embedded fonts: about 400 KB and font load time for little gain.
- A user theme directory now: deferred. The JSON is `ThemeRegistry`-compatible, so a `themes/` directory can be added later without a format change.
- A custom title bar: the native one stays because of Linux CSD and Windows control risk.
- Per-view tweaks on the built-in themes: no single source, drift.

## Consequences

- Existing users see a new palette after upgrading; there is no user theme file to migrate.
- Views must take colours from `cx.theme()` and `theme::tokens` only.
- Theme JSON parse is measured against the 1 ms budget (ADR-0045 budgets).
