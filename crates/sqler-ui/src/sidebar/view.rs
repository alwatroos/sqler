// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The sidebar view: renders `SidebarModel` as a tree of groups and connections. Clicks become
//! `WorkspaceAction`s or `ConnectionAction`s; dragging a connection onto a group (or onto the
//! header, the root) moves it.

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, ElementId, Hsla, InteractiveElement as _,
    IntoElement, ListAlignment, ListState, MouseButton, MouseDownEvent, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, list, px, rgb,
};
use gpui_component::ActiveTheme as _;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use sqler_config::{ConnectionId, GroupId};
use sqler_driver_api::sqler_core::Environment;

use super::{
    ConnectionStatus, RowKind, Selection, SidebarEntry, SidebarItem, SidebarRow, changed_range,
};
use crate::browser::{BrowserAction, LoadState, MenuEntry, TreeNodeId};
use crate::connections::ConnectionAction;
use crate::tabs::{Rgb, env_badge};
use crate::workspace::{BrowserNote, Workspace, WorkspaceAction, WorkspaceView};

/// Pixels rendered above and below the visible part of the list.
const OVERDRAW: f32 = 240.;

/// One line of the list with what decides its height, so a change remeasures only that line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListEntry {
    item: SidebarItem,
    /// The indentation level of an object row's top level: the connection's depth plus one.
    depth: usize,
    selected: bool,
    note: Option<String>,
}

/// The list state to start with.
pub(crate) fn new_list_state() -> ListState {
    ListState::new(0, ListAlignment::Top, px(OVERDRAW))
}

/// The lines of the list for the workspace as it is now.
pub(crate) fn build_entries(ws: &Workspace) -> Vec<ListEntry> {
    let model = ws.sidebar();
    let mut base = 0;
    ws.sidebar_items()
        .into_iter()
        .map(|item| match &item {
            SidebarItem::Row(row) => {
                let depth = row.depth;
                base = depth + 1;
                let (selected, note) = match &row.kind {
                    RowKind::Group { id, .. } => (
                        model.selection() == Some(&Selection::Group(id.clone())),
                        None,
                    ),
                    RowKind::Connection { index } => {
                        let entry = &model.entries()[*index];
                        let browser = ws.browser_note(entry.id());
                        (
                            model.selected() == Some(*index),
                            Some(format!("{:?}/{browser:?}", entry.status())),
                        )
                    }
                };
                ListEntry {
                    item,
                    depth,
                    selected,
                    note,
                }
            }
            SidebarItem::Object { .. } => ListEntry {
                item,
                depth: base,
                selected: false,
                note: None,
            },
        })
        .collect()
}

/// Replaces `entries` by `new` and tells the list which lines changed, keeping the scroll
/// position and the measured heights of the rest.
pub(crate) fn sync_list(entries: &mut Vec<ListEntry>, state: &ListState, new: Vec<ListEntry>) {
    let (range, count) = changed_range(entries, &new);
    if !range.is_empty() || count > 0 {
        state.splice(range, count);
    }
    *entries = new;
}

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

/// The colours of the current theme, as the rows need them.
fn palette_of(cx: &Context<WorkspaceView>) -> Palette {
    let theme = cx.theme();
    Palette {
        border: theme.sidebar_border,
        muted: theme.muted_foreground,
        danger: theme.danger,
        selected_bg: theme.sidebar_accent,
    }
}

/// Renders the sidebar: a fixed header and the virtualized list of groups, connections and
/// object rows (only the rows in view are built).
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn sidebar(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let model = view.workspace().sidebar();
    let theme = cx.theme();
    let (bg, fg, border) = (
        theme.sidebar,
        theme.sidebar_foreground,
        theme.sidebar_border,
    );
    let Palette {
        muted,
        danger,
        selected_bg,
        ..
    } = palette_of(cx);
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

    let mut top = div()
        .id("sidebar-top")
        .flex()
        .flex_col()
        .flex_none()
        .gap_1()
        .p_2()
        .child(header);

    if let Some(error) = model.load_error() {
        top = top.child(
            div()
                .text_sm()
                .text_color(danger)
                .child(format!("Could not load connections: {error}")),
        );
        top = top.child(div().text_xs().text_color(muted).child(
            "Changes are not saved while the file cannot be loaded. Fix connections.toml and restart.",
        ));
    } else if model.entries().is_empty() && model.store().groups().is_empty() {
        top = top.child(div().text_sm().text_color(muted).child(EMPTY_TEXT));
    }
    if let Some(error) = model.save_error() {
        top = top.child(
            div()
                .text_xs()
                .text_color(danger)
                .child(format!("Could not save connections: {error}")),
        );
    }

    let rows = list(
        view.sidebar_list().clone(),
        cx.processor(|view, ix: usize, _window, cx| item(view, ix, cx)),
    )
    .size_full();

    div()
        .id("sidebar")
        .track_focus(view.sidebar_focus())
        .w(px(260.))
        .h_full()
        .flex()
        .flex_col()
        .flex_none()
        .bg(bg)
        .text_color(fg)
        .border_r_1()
        .border_color(border)
        .child(top)
        .child(div().flex_1().min_h_0().px_2().pb_2().child(rows))
        .child(crate::history::view::saved_section(view, cx))
}

/// Builds the element of one line of the list.
fn item(view: &WorkspaceView, ix: usize, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let Some(entry) = view.list_entries().get(ix) else {
        return div().into_any_element();
    };
    let palette = palette_of(cx);
    let ws = view.workspace();
    let model = ws.sidebar();
    match &entry.item {
        SidebarItem::Row(SidebarRow { depth, kind }) => match kind {
            RowKind::Group {
                id,
                name,
                expanded,
                empty,
            } => group_row(
                id,
                name,
                *depth,
                *expanded,
                *empty,
                entry.selected,
                palette,
                cx,
            )
            .into_any_element(),
            RowKind::Connection { index } => {
                let sidebar_entry = &model.entries()[*index];
                let conn = sidebar_entry.id();
                let browser = ws.browser(conn).map(|tree| ConnectionBrowser {
                    conn: conn.clone(),
                    root: tree.root(),
                    expanded: tree.is_expanded(tree.root()),
                    note: ws.browser_note(conn),
                    menu: ws.menu_entries(conn, tree.root()),
                    // The connection row is the highlighted one while no object is selected.
                    root_selected: tree.selected().is_none_or(|n| n == tree.root()),
                });
                let selected = entry.selected && browser.as_ref().is_none_or(|b| b.root_selected);
                connection_row(
                    *index,
                    *depth,
                    sidebar_entry,
                    selected,
                    palette,
                    browser,
                    cx,
                )
            }
        },
        SidebarItem::Object { conn, row_index } => {
            object_row(view, conn, *row_index, entry.depth, palette, cx)
        }
    }
}

/// What a connection row needs from its object browser.
struct ConnectionBrowser {
    conn: ConnectionId,
    root: TreeNodeId,
    expanded: bool,
    note: Option<BrowserNote>,
    menu: Vec<MenuEntry>,
    root_selected: bool,
}

/// A context menu with the given entries; each runs its command on `node` of `conn`.
fn with_menu(
    row: gpui::Stateful<gpui::Div>,
    conn: &ConnectionId,
    node: TreeNodeId,
    entries: Vec<MenuEntry>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    if entries.is_empty() {
        return row.into_any_element();
    }
    let weak = cx.weak_entity();
    let conn = conn.clone();
    row.context_menu(move |mut menu, _, _| {
        for entry in &entries {
            let (weak, conn, entry) = (weak.clone(), conn.clone(), *entry);
            menu = menu.item(PopupMenuItem::new(entry.label()).on_click(move |_, _, cx| {
                weak.update(cx, |view, cx| {
                    view.dispatch_object(&conn, node, BrowserAction::Entry(entry), cx);
                })
                .ok();
            }));
        }
        menu
    })
    .into_any_element()
}

#[allow(clippy::too_many_lines)] // one declarative element tree
fn object_row(
    view: &WorkspaceView,
    conn: &ConnectionId,
    row_index: usize,
    base: usize,
    palette: Palette,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let Palette {
        muted,
        danger,
        selected_bg,
        ..
    } = palette;
    let ws = view.workspace();
    let Some(tree) = ws.browser(conn) else {
        return div().into_any_element();
    };
    let Some(row) = tree.rows().get(row_index) else {
        return div().into_any_element();
    };
    let node = row.id;
    let selected = ws.sidebar().selected_id() == Some(conn) && tree.selected() == Some(node);
    let key = format!("object-{conn}-{node:?}");
    let click_conn = conn.clone();
    let toggle_conn = conn.clone();
    let press_conn = conn.clone();
    let mut line = div()
        .id(ElementId::Name(key.clone().into()))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h(px(24.))
        .pl(px(8. + indent(base + row.depth)))
        .pr_2()
        .rounded_md()
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |view, _: &MouseDownEvent, window, cx| {
                view.select_object(&press_conn, node, window, cx);
            }),
        )
        .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
            view.click_object(&click_conn, node, event.click_count() >= 2, window, cx);
        }));
    if selected {
        line = line.bg(selected_bg);
    }
    let chevron = div()
        .id(ElementId::Name(format!("{key}-toggle").into()))
        .w(px(14.))
        .flex_none()
        .text_color(muted)
        .child(match (row.expandable, row.expanded) {
            (false, _) => "",
            (true, true) => "▾",
            (true, false) => "▸",
        });
    let chevron = if row.expandable {
        chevron
            .cursor_pointer()
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.dispatch_object(&toggle_conn, node, BrowserAction::Toggle, cx);
                cx.stop_propagation();
            }))
    } else {
        chevron
    };
    line = line
        .child(chevron)
        .child(div().flex_1().min_w_0().truncate().child(row.label.clone()));
    if let Some(kind) = row.kind {
        line = line.child(div().text_xs().text_color(muted).child(kind.as_str()));
    }
    match &row.state {
        LoadState::Loading => {
            line = line.child(div().text_xs().text_color(muted).child("loading…"));
        }
        LoadState::Failed(message) => {
            line = line.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .max_w(px(110.))
                    .truncate()
                    .child(message.clone()),
            );
        }
        LoadState::NotLoaded | LoadState::Loaded => {}
    }
    if let Some((done, total)) = row.progress {
        let cancel_conn = conn.clone();
        line = line
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("{done}/{total}")),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("{key}-cancel").into()))
                    .text_xs()
                    .text_color(danger)
                    .cursor_pointer()
                    .child("cancel")
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        view.dispatch_object(&cancel_conn, node, BrowserAction::CancelRefresh, cx);
                        cx.stop_propagation();
                    })),
            );
    }
    with_menu(line, conn, node, ws.menu_entries(conn, node), cx)
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
    browser: Option<ConnectionBrowser>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
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
        .on_click(
            cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                view.focus_sidebar(window, cx);
                let action = if event.click_count() >= 2 {
                    WorkspaceAction::OpenConnection(index)
                } else {
                    WorkspaceAction::SelectConnection(index)
                };
                view.dispatch(action, cx);
            }),
        );
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
    let mut title = div().flex().flex_row().items_center().gap_2();
    if let Some(browser) = &browser {
        let (conn, root) = (browser.conn.clone(), browser.root);
        title = title.child(
            div()
                .id(("connection-toggle", index))
                .w(px(14.))
                .flex_none()
                .text_color(muted)
                .cursor_pointer()
                .child(if browser.expanded { "▾" } else { "▸" })
                .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                    view.dispatch_object(&conn, root, BrowserAction::Toggle, cx);
                    cx.stop_propagation();
                })),
        );
    }
    title = title
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
    match browser.as_ref().and_then(|b| b.note.as_ref()) {
        Some(BrowserNote::Busy(text)) => {
            row = row.child(div().text_xs().text_color(muted).child(*text));
        }
        Some(BrowserNote::Failed(message)) => {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(format!("Browser: {message}")),
            );
        }
        None => {}
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
    match browser {
        Some(b) => {
            let conn = b.conn.clone();
            with_menu(row, &conn, b.root, b.menu, cx)
        }
        None => row.into_any_element(),
    }
}
