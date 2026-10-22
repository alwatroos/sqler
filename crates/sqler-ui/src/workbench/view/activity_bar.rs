// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The activity bar: one icon per view, AI and settings at the bottom.

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
    rgb,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon};

use super::{Hints, appearance, run_command_id};
use crate::theme::icons::AppIcon;
use crate::theme::{activity_foreground, tooltip_text};
use crate::workbench::ActivityItem;
use crate::workspace::WorkspaceView;

/// Width of the bar in pixels.
const WIDTH: f32 = 44.;
/// Height of one item in pixels.
const ITEM_HEIGHT: f32 = 40.;
/// Diameter of the attention dot, in pixels.
const DOT: f32 = 8.;
/// Distance of the attention dot from the top and right edge of an item, in pixels.
const DOT_INSET: f32 = 8.;

fn icon_of(item: ActivityItem) -> AppIcon {
    match item {
        ActivityItem::Connections => AppIcon::Database,
        ActivityItem::SavedQueries => AppIcon::Bookmark,
        ActivityItem::History => AppIcon::Clock,
        ActivityItem::Plugins => AppIcon::Plug,
        ActivityItem::Ai => AppIcon::Sparkles,
        ActivityItem::Settings => AppIcon::Settings,
    }
}

/// The index in the bar (top items, then the bottom ones) of the item `F6` lands on: the
/// active one, else the first.
pub(crate) fn focus_index(view: &WorkspaceView) -> usize {
    let ws = view.workspace();
    if ws.settings_tab_active() {
        return ActivityItem::COUNT - 1;
    }
    ActivityItem::TOP
        .iter()
        .chain(&ActivityItem::BOTTOM)
        .position(|item| item.is_active(ws.layout()))
        .unwrap_or(0)
}

fn item_element(
    view: &WorkspaceView,
    item: ActivityItem,
    hints: &Hints,
    handle: &FocusHandle,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let ws = view.workspace();
    let active = if item == ActivityItem::Settings {
        ws.settings_tab_active()
    } else {
        item.is_active(ws.layout())
    };
    let command = item.click_command(ws.layout());
    let needs_attention = item == ActivityItem::Plugins && ws.plugins().needs_attention();
    let title = item.title();
    let label = if needs_attention {
        SharedString::from(format!("{title}, needs attention"))
    } else {
        SharedString::from(title)
    };
    let tip = tooltip_text(title, hints.get(item.command()).as_deref());
    let theme = cx.theme();
    let (ring, hover, fg) = (theme.ring, theme.list_hover, theme.sidebar_foreground);
    let attention = theme.warning;
    let idle = rgb(activity_foreground(appearance(cx)).0);

    let mut el = div()
        .id(SharedString::from(format!("activity-{}", item.icon_name())))
        .relative()
        .w(px(WIDTH))
        .h(px(ITEM_HEIGHT))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .track_focus(handle)
        .focus_visible(move |s| s.border_color(ring))
        .role(Role::Button)
        .aria_label(label)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
            run_command_id(view, command, window, cx);
        }))
        .child(
            Icon::new(icon_of(item))
                .size(px(20.))
                .text_color(if active { fg } else { idle.into() }),
        );
    if needs_attention {
        // A plugin waits for a decision or has failed: the dot is the only sign outside the view.
        el = el.child(
            div()
                .absolute()
                .top(px(DOT_INSET))
                .right(px(DOT_INSET))
                .size(px(DOT))
                .rounded_full()
                .bg(attention),
        );
    }
    if active {
        el = el.child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .h_full()
                .w(px(2.))
                .bg(ring),
        );
    }
    el.into_any_element()
}

/// The activity bar. `focus` marks the region for `F6`.
pub(crate) fn activity_bar(
    view: &WorkspaceView,
    hints: &Hints,
    focus: &FocusHandle,
    items: &[FocusHandle],
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (bg, border) = (theme.sidebar, theme.sidebar_border);
    let mut top = div().flex().flex_col();
    for (item, handle) in ActivityItem::TOP.into_iter().zip(items) {
        top = top.child(item_element(view, item, hints, handle, cx));
    }
    let mut bottom = div().flex().flex_col();
    for (item, handle) in ActivityItem::BOTTOM
        .into_iter()
        .zip(items.iter().skip(ActivityItem::TOP.len()))
    {
        bottom = bottom.child(item_element(view, item, hints, handle, cx));
    }
    div()
        .id("activity-bar")
        .track_focus(focus)
        .role(Role::Toolbar)
        .aria_label("Activity bar")
        .w(px(WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .justify_between()
        .bg(bg)
        .border_r_1()
        .border_color(border)
        .child(top)
        .child(bottom)
        .into_any_element()
}
