// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The status bar: environment, connection, database, run state, rows, timing and the panel
//! toggles. It is a solid block for production connections.

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, FontWeight, Hsla, InteractiveElement as _,
    IntoElement as _, ParentElement as _, Role, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon};
use sqler_driver_api::sqler_core::Environment;

use sqler_plugin_api::Alignment;

use super::{Hints, appearance, run_command_id, scale};
use crate::plugins::status::Shown;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::env_chip;
use crate::theme::{SPACE, env_accent, env_label, tooltip_text};
use crate::workbench::{RunIndicator, StatusBarModel, status_solid};
use crate::workspace::WorkspaceView;

/// Height of the bar in pixels.
const HEIGHT: f32 = 24.;

/// The colours of the bar.
#[derive(Clone, Copy)]
struct Colors {
    text: Hsla,
    hover: Hsla,
    ring: Hsla,
}

/// An item of the bar: padding, hover background, and a tab stop when it does something.
fn item(id: impl Into<gpui::ElementId>, colors: Colors) -> Stateful<gpui::Div> {
    let hover = colors.hover;
    div()
        .id(id)
        .h_full()
        .px(px(SPACE[3]))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[1]))
        .text_color(colors.text)
        .hover(move |s| s.bg(hover))
}

/// An item a plugin contributed: its text, and the tooltip of the manifest (else the plugin name).
fn plugin_item(shown: Shown, colors: Colors) -> Stateful<gpui::Div> {
    let tip = SharedString::from(shown.tooltip);
    item(
        SharedString::from(format!("status-plugin-{}", shown.key)),
        colors,
    )
    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
    .child(shown.text)
}

/// A clickable item that runs `command`.
fn action_item(
    id: &'static str,
    colors: Colors,
    handle: Option<&FocusHandle>,
    command: &'static str,
    label: impl Into<SharedString>,
    tip: Option<SharedString>,
    cx: &mut Context<WorkspaceView>,
) -> Stateful<gpui::Div> {
    let label: SharedString = label.into();
    let tip = tip.unwrap_or_else(|| label.clone());
    let ring = colors.ring;
    let el = item(id, colors)
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer();
    let el = match handle {
        Some(handle) => el.track_focus(handle),
        None => el.focusable(),
    };
    el.tab_stop(true)
        .focus_visible(move |s| s.border_color(ring))
        .role(Role::Button)
        .aria_label(label)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
            run_command_id(view, command, window, cx);
        }))
}

/// The status bar. `focus` marks the region for `F6`.
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn status_bar(
    view: &WorkspaceView,
    model: &StatusBarModel,
    hints: &Hints,
    focus: &FocusHandle,
    first: &FocusHandle,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let solid = status_solid(model);
    let theme = cx.theme();
    let (mut bg, mut text, border) = (
        theme.status_bar,
        theme.muted_foreground,
        theme.status_bar_border,
    );
    let mut hover = theme.list_hover;
    let mut ring = theme.ring;
    if solid && let Some((fill, on_solid)) = env_accent(Environment::Prod, appearance(cx)).solid {
        bg = rgb(fill.0).into();
        text = rgb(on_solid.0).into();
        hover = Hsla::from(rgb(on_solid.0)).opacity(0.15);
        ring = rgb(on_solid.0).into();
    }
    let colors = Colors { text, hover, ring };
    let size = scale(cx).xs;
    // The first action item of the bar takes the handle `F6` focuses.
    let mut first = Some(first);

    let mut left = div().h_full().flex().flex_row().items_center();
    if let Some(env) = model.env {
        left = left.child(item("status-env", colors).child(if solid {
            div()
                .px(px(SPACE[2]))
                .border_1()
                .border_color(text)
                .rounded(cx.theme().radius)
                .font_weight(FontWeight::MEDIUM)
                .child(env_label(env))
                .into_any_element()
        } else {
            env_chip(env, cx).into_any_element()
        }));
    }
    if let Some(name) = &model.connection {
        // On the solid block the accent would vanish into the fill, so the dot takes the text colour.
        let dot = model.accent.map(|c| {
            let fill: Hsla = if solid { text } else { rgb(c.0).into() };
            div().size(px(6.)).flex_none().rounded_full().bg(fill)
        });
        left = left.child(
            action_item(
                "status-connection",
                colors,
                first.take(),
                "workbench:show-connections",
                "Show connections",
                None,
                cx,
            )
            .children(dot)
            .child(name.clone()),
        );
    }
    if let Some(database) = &model.database {
        left = left.child(
            item("status-database", colors)
                .child(Icon::new(AppIcon::Database).size(px(12.)).text_color(text))
                .child(database.clone()),
        );
    }

    if let Some(status) = &model.ai {
        left = left.child(
            action_item(
                "status-ai",
                colors,
                None,
                "workbench:show-ai",
                status.label(),
                Some(status.tooltip().into()),
                cx,
            )
            .child(Icon::new(AppIcon::Sparkles).size(px(12.)).text_color(text))
            .child(status.label()),
        );
    }

    if model.agent_endpoints > 0 {
        let count = model.agent_endpoints;
        left = left.child(
            action_item(
                "status-mcp",
                colors,
                None,
                "workbench:show-ai",
                crate::ai::endpoint_label(count),
                Some(crate::ai::endpoint_tooltip(count).into()),
                cx,
            )
            .child(Icon::new(AppIcon::Plug).size(px(12.)).text_color(text))
            .child(crate::ai::endpoint_label(count)),
        );
    }

    // Plugin items are plain text: not focusable, the tooltip says whose they are.
    for shown in view.workspace().plugin_ui().status().shown(Alignment::Left) {
        left = left.child(plugin_item(shown, colors));
    }

    let mut right = div().h_full().flex().flex_row().items_center();
    let running = match model.run {
        RunIndicator::Idle => None,
        RunIndicator::Running => Some("Running…"),
        RunIndicator::Cancelling => Some("Cancelling…"),
    };
    if let Some(label) = running {
        right = right.child(
            item("status-run", colors)
                .child(
                    Icon::new(AppIcon::LoaderCircle)
                        .size(px(12.))
                        .text_color(text),
                )
                .child(label),
        );
    }
    if let Some(rows) = &model.rows {
        right = right.child(item("status-rows", colors).child(rows.clone()));
    }
    if let Some(timing) = &model.timing {
        right = right.child(item("status-timing", colors).child(timing.clone()));
    }
    for shown in view
        .workspace()
        .plugin_ui()
        .status()
        .shown(Alignment::Right)
    {
        right = right.child(plugin_item(shown, colors));
    }
    let layout = view.workspace().layout();
    for (id, icon, command, label, open) in [
        (
            "status-toggle-side",
            AppIcon::PanelLeft,
            "workbench:toggle-side-panel",
            "Toggle side panel",
            layout.side_visible(),
        ),
        (
            "status-toggle-panel",
            AppIcon::PanelBottom,
            "workbench:toggle-bottom-panel",
            "Toggle panel",
            layout.panel_visible(),
        ),
    ] {
        let tip = tooltip_text(label, hints.get(command).as_deref());
        right = right.child(
            action_item(
                id,
                colors,
                first.take(),
                command,
                label,
                Some(tip.into()),
                cx,
            )
            // Open: a filled item with the full-strength icon; closed: a faded icon.
            .when(open, |el| el.bg(text.opacity(0.2)))
            .child(Icon::new(icon).size(px(14.)).text_color(if open {
                text
            } else {
                text.opacity(0.55)
            })),
        );
    }

    div()
        .id("status-bar")
        .track_focus(focus)
        .h(px(HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .text_size(px(size))
        .bg(bg)
        .text_color(text)
        .border_t_1()
        .border_color(border)
        .child(left)
        .child(right)
        .into_any_element()
}
