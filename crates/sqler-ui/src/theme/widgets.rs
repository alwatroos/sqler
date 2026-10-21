// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Small shared widgets: toolbar buttons, the environment chip, key hints, section headers and
//! empty states. Each returns a `gpui-component` `Button` or a GPUI `Div`, so callers keep
//! chaining `.on_click`, `.child` and `.disabled`. Colours come from the active theme or
//! `super::tokens`; sizes from [`SPACE`] and [`type_scale`].

use gpui::{
    App, Div, ElementId, FontWeight, ParentElement as _, SharedString, Styled as _, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _};
use sqler_driver_api::sqler_core::Environment;

use super::icons::AppIcon;
use super::{Appearance, SPACE, env_accent, env_label, type_scale};

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 1..=u16::MAX
fn scale(cx: &App) -> super::TypeScale {
    let size: f32 = cx.theme().font_size.into();
    type_scale(size.round().clamp(1., f32::from(u16::MAX)) as u16)
}

fn appearance(cx: &App) -> Appearance {
    if cx.theme().mode.is_dark() {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

/// A ghost icon button with a tooltip.
#[must_use]
pub fn icon_button(
    id: impl Into<ElementId>,
    icon: AppIcon,
    tooltip: impl Into<SharedString>,
) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .icon(icon)
        .tooltip(tooltip)
        .tab_stop(true)
}

/// A ghost icon button with a text label and a tooltip.
#[must_use]
pub fn toolbar_button(
    id: impl Into<ElementId>,
    icon: AppIcon,
    label: impl Into<SharedString>,
    tooltip: impl Into<SharedString>,
) -> Button {
    icon_button(id, icon, tooltip).label(label)
}

/// The environment chip: `DEV`, `STAGING` or `PROD`. Production is a solid fill.
#[must_use]
pub fn env_chip(env: Environment, cx: &App) -> Div {
    let accent = env_accent(env, appearance(cx));
    let chip = div()
        .h(px(18.))
        .px(px(SPACE[2]))
        .flex()
        .items_center()
        .rounded(cx.theme().radius)
        .text_size(px(scale(cx).xs))
        .font_weight(FontWeight::MEDIUM)
        .child(env_label(env));
    match accent.solid {
        Some((solid, on_solid)) => chip.bg(rgb(solid.0)).text_color(rgb(on_solid.0)),
        None => chip
            .bg(gpui::Hsla::from(rgb(accent.fg.0)).opacity(accent.chip_alpha))
            .text_color(rgb(accent.fg.0)),
    }
}

/// A keyboard shortcut hint such as `⌘↩`.
#[must_use]
pub fn kbd(label: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .px(px(SPACE[1]))
        .rounded(px(4.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted)
        .text_size(px(scale(cx).xs))
        .text_color(theme.muted_foreground)
        .child(label.into())
}

/// A panel header: the upper-case title on the left. The caller appends trailing buttons.
#[must_use]
pub fn section_header(title: impl Into<SharedString>, cx: &App) -> Div {
    let title: SharedString = title.into();
    div()
        .h(px(30.))
        .px(px(SPACE[4]))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(scale(cx).xs))
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().muted_foreground)
                .child(title.to_uppercase()),
        )
}

/// An empty panel: icon, title and one line. The caller appends an action.
#[must_use]
pub fn empty_state(
    icon: AppIcon,
    title: impl Into<SharedString>,
    line: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let scale = scale(cx);
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SPACE[2]))
        .child(
            Icon::new(icon)
                .size(px(20.))
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .text_size(px(scale.md))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(title.into()),
        )
        .child(
            div()
                .text_size(px(scale.sm))
                .text_color(theme.muted_foreground)
                .child(line.into()),
        )
}

#[cfg(test)]
mod tests {
    use gpui::{Context, IntoElement, Render, TestAppContext, Window};

    use super::*;
    use crate::theme::apply::apply;

    /// All six widgets in one column.
    struct Gallery;

    impl Render for Gallery {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .child(icon_button("run", AppIcon::Play, "Run"))
                .child(toolbar_button("save", AppIcon::Save, "Save", "Save (⌘S)"))
                .child(env_chip(Environment::Dev, cx))
                .child(env_chip(Environment::Staging, cx))
                .child(env_chip(Environment::Prod, cx))
                .child(kbd("⌘↩", cx))
                .child(section_header("Connections", cx).child(kbd("+", cx)))
                .child(empty_state(
                    AppIcon::Database,
                    "No connections",
                    "Add one.",
                    cx,
                ))
        }
    }

    #[gpui::test]
    fn gallery_renders_in_both_themes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
        });
        let (_gallery, cx) = cx.add_window_view(|_, _| Gallery);
        for look in [Appearance::Dark, Appearance::Light] {
            cx.update(|_, cx| apply(look, 14, cx));
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
    }
}
