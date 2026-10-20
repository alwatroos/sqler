// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tab strip and tab body views: render `TabStrip`; clicks become `WorkspaceAction`s.

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::ActiveTheme as _;

use super::TabStrip;
use crate::workspace::{WorkspaceAction, WorkspaceView};

/// Renders the tab strip.
///
/// With `highlight_active` false (the settings tab is shown) no connection tab gets the active
/// fill or text.
pub(crate) fn tab_strip(
    tabs: &TabStrip,
    highlight_active: bool,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (bar, border, fg, muted) = (
        theme.tab_bar,
        theme.border,
        theme.tab_foreground,
        theme.muted_foreground,
    );
    let (rest_fill, active_fill, active_text) =
        (theme.tab, theme.tab_active, theme.tab_active_foreground);
    let (danger, danger_fg) = (theme.danger, theme.danger_foreground);
    let active_id = tabs.active().map(super::Tab::id);

    let mut strip = div()
        .id("tab-strip")
        .w_full()
        .h(px(36.))
        .flex()
        .flex_row()
        .flex_none()
        .bg(bar)
        .border_b_1()
        .border_color(border);

    for tab in tabs.iter() {
        let id = tab.id();
        let header = tab.header();
        let active = highlight_active && active_id == Some(id);
        // Prod tabs get the theme's danger colour as a tint; a theme change recolours them.
        let mut bg = if active { active_fill } else { rest_fill };
        if header.is_prod {
            bg = danger.opacity(if active { 0.30 } else { 0.18 });
        }
        let stripe = header.accent.map_or(border, |super::Rgb(c)| rgb(c).into());
        let mut item = div()
            .id(("tab", id.raw()))
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap_2()
            .pr_2()
            .bg(bg)
            .text_color(if active { active_text } else { fg })
            .border_r_1()
            .border_color(border)
            .cursor_pointer()
            .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                view.dispatch(WorkspaceAction::ActivateTab(id), cx);
            }))
            .child(div().w(px(4.)).h_full().flex_none().bg(stripe))
            .child(div().text_sm().child(header.title.clone()));
        if let Some(badge) = header.badge {
            let (chip_fill, chip_ink) = if header.is_prod {
                (danger, danger_fg)
            } else {
                (border, muted)
            };
            item = item.child(
                div()
                    .text_xs()
                    .px_1()
                    .rounded_sm()
                    .bg(chip_fill)
                    .text_color(chip_ink)
                    .child(badge),
            );
        }
        item = item.child(
            div()
                .id(("tab-close", id.raw()))
                .px_1()
                .rounded_sm()
                .text_color(muted)
                .cursor_pointer()
                .child("×")
                .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                    view.dispatch(WorkspaceAction::CloseTab(id), cx);
                    cx.stop_propagation();
                })),
        );
        strip = strip.child(item);
    }
    strip
}

/// Renders the body of the active tab (a placeholder until the SQL editor arrives).
pub(crate) fn tab_body(
    tabs: &TabStrip,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    let text = match tabs.active() {
        Some(tab) => format!(
            "Connected to {} ({}). The SQL editor arrives in M3.",
            tab.connection().name,
            tab.connection().driver
        ),
        None => "Double-click a connection in the sidebar to open it in a tab.".to_owned(),
    };
    div()
        .id("tab-body")
        .flex_1()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(muted)
        .child(text)
}
