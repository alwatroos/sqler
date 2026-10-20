// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maps [`Appearance`] and the UI font size onto `gpui-component`'s theme.

use gpui::{App, WindowAppearance, px};
use gpui_component::{Theme, ThemeMode};

use super::Appearance;

/// Switches the global theme to `appearance` and sets the UI font size in points.
pub(crate) fn apply(appearance: Appearance, ui_font_size: u16, cx: &mut App) {
    let mode = match appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
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
