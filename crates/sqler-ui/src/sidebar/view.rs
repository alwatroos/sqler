// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The sidebar view: renders `SidebarModel` as a tree of groups and connections. Clicks become
//! `WorkspaceAction`s or `ConnectionAction`s; dragging a connection onto a group (or onto the
//! header, the root) moves it.

use gpui::{
    AnyElement, AnyView, App, AppContext as _, ClickEvent, Context, Div, ElementId, Hsla,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, list, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _};
use sqler_config::{ConnectionId, GroupId};
use sqler_driver_api::sqler_core::{Environment, ObjectKind};

use super::{
    ConnectionStatus, RowKind, Selection, SidebarEntry, SidebarItem, SidebarRow, changed_range,
};
use crate::browser::{BrowserAction, LoadState, MenuEntry, TreeNodeId, TreeNodeKind};
use crate::connections::ConnectionAction;
use crate::tabs::Rgb;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, env_chip, icon_button, section_header};
use crate::theme::{Appearance, SPACE, env_accent, tooltip_text};
use crate::workbench::view::run_command_id;
use crate::workspace::chrome::{ICON_SM, INDENT, RowState, hinted_item, list_row, ui_scale};
use crate::workspace::{BrowserNote, Workspace, WorkspaceAction, WorkspaceView};

/// Width of the failure message on an object row.
const FAIL_W: f32 = 110.;
/// Diameter of the colour dot of a connection.
const DOT: f32 = 8.;
/// Opacity of the tint behind a production connection that is not selected.
const PROD_TINT: f32 = 0.08;

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
    muted: Hsla,
    danger: Hsla,
    selected_bg: Hsla,
}

/// An element id built from a name and a group id.
fn group_id(name: &str, key: &str) -> ElementId {
    ElementId::Name(format!("{name}-{key}").into())
}

/// The colours of the current theme, as the rows need them.
fn palette_of(cx: &Context<WorkspaceView>) -> Palette {
    let theme = cx.theme();
    Palette {
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
    // The header is the drop target for "move to the root".
    let header = section_header("CONNECTIONS", cx)
        .id("sidebar-header")
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
                .flex()
                .flex_row()
                .gap_1()
                .child(
                    icon_button("new-connection", AppIcon::Plus, "New connection").on_click(
                        cx.listener(|view, _: &ClickEvent, window, cx| {
                            run_command_id(view, "connection:new", window, cx);
                        }),
                    ),
                )
                .child(
                    icon_button("new-group", AppIcon::Folder, "New group").on_click(cx.listener(
                        |view, _: &ClickEvent, window, cx| {
                            run_command_id(view, "connection-group:new", window, cx);
                        },
                    )),
                ),
        );

    let mut notes = div().flex().flex_col().flex_none().gap_1().px_2();

    if let Some(error) = model.load_error() {
        notes = notes.child(
            div()
                .text_sm()
                .text_color(danger)
                .child(format!("Could not load connections: {error}")),
        );
        notes = notes.child(div().text_xs().text_color(muted).child(
            "Changes are not saved while the file cannot be loaded. Fix connections.toml and restart.",
        ));
    } else if model.entries().is_empty() && model.store().groups().is_empty() {
        notes = notes.child(connections_empty(view, cx));
    }
    if let Some(error) = model.save_error() {
        notes = notes.child(
            div()
                .text_xs()
                .text_color(danger)
                .child(format!("Could not save connections: {error}")),
        );
    }

    let top = div()
        .id("sidebar-top")
        .flex()
        .flex_col()
        .flex_none()
        .gap_1()
        .child(header)
        .child(notes);

    let rows = list(
        view.sidebar_list().clone(),
        cx.processor(|view, ix: usize, window, cx| item(view, ix, window, cx)),
    )
    .size_full();

    div()
        .id("sidebar")
        .track_focus(view.sidebar_focus())
        .w_full()
        .h_full()
        .flex()
        .flex_col()
        .bg(bg)
        .text_color(fg)
        .border_r_1()
        .border_color(border)
        .child(top)
        .child(div().flex_1().min_h_0().px_2().pb_2().child(rows))
}

/// The icon of a tree node, 14 px, muted.
fn node_icon(kind: &TreeNodeKind, expanded: bool) -> AppIcon {
    let folder = if expanded {
        AppIcon::FolderOpen
    } else {
        AppIcon::Folder
    };
    match kind {
        TreeNodeKind::Root => AppIcon::Server,
        TreeNodeKind::Folder(_) | TreeNodeKind::Columns(_) => folder,
        TreeNodeKind::Column { .. } => AppIcon::Columns2,
        TreeNodeKind::Object(object) => match object.kind {
            ObjectKind::Database => AppIcon::Database,
            ObjectKind::Schema => AppIcon::ListTree,
            ObjectKind::Table => AppIcon::Table,
            ObjectKind::View => AppIcon::Eye,
            ObjectKind::MaterializedView => AppIcon::Table2,
            ObjectKind::Function | ObjectKind::Procedure => AppIcon::SquareFunction,
            ObjectKind::Trigger => AppIcon::SquarePlay,
            ObjectKind::Sequence => AppIcon::FileText,
            ObjectKind::Type => AppIcon::FileCode,
            ObjectKind::Extension => AppIcon::Plug,
            // `Index` and `Role` share the key icon; so does any future kind.
            _ => AppIcon::KeyRound,
        },
    }
}

/// The icon of a menu entry; destructive `Drop` has none.
fn entry_icon(entry: MenuEntry) -> Option<AppIcon> {
    match entry {
        MenuEntry::OpenData => Some(AppIcon::Table),
        MenuEntry::OpenInEditor => Some(AppIcon::FileCode),
        MenuEntry::GenerateCreate
        | MenuEntry::GenerateSelect
        | MenuEntry::GenerateInsert
        | MenuEntry::GenerateDrop => Some(AppIcon::FileText),
        MenuEntry::CopyName => Some(AppIcon::Copy),
        MenuEntry::Refresh => Some(AppIcon::RefreshCw),
        MenuEntry::Properties => Some(AppIcon::SlidersHorizontal),
        _ => None,
    }
}

/// A muted icon of the row size.
fn row_icon(icon: AppIcon, color: Hsla) -> Icon {
    Icon::new(icon).size(px(ICON_SM)).text_color(color)
}

/// The animated "running" indicator, sized like a row icon.
fn running_spinner(color: Hsla) -> Spinner {
    Spinner::new()
        .icon(AppIcon::LoaderCircle)
        .color(color)
        .xsmall()
}

/// The space where a chevron would be, for rows that cannot expand.
fn chevron_spacer() -> Div {
    div().w(px(ICON_SM)).flex_none()
}

/// The text size for `xs` secondary lines.
fn xs(cx: &App) -> Pixels {
    px(ui_scale(cx).xs)
}

/// A tooltip with the given text.
fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// The left padding of a row at `depth`.
fn row_pad(depth: usize) -> Pixels {
    px(SPACE[3] + indent(depth))
}

/// A small ghost button that dispatches a connection action from the action line of a row.
fn action_button(
    id: impl Into<ElementId>,
    label: &'static str,
    action: ConnectionAction,
    cx: &mut Context<WorkspaceView>,
) -> Button {
    // Destructive actions stay neutral at rest; the confirm dialog carries the danger colour.
    Button::new(id)
        .ghost()
        .xsmall()
        .label(label)
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_connection(action.clone(), cx);
            cx.stop_propagation();
        }))
}

/// Builds the element of one line of the list.
fn item(
    view: &WorkspaceView,
    ix: usize,
    window: &mut Window,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let Some(entry) = view.list_entries().get(ix) else {
        return div().into_any_element();
    };
    let focused = view.sidebar_focus().contains_focused(window, cx);
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
                RowState {
                    selected: entry.selected,
                    focused,
                    disabled: false,
                },
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
                    RowState {
                        selected,
                        focused,
                        disabled: false,
                    },
                    browser,
                    cx,
                )
            }
        },
        SidebarItem::Object { conn, row_index } => {
            object_row(view, conn, *row_index, entry.depth, focused, cx)
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

/// A context menu with the given entries; each runs its command on `node` of `conn`. The
/// shortcut hints are read when the menu opens.
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
    row.context_menu(move |mut menu, _, cx| {
        for entry in &entries {
            let (weak, conn, entry) = (weak.clone(), conn.clone(), *entry);
            let hint = weak.upgrade().and_then(|view| {
                view.read(cx)
                    .shortcut_hint(&BrowserAction::Entry(entry).command_id())
            });
            if entry.starts_group() {
                menu = menu.separator();
            }
            menu = menu.item(
                hinted_item(entry.label(), entry_icon(entry), hint).on_click(move |_, _, cx| {
                    weak.update(cx, |view, cx| {
                        view.dispatch_object(&conn, node, BrowserAction::Entry(entry), cx);
                    })
                    .ok();
                }),
            );
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
    focused: bool,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
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
    let state = RowState {
        selected,
        focused,
        disabled: false,
    };
    let mut line = list_row(ElementId::Name(key.clone().into()), state, cx)
        .w_full()
        .pl(row_pad(base + row.depth))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |view, _: &MouseDownEvent, window, cx| {
                view.select_object(&press_conn, node, window, cx);
            }),
        )
        .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
            view.click_object(&click_conn, node, event.click_count() >= 2, window, cx);
        }));
    let chevron = if row.expandable {
        let icon = if row.expanded {
            AppIcon::ChevronDown
        } else {
            AppIcon::ChevronRight
        };
        chevron_spacer()
            .id(ElementId::Name(format!("{key}-toggle").into()))
            .cursor_pointer()
            .child(row_icon(icon, muted))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.dispatch_object(&toggle_conn, node, BrowserAction::Toggle, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
    } else {
        chevron_spacer().into_any_element()
    };
    if let Some(kind) = tree.kind(node) {
        line = line
            .child(chevron)
            .child(row_icon(node_icon(kind, row.expanded), muted));
    } else {
        line = line.child(chevron);
    }
    line = line.child(div().flex_1().min_w_0().truncate().child(row.label.clone()));
    if let Some(kind) = row.kind {
        line = line.tooltip(tip(format!("{} ({})", row.label, kind.as_str())));
    }
    match &row.state {
        LoadState::Loading => {
            line = line.child(
                div()
                    .id(ElementId::Name(format!("{key}-loading").into()))
                    .child(running_spinner(muted))
                    .tooltip(tip("Loading…")),
            );
        }
        LoadState::Failed(message) => {
            line = line.child(row_icon(AppIcon::CircleAlert, danger)).child(
                div()
                    .text_size(xs(cx))
                    .text_color(danger)
                    .max_w(px(FAIL_W))
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
                    .text_size(xs(cx))
                    .text_color(muted)
                    .child(format!("{done}/{total}")),
            )
            .child(
                icon_button(
                    ElementId::Name(format!("{key}-cancel").into()),
                    AppIcon::X,
                    "Cancel Refresh",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.dispatch_object(&cancel_conn, node, BrowserAction::CancelRefresh, cx);
                    cx.stop_propagation();
                })),
            );
    }
    with_menu(line, conn, node, ws.menu_entries(conn, node), cx)
}

fn group_row(
    id: &GroupId,
    name: &str,
    depth: usize,
    expanded: bool,
    empty: bool,
    state: RowState,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (muted, drag_bg) = (theme.muted_foreground, theme.list_hover);
    let key = id.to_string();
    let click_id = id.clone();
    let toggle_id = id.clone();
    let drop_id = id.clone();
    let mut container = div()
        .id(group_id("group", &key))
        .w_full()
        .flex()
        .flex_col()
        .drag_over::<DragConnection>(move |style, _, _, _| style.bg(drag_bg))
        .on_drop(cx.listener(move |view, drag: &DragConnection, _, cx| {
            view.dispatch_connection(
                ConnectionAction::MoveConnection {
                    id: drag.id.clone(),
                    to: Some(drop_id.clone()),
                    index: usize::MAX,
                },
                cx,
            );
        }));
    let chevron = chevron_spacer()
        .id(group_id("group-toggle", &key))
        .cursor_pointer()
        .child(row_icon(
            if expanded {
                AppIcon::ChevronDown
            } else {
                AppIcon::ChevronRight
            },
            muted,
        ))
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_connection(ConnectionAction::ToggleGroup(toggle_id.clone()), cx);
            cx.stop_propagation();
        }));
    let mut line = list_row(group_id("group-line", &key), state, cx)
        .pl(row_pad(depth))
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_connection(ConnectionAction::SelectGroup(click_id.clone()), cx);
        }))
        .child(chevron)
        .child(row_icon(node_group_icon(expanded), muted))
        .child(div().flex_1().min_w_0().truncate().child(name.to_owned()));
    if empty {
        line = line.child(div().text_size(xs(cx)).text_color(muted).child("empty"));
    }
    container = container.child(line);
    if state.selected {
        container = container.child(
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE[1]))
                .pl(row_pad(depth + 1))
                .child(action_button(
                    group_id("group-new", &key),
                    "New subgroup",
                    ConnectionAction::NewGroup {
                        parent: Some(id.clone()),
                    },
                    cx,
                ))
                .child(action_button(
                    group_id("group-rename", &key),
                    "Rename",
                    ConnectionAction::RenameGroup(id.clone()),
                    cx,
                ))
                .child(action_button(
                    group_id("group-delete", &key),
                    "Delete",
                    ConnectionAction::DeleteGroup(id.clone()),
                    cx,
                )),
        );
    }
    container
}

/// The icon of a connection group.
fn node_group_icon(expanded: bool) -> AppIcon {
    if expanded {
        AppIcon::FolderOpen
    } else {
        AppIcon::Folder
    }
}

#[allow(clippy::too_many_lines)] // one declarative element tree
fn connection_row(
    index: usize,
    depth: usize,
    entry: &SidebarEntry,
    state: RowState,
    browser: Option<ConnectionBrowser>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let appearance = if theme.mode.is_dark() {
        Appearance::Dark
    } else {
        Appearance::Light
    };
    let config = entry.config();
    let is_prod = config.environment == Some(Environment::Prod);
    let dot = config
        .color
        .as_deref()
        .and_then(Rgb::parse)
        .map(|Rgb(c)| Hsla::from(rgb(c)));
    let drag = DragConnection {
        id: entry.id().clone(),
        name: config.name.clone(),
    };
    let mut container = div()
        .id(("connection", index))
        .w_full()
        .flex()
        .flex_col()
        .rounded(cx.theme().radius)
        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()));
    // Prod rows carry a faint tint so they stand out in a long list; the left edge belongs to
    // the focus indicator.
    if is_prod && !state.selected {
        let fg = env_accent(Environment::Prod, appearance).fg;
        container = container.bg(Hsla::from(rgb(fg.0)).opacity(PROD_TINT));
    }
    let mut line = list_row(("connection-line", index), state, cx)
        .pl(row_pad(depth))
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
        )
        .tooltip(tip(format!("{} · {}", config.name, config.driver)));
    line = match &browser {
        Some(browser) => {
            let (conn, root) = (browser.conn.clone(), browser.root);
            line.child(
                chevron_spacer()
                    .id(("connection-toggle", index))
                    .cursor_pointer()
                    .child(row_icon(
                        if browser.expanded {
                            AppIcon::ChevronDown
                        } else {
                            AppIcon::ChevronRight
                        },
                        muted,
                    ))
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        view.dispatch_object(&conn, root, BrowserAction::Toggle, cx);
                        cx.stop_propagation();
                    })),
            )
        }
        None => line.child(chevron_spacer()),
    };
    // The slot is always reserved so the names of all connections line up.
    let mut dot_slot = div().size(px(DOT)).flex_none().rounded_full();
    if let Some(color) = dot {
        dot_slot = dot_slot.bg(color);
    }
    line = line.child(dot_slot);
    line = line.child(row_icon(AppIcon::Server, muted)).child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(px(ui_scale(cx).sm))
            .child(config.name.clone()),
    );
    if let Some(env) = config.environment {
        line = line.child(env_chip(env, cx));
    }
    match entry.status() {
        ConnectionStatus::Idle => {}
        ConnectionStatus::Connecting => {
            line = line.child(running_spinner(muted));
        }
        ConnectionStatus::Failed(_) => {
            line = line.child(row_icon(AppIcon::CircleAlert, danger));
        }
    }
    container = container.child(line);

    let note_pad = row_pad(depth + 1);
    let note = |text: String, color: Hsla, cx: &App| {
        div()
            .pl(note_pad)
            .pr(px(SPACE[3]))
            .text_size(xs(cx))
            .text_color(color)
            .child(text)
    };
    match entry.status() {
        ConnectionStatus::Idle => {}
        ConnectionStatus::Connecting => {
            container = container.child(note("connecting…".to_owned(), muted, cx));
        }
        ConnectionStatus::Failed(message) => {
            container = container.child(note(message.clone(), danger, cx));
        }
    }
    match browser.as_ref().and_then(|b| b.note.as_ref()) {
        Some(BrowserNote::Busy(text)) => {
            container = container.child(note((*text).to_owned(), muted, cx));
        }
        Some(BrowserNote::Failed(message)) => {
            container = container.child(note(format!("Browser: {message}"), danger, cx));
        }
        None => {}
    }
    if state.selected {
        let id = entry.id().clone();
        container = container.child(
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE[1]))
                .pl(note_pad)
                .child(
                    Button::new(("open-connection", index))
                        .ghost()
                        .xsmall()
                        .label("Open")
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.dispatch(WorkspaceAction::OpenConnection(index), cx);
                            cx.stop_propagation();
                        })),
                )
                .child(action_button(
                    ("edit-connection", index),
                    "Edit",
                    ConnectionAction::Edit(id.clone()),
                    cx,
                ))
                .child(action_button(
                    ("move-connection", index),
                    "Move",
                    ConnectionAction::OpenMoveTo(id.clone()),
                    cx,
                ))
                .child(action_button(
                    ("delete-connection", index),
                    "Delete",
                    ConnectionAction::Delete(id),
                    cx,
                )),
        );
    }
    match browser {
        Some(b) => {
            let conn = b.conn.clone();
            with_menu(container, &conn, b.root, b.menu, cx)
        }
        None => container.into_any_element(),
    }
}

/// The empty state of the connection list: what to do next, with the action at hand.
pub(crate) fn connections_empty(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> Div {
    let hint = view.shortcut_hint(&ConnectionAction::New.command_id());
    empty_state(
        AppIcon::Database,
        "No connections yet",
        "Add a database connection to browse its objects and run queries.",
        cx,
    )
    .py(px(SPACE[5]))
    .px(px(SPACE[4]))
    .text_center()
    .child(
        div().pt(px(SPACE[3])).child(
            // Outline: a welcome screen next to this list owns the one primary action.
            Button::new("empty-new-connection")
                .outline()
                .small()
                .icon(AppIcon::Plus)
                .label("New Connection")
                .tooltip(tooltip_text("New Connection", hint.as_deref()))
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                    view.dispatch_connection(ConnectionAction::New, cx);
                })),
        ),
    )
}

#[cfg(test)]
mod tests {
    use sqler_catalog::NodeKey;
    use sqler_driver_api::sqler_core::{ColumnInfo, DataType, DbObject, ObjectPath};

    use super::*;

    fn object(kind: ObjectKind) -> TreeNodeKind {
        TreeNodeKind::Object(DbObject::new(
            kind,
            ObjectPath::root().in_schema("public").in_table("t"),
            "x",
        ))
    }

    #[test]
    fn node_icon_table() {
        let cases = [
            (ObjectKind::Database, AppIcon::Database),
            (ObjectKind::Schema, AppIcon::ListTree),
            (ObjectKind::Table, AppIcon::Table),
            (ObjectKind::View, AppIcon::Eye),
            (ObjectKind::MaterializedView, AppIcon::Table2),
            (ObjectKind::Function, AppIcon::SquareFunction),
            (ObjectKind::Procedure, AppIcon::SquareFunction),
            (ObjectKind::Trigger, AppIcon::SquarePlay),
            (ObjectKind::Sequence, AppIcon::FileText),
            (ObjectKind::Type, AppIcon::FileCode),
            (ObjectKind::Index, AppIcon::KeyRound),
            (ObjectKind::Extension, AppIcon::Plug),
            (ObjectKind::Role, AppIcon::KeyRound),
        ];
        for (kind, icon) in cases {
            assert_eq!(node_icon(&object(kind), false), icon, "{kind:?}");
        }
    }

    #[test]
    fn node_icon_structure_nodes() {
        let folder = TreeNodeKind::Folder(NodeKey::new(ObjectPath::root(), ObjectKind::Table));
        assert_eq!(node_icon(&folder, false), AppIcon::Folder);
        assert_eq!(node_icon(&folder, true), AppIcon::FolderOpen);
        let columns = TreeNodeKind::Columns(DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("public"),
            "t",
        ));
        assert_eq!(node_icon(&columns, true), AppIcon::FolderOpen);
        assert_eq!(node_icon(&TreeNodeKind::Root, true), AppIcon::Server);
        let column = TreeNodeKind::Column {
            table: DbObject::new(ObjectKind::Table, ObjectPath::root(), "t"),
            column: ColumnInfo::new("id", DataType::Integer),
        };
        assert_eq!(node_icon(&column, false), AppIcon::Columns2);
    }

    #[test]
    fn entry_icons() {
        assert_eq!(entry_icon(MenuEntry::OpenData), Some(AppIcon::Table));
        assert_eq!(entry_icon(MenuEntry::Drop), None);
        assert_eq!(entry_icon(MenuEntry::Refresh), Some(AppIcon::RefreshCw));
    }
}
