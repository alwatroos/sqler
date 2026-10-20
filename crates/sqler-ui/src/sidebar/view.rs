// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The sidebar view: renders `SidebarModel`; clicks become `WorkspaceAction`s.

use gpui::{
    Context, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::ActiveTheme as _;
use sqler_driver_api::sqler_core::Environment;

use super::{ConnectionStatus, SidebarEntry, SidebarModel};
use crate::tabs::{Rgb, env_badge};
use crate::workspace::{WorkspaceAction, WorkspaceView};

/// Shown when there are no saved connections and no load error.
pub(crate) const EMPTY_TEXT: &str = "No saved connections. Add them to connections.toml in the config directory (see User guide > Settings).";

/// Renders the sidebar.
pub(crate) fn sidebar(
    model: &SidebarModel,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (bg, fg, border) = (
        theme.sidebar,
        theme.sidebar_foreground,
        theme.sidebar_border,
    );
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let selected_bg = theme.sidebar_accent;
    let mut list = div()
        .id("sidebar")
        .w(px(260.))
        .h_full()
        .flex()
        .flex_col()
        .flex_none()
        .gap_1()
        .p_2()
        .bg(bg)
        .text_color(fg)
        .border_r_1()
        .border_color(border)
        .child(div().text_xs().text_color(muted).child("CONNECTIONS"));

    if let Some(error) = model.load_error() {
        list = list.child(
            div()
                .text_sm()
                .text_color(danger)
                .child(format!("Could not load connections: {error}")),
        );
    } else if model.entries().is_empty() {
        list = list.child(div().text_sm().text_color(muted).child(EMPTY_TEXT));
    }

    let palette = Palette {
        border,
        muted,
        danger,
        selected_bg,
    };
    for (index, entry) in model.entries().iter().enumerate() {
        let selected = model.selected() == Some(index);
        list = list.child(connection_row(index, entry, selected, palette, cx));
    }
    list
}

/// The colours a row needs, copied out of the theme.
#[derive(Clone, Copy)]
struct Palette {
    border: Hsla,
    muted: Hsla,
    danger: Hsla,
    selected_bg: Hsla,
}

fn connection_row(
    index: usize,
    entry: &SidebarEntry,
    selected: bool,
    palette: Palette,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let Palette {
        border,
        muted,
        danger,
        selected_bg,
    } = palette;
    let config = entry.config();
    let dot = config
        .color
        .as_deref()
        .and_then(Rgb::parse)
        .map_or(border, |Rgb(c)| rgb(c).into());
    let mut row = div()
        .id(("connection", index))
        .flex()
        .flex_col()
        .gap_0p5()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .on_click(cx.listener(move |view, event: &gpui::ClickEvent, _, cx| {
            let action = if event.click_count() >= 2 {
                WorkspaceAction::OpenConnection(index)
            } else {
                WorkspaceAction::SelectConnection(index)
            };
            view.dispatch(action, cx);
        }));
    if selected {
        row = row.bg(selected_bg);
    }
    let mut title = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(div().size(px(8.)).flex_none().rounded_full().bg(dot))
        .child(div().flex_1().truncate().child(config.name.clone()));
    if let Some(badge) = env_badge(config.environment) {
        title = title.child(
            div()
                .text_xs()
                .text_color(if config.environment == Some(Environment::Prod) {
                    danger
                } else {
                    muted
                })
                .child(badge),
        );
    }
    row = row.child(title).child(
        div()
            .text_xs()
            .text_color(muted)
            .child(config.driver.to_string()),
    );
    match entry.status() {
        ConnectionStatus::Idle => {}
        ConnectionStatus::Connecting => {
            row = row.child(div().text_xs().text_color(muted).child("connecting…"));
        }
        ConnectionStatus::Failed(message) => {
            row = row.child(div().text_xs().text_color(danger).child(message.clone()));
        }
    }
    if selected {
        row = row.child(
            div()
                .id(("open-connection", index))
                .text_xs()
                .cursor_pointer()
                .child("Open in new tab (or double-click)")
                .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                    view.dispatch(WorkspaceAction::OpenConnection(index), cx);
                    cx.stop_propagation();
                })),
        );
    }
    row
}
