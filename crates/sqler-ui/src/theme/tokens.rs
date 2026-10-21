// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Design tokens that do not need `GPUI`: spacing, the type scale, environment accents, the
//! activity-bar foreground and the contrast maths. The colour tables of the two themes live in
//! `assets/themes/sqler.json`, embedded as [`THEMES_JSON`].
//!
//! SQLer-only aliases of theme colours (no key of their own in the JSON):
//! - activity bar background = `sidebar.background`
//! - active activity foreground = `foreground`
//! - active indicator = `ring`
//! - dirty dot = `muted.foreground`
//! - preview tab title = `tab.foreground`, italic

use sqler_driver_api::sqler_core::Environment;

use super::Appearance;
use crate::tabs::Rgb;

/// The embedded `ThemeSet` with the `SQLer Dark` and `SQLer Light` themes.
pub const THEMES_JSON: &str = include_str!("../../assets/themes/sqler.json");

/// The spacing scale in logical pixels.
pub const SPACE: [f32; 8] = [2., 4., 6., 8., 12., 16., 24., 32.];

/// Font sizes in points, derived from the UI font size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeScale {
    /// Section headers, chips (U-3).
    pub xs: f32,
    /// Secondary text (U-1).
    pub sm: f32,
    /// Body text (U).
    pub md: f32,
    /// Emphasis (U+2).
    pub lg: f32,
    /// Titles (U+6).
    pub xl: f32,
    /// Editor and grid text (U-1).
    pub mono: f32,
}

/// The type scale for the UI font size `ui` (default 14).
#[must_use]
pub fn type_scale(ui: u16) -> TypeScale {
    let u = f32::from(ui);
    TypeScale {
        xs: (u - 3.).max(1.),
        sm: (u - 1.).max(1.),
        md: u,
        lg: u + 2.,
        xl: u + 6.,
        mono: (u - 1.).max(1.),
    }
}

/// How an environment is shown on a chip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvAccent {
    /// Text colour, also the chip background at `chip_alpha`.
    pub fg: Rgb,
    /// Opacity of the tinted chip background.
    pub chip_alpha: f32,
    /// `(solid background, text on it)`; only for production.
    pub solid: Option<(Rgb, Rgb)>,
}

/// The chip colours of `e` in `a`. Unknown future environments look like `Dev`.
#[must_use]
pub fn env_accent(e: Environment, a: Appearance) -> EnvAccent {
    let dark = a == Appearance::Dark;
    let (fg, solid) = match e {
        Environment::Staging => (if dark { 0x00de_c184 } else { 0x008a_6d00 }, None),
        Environment::Prod => (
            if dark { 0x00e0_6c75 } else { 0x00c6_2828 },
            Some((
                Rgb(if dark { 0x00b8_3a3a } else { 0x00c6_2828 }),
                Rgb(0xff_ffff),
            )),
        ),
        _ => (if dark { 0x00a1_c181 } else { 0x003f_7d32 }, None),
    };
    EnvAccent {
        fg: Rgb(fg),
        chip_alpha: 0.18,
        solid,
    }
}

/// The chip text of `e`: `DEV`, `STAGING` or `PROD`.
#[must_use]
pub fn env_label(e: Environment) -> &'static str {
    match e {
        Environment::Staging => "STAGING",
        Environment::Prod => "PROD",
        _ => "DEV",
    }
}

/// The idle foreground of the activity bar icons.
#[must_use]
pub fn activity_foreground(a: Appearance) -> Rgb {
    match a {
        Appearance::Dark => Rgb(0x87_8a98),
        Appearance::Light => Rgb(0x7e_7f83),
    }
}

/// Tooltip text: the title, plus the shortcut in parentheses when there is one.
#[must_use]
pub fn tooltip_text(title: &str, shortcut: Option<&str>) -> String {
    match shortcut {
        Some(s) => format!("{title} ({s})"),
        None => title.to_owned(),
    }
}

fn luminance(c: Rgb) -> f32 {
    let lin = |shift: u32| {
        let v = f32::from(((c.0 >> shift) & 0xff) as u8) / 255.;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(16) + 0.7152 * lin(8) + 0.0722 * lin(0)
}

/// The WCAG 2.x contrast ratio of two colours, `1.0..=21.0`, independent of the order.
#[must_use]
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    const COLOR_KEYS: [&str; 58] = [
        "background",
        "foreground",
        "border",
        "input.border",
        "ring",
        "muted.background",
        "muted.foreground",
        "accent.background",
        "accent.foreground",
        "primary.background",
        "primary.foreground",
        "primary.hover.background",
        "primary.active.background",
        "secondary.background",
        "secondary.foreground",
        "popover.background",
        "popover.foreground",
        "sidebar.background",
        "sidebar.foreground",
        "sidebar.border",
        "sidebar.accent.background",
        "sidebar.accent.foreground",
        "list.background",
        "list.hover.background",
        "list.active.background",
        "list.active.border",
        "tab_bar.background",
        "tab.background",
        "tab.foreground",
        "tab.active.background",
        "tab.active.foreground",
        "status_bar.background",
        "status_bar.border",
        "table.background",
        "table.head.background",
        "table.head.foreground",
        "table.even.background",
        "table.hover.background",
        "table.row.border",
        "table.active.background",
        "table.active.border",
        "selection.background",
        "caret",
        "scrollbar.background",
        "scrollbar.thumb.background",
        "scrollbar.thumb.hover.background",
        "danger.background",
        "danger.foreground",
        "danger.hover.background",
        "warning.background",
        "warning.foreground",
        "success.background",
        "success.foreground",
        "info.background",
        "info.foreground",
        "drag.border",
        "drop_target.background",
        "overlay",
    ];
    const EDITOR_KEYS: [&str; 5] = [
        "editor.background",
        "editor.foreground",
        "editor.active_line.background",
        "editor.line_number",
        "editor.active_line_number",
    ];
    const SYNTAX_KEYS: [&str; 11] = [
        "keyword",
        "string",
        "number",
        "boolean",
        "comment",
        "function",
        "type",
        "operator",
        "punctuation",
        "constant",
        "variable",
    ];

    fn themes() -> Vec<Value> {
        let root: Value = serde_json::from_str(THEMES_JSON).expect("valid JSON");
        root["themes"].as_array().expect("themes array").clone()
    }

    fn is_hex(s: &str) -> bool {
        s.strip_prefix('#').is_some_and(|d| {
            matches!(d.len(), 6 | 8)
                && d.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    }

    fn color(theme: &Value, key: &str) -> Rgb {
        let s = theme["colors"][key].as_str().expect(key);
        Rgb::parse(&s[..7]).expect(key)
    }

    #[test]
    fn json_has_two_themes_with_shape() {
        let t = themes();
        assert_eq!(t.len(), 2);
        for (theme, name, mode) in [
            (&t[0], "SQLer Dark", "dark"),
            (&t[1], "SQLer Light", "light"),
        ] {
            assert_eq!(theme["name"], name);
            assert_eq!(theme["mode"], mode);
            assert_eq!(theme["radius"], 6);
            assert_eq!(theme["radius.lg"], 8);
            assert_eq!(theme["shadow"], false);
            let obj = theme.as_object().expect("object");
            assert!(
                obj.keys()
                    .all(|k| !k.starts_with("font") && !k.starts_with("mono_font"))
            );
        }
    }

    #[test]
    fn json_has_every_key_and_valid_hex() {
        for theme in themes() {
            for key in COLOR_KEYS {
                let v = theme["colors"][key]
                    .as_str()
                    .unwrap_or_else(|| panic!("{key}"));
                assert!(is_hex(v), "{key}: {v}");
            }
            assert_eq!(
                theme["colors"].as_object().expect("colors").len(),
                COLOR_KEYS.len()
            );
            for key in EDITOR_KEYS {
                let v = theme["highlight"][key]
                    .as_str()
                    .unwrap_or_else(|| panic!("{key}"));
                assert!(is_hex(v), "{key}: {v}");
            }
            for key in SYNTAX_KEYS {
                let v = theme["highlight"]["syntax"][key]["color"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{key}"));
                assert!(is_hex(v), "{key}: {v}");
            }
        }
    }

    #[test]
    fn type_scale_default() {
        let s = type_scale(14);
        assert_eq!(
            (s.xs, s.sm, s.md, s.lg, s.xl, s.mono),
            (11., 13., 14., 16., 20., 13.)
        );
        assert!(type_scale(0).xs >= 1.);
    }

    #[test]
    fn env_accent_table() {
        let cases = [
            (Environment::Dev, 0x00a1_c181, 0x003f_7d32),
            (Environment::Staging, 0x00de_c184, 0x008a_6d00),
            (Environment::Prod, 0x00e0_6c75, 0x00c6_2828),
        ];
        for (env, dark, light) in cases {
            let d = env_accent(env, Appearance::Dark);
            let l = env_accent(env, Appearance::Light);
            assert_eq!((d.fg, l.fg), (Rgb(dark), Rgb(light)));
            assert!((d.chip_alpha - 0.18).abs() < f32::EPSILON);
            assert_eq!(d.solid.is_some(), env == Environment::Prod);
        }
        assert_eq!(
            env_accent(Environment::Prod, Appearance::Dark).solid,
            Some((Rgb(0x00b8_3a3a), Rgb(0x00ff_ffff)))
        );
        assert_eq!(
            env_accent(Environment::Prod, Appearance::Light).solid,
            Some((Rgb(0x00c6_2828), Rgb(0x00ff_ffff)))
        );
        assert_eq!(env_label(Environment::Dev), "DEV");
        assert_eq!(env_label(Environment::Staging), "STAGING");
        assert_eq!(env_label(Environment::Prod), "PROD");
    }

    #[test]
    fn activity_foreground_values() {
        assert_eq!(activity_foreground(Appearance::Dark), Rgb(0x0087_8a98));
        assert_eq!(activity_foreground(Appearance::Light), Rgb(0x007e_7f83));
    }

    #[test]
    fn tooltip_text_forms() {
        assert_eq!(tooltip_text("Run", None), "Run");
        assert_eq!(tooltip_text("Run", Some("⌘↩")), "Run (⌘↩)");
    }

    #[test]
    fn contrast_basics() {
        let (b, w) = (Rgb(0), Rgb(0xff_ffff));
        assert!((contrast(b, w) - 21.).abs() < 0.01);
        assert!((contrast(w, w) - 1.).abs() < f32::EPSILON);
        assert!((contrast(b, w) - contrast(w, b)).abs() < f32::EPSILON);
    }

    #[test]
    fn text_contrast_is_at_least_4_5() {
        let pairs = [
            ("foreground", "background"),
            ("muted.foreground", "background"),
            ("muted.foreground", "muted.background"),
            ("tab.foreground", "tab.background"),
            ("tab.active.foreground", "tab.active.background"),
            ("sidebar.foreground", "sidebar.background"),
            ("foreground", "status_bar.background"),
            ("popover.foreground", "popover.background"),
            ("primary.foreground", "primary.background"),
            ("danger.foreground", "danger.background"),
            ("success.foreground", "success.background"),
            ("warning.foreground", "warning.background"),
            ("info.foreground", "info.background"),
        ];
        for (theme, appearance) in themes().iter().zip([Appearance::Dark, Appearance::Light]) {
            for (fg, bg) in pairs {
                let c = contrast(color(theme, fg), color(theme, bg));
                assert!(c >= 4.5, "{} {fg} on {bg}: {c:.2}", theme["name"]);
            }
            if let Some((solid, on)) = env_accent(Environment::Prod, appearance).solid {
                assert!(contrast(on, solid) >= 4.5);
            }
        }
    }
}
