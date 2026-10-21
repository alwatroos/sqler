// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The settings tab: its header at the end of the tab strip and the settings page. Colours come
//! from the theme tokens only; every click becomes a `WorkspaceAction`.

use gpui::{
    ClickEvent, Context, Div, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::ActiveTheme as _;
use sqler_config::{FONT_SIZE_RANGE, ThemePreference};

use crate::workspace::{Workspace, WorkspaceAction, WorkspaceView};

/// A clickable chip that dispatches `action`.
fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    action: WorkspaceAction,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Stateful<Div> {
    let theme = cx.theme();
    let (border, fg) = (theme.border, theme.foreground);
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(border)
        .text_color(fg)
        .cursor_pointer()
        .child(label.into())
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
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
    let active = ws.settings_tab_active();
    Some(
        div()
            .id("settings-tab")
            .h_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .bg(if active { active_fill } else { rest_fill })
            .text_color(if active { active_text } else { fg })
            .border_l_1()
            .border_r_1()
            .border_color(border)
            .cursor_pointer()
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.dispatch(WorkspaceAction::OpenSettings, cx);
            }))
            .child(div().text_sm().child("Settings"))
            .child(
                div()
                    .id("settings-close")
                    .px_1()
                    .rounded_sm()
                    .text_color(muted)
                    .cursor_pointer()
                    .child("×")
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                        view.dispatch(WorkspaceAction::CloseSettings, cx);
                        cx.stop_propagation();
                    })),
            ),
    )
}

/// A labelled settings row.
fn row(label: &'static str, muted: Hsla, content: Div) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_4()
        .child(div().w(px(120.)).text_color(muted).child(label))
        .child(content)
}

/// The settings page, shown instead of the tab body while the settings tab is active.
pub(crate) fn page(ws: &Workspace, cx: &mut Context<WorkspaceView>) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let (primary, primary_fg) = (theme.primary, theme.primary_foreground);
    let settings = ws.settings().settings();
    let (current_theme, size) = (settings.theme, settings.ui_font_size);

    let mut choices = div().flex().flex_row().gap_2();
    for (id, label, pref) in [
        ("theme-light", "Light", ThemePreference::Light),
        ("theme-dark", "Dark", ThemePreference::Dark),
        ("theme-system", "Follow system", ThemePreference::System),
    ] {
        let mut choice = button(id, label, WorkspaceAction::SetTheme(pref), cx);
        if current_theme == pref {
            choice = choice
                .bg(primary)
                .text_color(primary_fg)
                .border_color(primary);
        }
        choices = choices.child(choice);
    }

    let font = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(button(
            "font-smaller",
            "−",
            WorkspaceAction::SetUiFontSize(size.saturating_sub(1)),
            cx,
        ))
        .child(
            div()
                .min_w(px(48.))
                .text_center()
                .child(format!("{size} pt")),
        )
        .child(button(
            "font-larger",
            "+",
            WorkspaceAction::SetUiFontSize(size.saturating_add(1)),
            cx,
        ))
        .child(div().text_color(muted).child(format!(
            "{} to {}",
            FONT_SIZE_RANGE.start(),
            FONT_SIZE_RANGE.end()
        )));

    let mut page = div()
        .id("settings-page")
        .flex_1()
        .w_full()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .child(div().text_xl().child("Settings"))
        .child(row("Theme", muted, choices))
        .child(row("UI font size", muted, font));
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
            .text_sm()
            .child("Saved to settings.toml in the config directory (see User guide > Settings)."),
    )
}
