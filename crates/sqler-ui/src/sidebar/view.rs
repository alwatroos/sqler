// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The sidebar view: renders `SidebarModel` as a tree of groups and connections. Clicks become
//! `WorkspaceAction`s or `ConnectionAction`s; dragging a connection onto a group (or onto the
//! header, the root) moves it.

use gpui::{
    AppContext as _, Context, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};
use gpui_component::ActiveTheme as _;
use sqler_config::{ConnectionId, GroupId};
use sqler_driver_api::sqler_core::Environment;

use super::{ConnectionStatus, RowKind, Selection, SidebarEntry, SidebarModel};
use crate::connections::ConnectionAction;
use crate::tabs::{Rgb, env_badge};
use crate::workspace::{WorkspaceAction, WorkspaceView};

/// Shown when there are no saved connections and no load error.
pub(crate) const EMPTY_TEXT: &str = "No saved connections. Use New connection above, or run connection:new from the command palette.";

/// Indentation per nesting level, in pixels.
const INDENT: f32 = 14.;

/// The indentation of a nesting depth, in pixels.
pub(crate) fn indent(depth: usize) -> f32 {
    INDENT * u16::try_from(depth).map_or(0., f32::from)
}

/// The payload of a drag that started on a connection row.
#[derive(Clone)]
pub(crate) struct DragConnection {
    id: ConnectionId,
    name: String,
}

impl Render for DragConnection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .text_sm()
            .child(self.name.clone())
    }
}

/// The colours a row needs, copied out of the theme.
#[derive(Clone, Copy)]
struct Palette {
    border: Hsla,
    muted: Hsla,
    danger: Hsla,
    selected_bg: Hsla,
}

/// An element id built from a name and a group id.
fn group_id(name: &str, key: &str) -> ElementId {
    ElementId::Name(format!("{name}-{key}").into())
}

/// A small text button that dispatches a connection action.
fn text_button(
    id: impl Into<ElementId>,
    label: &'static str,
    color: Hsla,
    action: ConnectionAction,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .text_xs()
        .px_1()
        .rounded_sm()
        .text_color(color)
        .cursor_pointer()
        .child(label)
        .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
            view.dispatch_connection(action.clone(), cx);
            cx.stop_propagation();
        }))
}

/// Renders the sidebar.
#[allow(clippy::too_many_lines)] // one declarative element tree
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
    let palette = Palette {
        border,
        muted,
        danger,
        selected_bg,
    };
    let new_group_parent = model.selected_group().cloned();

    // The header is the drop target for "move to the root".
    let header = div()
        .id("sidebar-header")
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_1()
        .rounded_sm()
        .drag_over::<DragConnection>(move |style, _, _, _| style.bg(selected_bg))
        .on_drop(cx.listener(|view, drag: &DragConnection, _, cx| {
            view.dispatch_connection(
                ConnectionAction::MoveConnection {
                    id: drag.id.clone(),
                    to: None,
                    index: usize::MAX,
                },
                cx,
            );
        }))
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(muted)
                .child("CONNECTIONS"),
        )
        .child(text_button(
            "new-connection",
            "New connection",
            fg,
            ConnectionAction::New,
            cx,
        ))
        .child(text_button(
            "new-group",
            "New group",
            fg,
            ConnectionAction::NewGroup {
                parent: new_group_parent,
            },
            cx,
        ));

    let mut list = div()
        .id("sidebar")
        .w(px(260.))
        .h_full()
        .flex()
        .flex_col()
        .flex_none()
        .gap_1()
        .p_2()
        .overflow_y_scroll()
        .bg(bg)
        .text_color(fg)
        .border_r_1()
        .border_color(border)
        .child(header);

    if let Some(error) = model.load_error() {
        list = list.child(
            div()
                .text_sm()
                .text_color(danger)
                .child(format!("Could not load connections: {error}")),
        );
        list = list.child(div().text_xs().text_color(muted).child(
            "Changes are not saved while the file cannot be loaded. Fix connections.toml and restart.",
        ));
    } else if model.entries().is_empty() && model.store().groups().is_empty() {
        list = list.child(div().text_sm().text_color(muted).child(EMPTY_TEXT));
    }
    if let Some(error) = model.save_error() {
        list = list.child(
            div()
                .text_xs()
                .text_color(danger)
                .child(format!("Could not save connections: {error}")),
        );
    }

    for row in model.rows() {
        let depth = row.depth;
        match row.kind {
            RowKind::Group {
                id,
                name,
                expanded,
                empty,
            } => {
                let selected = model.selection() == Some(&Selection::Group(id.clone()));
                list = list.child(group_row(
                    &id, &name, depth, expanded, empty, selected, palette, cx,
                ));
            }
            RowKind::Connection { index } => {
                let entry = &model.entries()[index];
                let selected = model.selected() == Some(index);
                list = list.child(connection_row(index, depth, entry, selected, palette, cx));
            }
        }
    }
    list
}

#[allow(clippy::too_many_arguments)] // theme colours are bundled in `Palette`; the rest is row data
fn group_row(
    id: &GroupId,
    name: &str,
    depth: usize,
    expanded: bool,
    empty: bool,
    selected: bool,
    palette: Palette,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let Palette {
        muted,
        danger,
        selected_bg,
        ..
    } = palette;
    let key = id.to_string();
    let click_id = id.clone();
    let toggle_id = id.clone();
    let drop_id = id.clone();
    let mut row = div()
        .id(group_id("group", &key))
        .flex()
        .flex_col()
        .gap_0p5()
        .pl(px(8. + indent(depth)))
        .pr_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .drag_over::<DragConnection>(move |style, _, _, _| style.bg(selected_bg))
        .on_drop(cx.listener(move |view, drag: &DragConnection, _, cx| {
            view.dispatch_connection(
                ConnectionAction::MoveConnection {
                    id: drag.id.clone(),
                    to: Some(drop_id.clone()),
                    index: usize::MAX,
                },
                cx,
            );
        }))
        .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
            view.dispatch_connection(ConnectionAction::SelectGroup(click_id.clone()), cx);
        }));
    if selected {
        row = row.bg(selected_bg);
    }
    let chevron = div()
        .id(group_id("group-toggle", &key))
        .w(px(14.))
        .text_color(muted)
        .cursor_pointer()
        .child(if expanded { "▾" } else { "▸" })
        .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
            view.dispatch_connection(ConnectionAction::ToggleGroup(toggle_id.clone()), cx);
            cx.stop_propagation();
        }));
    let mut title = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(chevron)
        .child(div().flex_1().truncate().child(name.to_owned()));
    if empty {
        title = title.child(div().text_xs().text_color(muted).child("empty"));
    }
    row = row.child(title);
    if selected {
        row = row.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(text_button(
                    group_id("group-new", &key),
                    "New subgroup",
                    muted,
                    ConnectionAction::NewGroup {
                        parent: Some(id.clone()),
                    },
                    cx,
                ))
                .child(text_button(
                    group_id("group-rename", &key),
                    "Rename",
                    muted,
                    ConnectionAction::RenameGroup(id.clone()),
                    cx,
                ))
                .child(text_button(
                    group_id("group-delete", &key),
                    "Delete",
                    danger,
                    ConnectionAction::DeleteGroup(id.clone()),
                    cx,
                )),
        );
    }
    row
}

#[allow(clippy::too_many_lines)] // one declarative element tree
fn connection_row(
    index: usize,
    depth: usize,
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
    let is_prod = config.environment == Some(Environment::Prod);
    let dot = config
        .color
        .as_deref()
        .and_then(Rgb::parse)
        .map_or(border, |Rgb(c)| rgb(c).into());
    let drag = DragConnection {
        id: entry.id().clone(),
        name: config.name.clone(),
    };
    let mut row = div()
        .id(("connection", index))
        .flex()
        .flex_col()
        .gap_0p5()
        .ml(px(indent(depth)))
        .px_2()
        .py_1()
        .rounded_md()
        .border_l_2()
        // Prod rows keep a danger-coloured edge and tint so they stand out in a long list.
        .border_color(if is_prod { danger } else { border.opacity(0.) })
        .cursor_pointer()
        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_click(cx.listener(move |view, event: &gpui::ClickEvent, _, cx| {
            let action = if event.click_count() >= 2 {
                WorkspaceAction::OpenConnection(index)
            } else {
                WorkspaceAction::SelectConnection(index)
            };
            view.dispatch(action, cx);
        }));
    if is_prod {
        row = row.bg(danger.opacity(0.12));
    }
    if selected {
        row = row.bg(if is_prod {
            danger.opacity(0.28)
        } else {
            selected_bg
        });
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
                .text_color(if is_prod { danger } else { muted })
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
        let id = entry.id().clone();
        row = row.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(
                    div()
                        .id(("open-connection", index))
                        .text_xs()
                        .px_1()
                        .cursor_pointer()
                        .child("Open")
                        .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                            view.dispatch(WorkspaceAction::OpenConnection(index), cx);
                            cx.stop_propagation();
                        })),
                )
                .child(text_button(
                    ("edit-connection", index),
                    "Edit",
                    muted,
                    ConnectionAction::Edit(id.clone()),
                    cx,
                ))
                .child(text_button(
                    ("move-connection", index),
                    "Move",
                    muted,
                    ConnectionAction::OpenMoveTo(id.clone()),
                    cx,
                ))
                .child(text_button(
                    ("delete-connection", index),
                    "Delete",
                    danger,
                    ConnectionAction::Delete(id),
                    cx,
                )),
        );
    }
    row
}
