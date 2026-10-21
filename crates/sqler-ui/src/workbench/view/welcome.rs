// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The welcome view: shown in the editor area while no tab is open.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Role, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon};

use super::{Hints, run_command_id, scale};
use crate::palette::OPEN_PALETTE;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{env_chip, kbd};
use crate::workbench::{WelcomeModel, welcome_model};
use crate::workspace::{WorkspaceAction, WorkspaceView};

/// One entry of the left column.
struct Entry {
    id: &'static str,
    icon: AppIcon,
    label: &'static str,
    command: &'static str,
    enabled: bool,
}

/// The welcome view.
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn welcome(
    view: &WorkspaceView,
    hints: &Hints,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let model: WelcomeModel = welcome_model(view.workspace());
    let theme = cx.theme();
    let (bg, fg, muted, hover, ring) = (
        theme.background,
        theme.foreground,
        theme.muted_foreground,
        theme.list_hover,
        theme.ring,
    );
    let sc = scale(cx);

    let title = div()
        .text_size(px(sc.xl))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(fg)
        .child("SQLer");

    let body = if model.has_connections {
        let entries = [
            Entry {
                id: "welcome-new-connection",
                icon: AppIcon::Plus,
                label: "New Connection",
                command: "connection:new",
                enabled: true,
            },
            Entry {
                id: "welcome-new-editor",
                icon: AppIcon::FileCode,
                label: "New SQL Editor",
                command: "editor:new",
                enabled: model.can_new_editor,
            },
            Entry {
                id: "welcome-open-saved",
                icon: AppIcon::Bookmark,
                label: "Open Saved Query",
                command: "query:open-saved",
                enabled: true,
            },
            Entry {
                id: "welcome-all-commands",
                icon: AppIcon::Command,
                label: "Show All Commands",
                command: OPEN_PALETTE,
                enabled: true,
            },
        ];
        let mut left = div().flex().flex_col().gap(px(SPACE[1])).w(px(260.));
        for e in entries {
            let mut row = div()
                .id(e.id)
                .h(px(32.))
                .px(px(SPACE[3]))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE[3]))
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(gpui::transparent_black())
                .text_size(px(sc.md))
                .child(Icon::new(e.icon).size(px(16.)).text_color(muted))
                .child(div().flex_1().child(e.label))
                .children(hints.get(e.command).map(|h| kbd(h, cx)));
            row = if e.enabled {
                let command = e.command;
                row.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .focusable()
                    .tab_stop(true)
                    .focus_visible(move |s| s.border_color(ring))
                    .role(Role::Button)
                    .aria_label(e.label)
                    .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                        run_command_id(view, command, window, cx);
                    }))
            } else {
                row.opacity(0.5).tooltip(|window, cx| {
                    Tooltip::new("Select a connection first").build(window, cx)
                })
            };
            left = left.child(row);
        }

        let mut right = div().flex().flex_col().gap(px(SPACE[1])).w(px(260.)).child(
            div()
                .px(px(SPACE[3]))
                .pb(px(SPACE[1]))
                .text_size(px(sc.sm))
                .text_color(muted)
                .child("Recent connections"),
        );
        if model.recent.is_empty() {
            right = right.child(
                div()
                    .px(px(SPACE[3]))
                    .text_size(px(sc.sm))
                    .text_color(muted)
                    .child("Connections you open appear here."),
            );
        }
        for item in model.recent {
            let index = item.index;
            right = right.child(
                div()
                    .id(("welcome-recent", index))
                    .h(px(32.))
                    .px(px(SPACE[3]))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE[3]))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .text_size(px(sc.md))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .focusable()
                    .tab_stop(true)
                    .focus_visible(move |s| s.border_color(ring))
                    .role(Role::Button)
                    .aria_label(item.name.clone())
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        view.dispatch(WorkspaceAction::OpenConnection(index), cx);
                    }))
                    .child(Icon::new(AppIcon::Database).size(px(16.)).text_color(muted))
                    .child(div().flex_1().child(item.name))
                    .children(item.env.map(|e| env_chip(e, cx))),
            );
        }
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(SPACE[7]))
            .child(left)
            .child(right)
            .into_any_element()
    } else {
        Button::new("welcome-add-first")
            .primary()
            .icon(AppIcon::Plus)
            .label("Add your first connection")
            .on_click(cx.listener(|view, _: &ClickEvent, window, cx| {
                run_command_id(view, "connection:new", window, cx);
            }))
            .into_any_element()
    };

    div()
        .id("welcome")
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(SPACE[6]))
        .bg(bg)
        .text_color(fg)
        .child(title)
        .child(body)
        .into_any_element()
}
