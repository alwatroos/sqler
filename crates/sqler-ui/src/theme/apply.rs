// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maps [`Appearance`] and the UI font size onto `gpui-component`'s theme.

use std::rc::Rc;

use gpui::{App, Global, WindowAppearance, px};
use gpui_component::{Theme, ThemeConfig, ThemeMode, ThemeSet};

use super::{Appearance, THEMES_JSON};

/// The two built-in `SQLer` themes, parsed once.
struct SqlerThemes {
    light: Rc<ThemeConfig>,
    dark: Rc<ThemeConfig>,
}

impl Global for SqlerThemes {}

fn parse() -> Result<SqlerThemes, String> {
    let set: ThemeSet = serde_json::from_str(THEMES_JSON).map_err(|e| e.to_string())?;
    let find = |name: &str| {
        set.themes
            .iter()
            .find(|t| t.name.as_ref() == name)
            .cloned()
            .map(Rc::new)
            .ok_or_else(|| format!("theme \"{name}\" is missing"))
    };
    Ok(SqlerThemes {
        light: find("SQLer Light")?,
        dark: find("SQLer Dark")?,
    })
}

/// Parses the embedded themes on first use. A failure keeps the `gpui-component` built-ins.
fn install(cx: &mut App) {
    if cx.has_global::<SqlerThemes>() {
        return;
    }
    match parse() {
        Ok(themes) => cx.set_global(themes),
        Err(e) => eprintln!("warning: built-in SQLer themes unavailable: {e}"),
    }
}

/// Switches the global theme to `appearance` and sets the UI font size in points.
pub(crate) fn apply(appearance: Appearance, ui_font_size: u16, cx: &mut App) {
    let mode = match appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
    install(cx);
    if !cx.has_global::<Theme>() {
        // `change` creates the global theme from the registry defaults.
        Theme::change(mode, None, cx);
    }
    if cx.has_global::<SqlerThemes>() {
        let (light, dark) = {
            let t = cx.global::<SqlerThemes>();
            (t.light.clone(), t.dark.clone())
        };
        Theme::update(cx, |theme| {
            theme.light_theme = light;
            theme.dark_theme = dark;
        });
    }
    // Reloads the config of `mode` even when the mode is unchanged.
    Theme::change(mode, None, cx);
    // `change` reloads the theme config and may reset the font size, so set it afterwards.
    Theme::update(cx, |theme| theme.font_size = px(f32::from(ui_font_size)));
}

/// The look of an OS window appearance; the vibrant variants map like `gpui-component` does.
pub(crate) fn appearance_of(appearance: WindowAppearance) -> Appearance {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use gpui::{TestAppContext, px, rgb};
    use gpui_component::ActiveTheme as _;

    use super::*;

    #[test]
    fn themes_parse_with_every_color_key() {
        let set: ThemeSet = serde_json::from_str(THEMES_JSON).expect("parse");
        assert_eq!(set.themes.len(), 2);
        for theme in &set.themes {
            let value = serde_json::to_value(&theme.colors).expect("serialise");
            let map = value.as_object().expect("object");
            let present = map.values().filter(|v| !v.is_null()).count();
            assert_eq!(present, 58, "{}", theme.name);
        }
    }

    #[test]
    fn parsing_is_fast() {
        let best = (0..20)
            .map(|_| {
                let t = Instant::now();
                let set: ThemeSet = serde_json::from_str(THEMES_JSON).expect("parse");
                std::hint::black_box(set);
                t.elapsed()
            })
            .min()
            .expect("runs");
        // Generous ceiling so shared CI runners do not flake; the 1 ms release
        // budget is a measurement note in docs/src/architecture/workbench.md, not a gate.
        assert!(best.as_millis() < 50, "{best:?}");
    }

    #[gpui::test]
    fn apply_switches_colors_font_and_radius(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply(Appearance::Dark, 14, cx);
            assert_eq!(cx.theme().background, rgb(0x0028_2c33).into());
            apply(Appearance::Light, 15, cx);
            assert_eq!(cx.theme().background, rgb(0x00fa_fafa).into());
            assert_eq!(cx.theme().font_size, px(15.));
            assert_eq!(cx.theme().radius, px(6.));
        });
    }
}
