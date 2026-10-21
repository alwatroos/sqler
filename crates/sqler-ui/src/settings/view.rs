// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The settings tab: its header at the end of the tab strip and the settings page. Colours come
//! from the theme tokens only; every click becomes a `WorkspaceAction`.

use gpui::{
    ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _};
use sqler_config::{FONT_SIZE_RANGE, ThemePreference};

use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{icon_button, section_header};
use crate::workspace::chrome::{ICON_SM, field_row, ui_scale};
use crate::workspace::{Workspace, WorkspaceAction, WorkspaceView};

/// Font weight of the page title.
const TITLE_WEIGHT: f32 = 600.;
/// Minimum width of the font size read-out.
const SIZE_READOUT_W: f32 = 48.;

/// A small button that dispatches `action`: primary when `chosen`, outline otherwise.
fn choice(
    id: &'static str,
    label: impl Into<SharedString>,
    chosen: bool,
    action: WorkspaceAction,
    cx: &mut Context<WorkspaceView>,
) -> Button {
    let button = Button::new(id).label(label.into()).small();
    let button = if chosen {
        button.primary()
    } else {
        button.outline()
    };
    button.on_click(cx.listener(move |view, _, _, cx| {
        view.dispatch(action, cx);
    }))
}

/// The Settings tab, drawn at the right end of the tab strip of the first group while it is
/// open. The activity bar gear opens it; the theme is chosen on the page.
pub(crate) fn strip_end(
    ws: &Workspace,
    cx: &mut Context<WorkspaceView>,
) -> Option<impl IntoElement + use<>> {
    if !ws.settings_tab_open() {
        return None;
    }
    let theme = cx.theme();
    let (border, rest_fill, active_fill) = (theme.border, theme.tab, theme.tab_active);
    let (fg, active_text, muted) = (
        theme.tab_foreground,
        theme.tab_active_foreground,
        theme.muted_foreground,
    );
    let sm = ui_scale(cx).sm;
    let active = ws.settings_tab_active();
    Some(
        div()
            .id("settings-tab")
            .h_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE[2]))
            .px(px(SPACE[3]))
            .bg(if active { active_fill } else { rest_fill })
            .text_color(if active { active_text } else { fg })
            .border_l_1()
            .border_r_1()
            .border_color(border)
            .cursor_pointer()
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.dispatch(WorkspaceAction::OpenSettings, cx);
            }))
            .child(
                Icon::new(AppIcon::Settings)
                    .size(px(ICON_SM))
                    .text_color(muted),
            )
            .child(div().text_size(px(sm)).child("Settings"))
            .child(
                icon_button("settings-close", AppIcon::X, "Close Settings").on_click(cx.listener(
                    |view, _: &ClickEvent, _, cx| {
                        view.dispatch(WorkspaceAction::CloseSettings, cx);
                        cx.stop_propagation();
                    },
                )),
            ),
    )
}

/// The settings page, shown instead of the tab body while the settings tab is active.
pub(crate) fn page(ws: &Workspace, cx: &mut Context<WorkspaceView>) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let scale = ui_scale(cx);
    let settings = ws.settings().settings();
    let (current_theme, size) = (settings.theme, settings.ui_font_size);

    let mut choices = div().flex().flex_row().gap(px(SPACE[2]));
    for (id, label, pref) in [
        ("theme-light", "Light", ThemePreference::Light),
        ("theme-dark", "Dark", ThemePreference::Dark),
        ("theme-system", "Follow system", ThemePreference::System),
    ] {
        choices = choices.child(choice(
            id,
            label,
            current_theme == pref,
            WorkspaceAction::SetTheme(pref),
            cx,
        ));
    }

    let stepper =
        |id: &'static str, label: &'static str, to: u16, cx: &mut Context<WorkspaceView>| {
            Button::new(id)
                .label(label)
                .small()
                .outline()
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.dispatch(WorkspaceAction::SetUiFontSize(to), cx);
                }))
        };
    let font = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .child(stepper(
            "font-smaller",
            "\u{2212}",
            size.saturating_sub(1),
            cx,
        ))
        .child(
            div()
                .min_w(px(SIZE_READOUT_W))
                .text_center()
                .child(format!("{size} pt")),
        )
        .child(stepper("font-larger", "+", size.saturating_add(1), cx))
        .child(
            div()
                .text_size(px(scale.sm))
                .text_color(muted)
                .child(format!(
                    "{} to {}",
                    FONT_SIZE_RANGE.start(),
                    FONT_SIZE_RANGE.end()
                )),
        );

    let mut page = div()
        .id("settings-page")
        .flex_1()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(SPACE[4]))
        .p(px(SPACE[6]))
        .child(
            div()
                .text_size(px(scale.xl))
                .font_weight(FontWeight(TITLE_WEIGHT))
                .child("Settings"),
        )
        .child(section_header("Appearance", cx).px(gpui::Pixels::ZERO))
        .child(field_row("Theme", choices, cx))
        .child(field_row("UI font size", font, cx));
    if let Some(notice) = ws.settings().notice() {
        page = page.child(div().text_color(danger).child(notice.to_owned()));
    }
    if !ws.settings().persistent() {
        page = page.child(
            div()
                .text_color(muted)
                .child("Changes apply to this session only."),
        );
    }
    page.child(
        div()
            .text_color(muted)
            .text_size(px(scale.sm))
            .child("Saved to settings.toml in the config directory (see User guide > Settings)."),
    )
}
