// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The bottom panel frame (tabs, close button) and the Messages view.

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement as _, ParentElement as _, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme as _, Icon};

use super::{run_command_id, scale};
use crate::tabs::TabId;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, icon_button};
use crate::workbench::{PanelView, message_lines};
use crate::workspace::WorkspaceView;

/// Height of the panel header in pixels.
const HEADER_HEIGHT: f32 = 30.;

/// The tabs of the header: view, label and the command that shows it.
const TABS: [(PanelView, &str, &str); 3] = [
    (PanelView::Results, "Results", "workbench:show-results"),
    (PanelView::Messages, "Messages", "workbench:show-messages"),
    (PanelView::History, "History", "history:open"),
];

/// The panel: a header with the view tabs and a close button above `body`. `focus` marks the
/// region for `F6`.
pub(crate) fn bottom_panel(
    view: &WorkspaceView,
    body: AnyElement,
    focus: &FocusHandle,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let current = view.workspace().layout().panel_view();
    let theme = cx.theme();
    let (bg, fg, muted, border, ring, hover) = (
        theme.background,
        theme.foreground,
        theme.muted_foreground,
        theme.border,
        theme.ring,
        theme.list_hover,
    );
    let text = scale(cx).sm;

    let mut tabs = div()
        .id("panel-tabs")
        .flex()
        .flex_row()
        .h_full()
        .role(Role::TabList);
    for (panel, label, command) in TABS {
        let active = panel == current;
        // The tab itself carries the focus ring; the underline of the active tab is its child.
        let underline = div()
            .h_full()
            .px(px(SPACE[3]))
            .flex()
            .items_center()
            .text_size(px(text))
            .text_color(if active { fg } else { muted })
            .border_b_2()
            .border_color(if active {
                ring
            } else {
                gpui::transparent_black()
            })
            .when(active, |el| el.font_weight(FontWeight::MEDIUM))
            .child(label);
        let tab = div()
            .id(SharedString::from(format!("panel-tab-{label}")))
            .h_full()
            .flex()
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .focusable()
            .tab_stop(true)
            .focus_visible(move |s| s.border_color(ring))
            .role(Role::Tab)
            .aria_label(label)
            .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                run_command_id(view, command, window, cx);
            }))
            .child(underline);
        tabs = tabs.child(tab);
    }

    let header = div()
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .pr(px(SPACE[2]))
        .border_b_1()
        .border_color(border)
        .child(tabs)
        .child(
            icon_button("panel-close", AppIcon::X, "Hide panel").on_click(cx.listener(
                |view, _: &ClickEvent, window, cx| {
                    run_command_id(view, "workbench:toggle-bottom-panel", window, cx);
                },
            )),
        );

    div()
        .id("bottom-panel")
        .track_focus(focus)
        .size_full()
        .flex()
        .flex_col()
        .bg(bg)
        .text_color(fg)
        .border_t_1()
        .border_color(border)
        .child(header)
        .child(div().flex_1().min_h_0().flex().flex_col().child(body))
        .into_any_element()
}

/// The Messages view: one row per statement of the last run of `tab`.
pub(crate) fn messages_body(
    view: &WorkspaceView,
    tab: Option<TabId>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let lines = tab
        .and_then(|t| view.workspace().query_tab(t))
        .map(message_lines)
        .unwrap_or_default();
    if lines.is_empty() {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_state(
                AppIcon::MessageSquare,
                "No messages yet",
                "Run a query to see one line per statement here.",
                cx,
            ))
            .into_any_element();
    }
    let theme = cx.theme();
    let (fg, muted, danger) = (theme.foreground, theme.muted_foreground, theme.danger);
    let text = scale(cx).sm;
    let mut list = div()
        .id("messages")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .py(px(SPACE[1]));
    for (i, line) in lines.into_iter().enumerate() {
        let color = if line.failed { danger } else { fg };
        let icon = if line.failed {
            Icon::new(AppIcon::CircleAlert).text_color(danger)
        } else {
            Icon::new(AppIcon::Check).text_color(muted)
        };
        let mut row = div()
            .id(("message", i))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE[3]))
            .px(px(SPACE[4]))
            .py(px(SPACE[0]))
            .text_size(px(text))
            .text_color(color)
            .child(icon.size(px(14.)))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(line.title.clone()),
            )
            .child(div().flex_1().child(line.status.clone()));
        if let Some(duration) = line.duration {
            row = row.child(div().text_color(muted).child(duration));
        }
        list = list.child(row);
    }
    list.into_any_element()
}
