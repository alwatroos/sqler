// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tab bar and tab body views: render `TabStrip` groups; clicks become `WorkspaceAction`s.

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, ElementId, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, Role, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
    rgb,
};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _};

use super::bar::{self, MENU, MenuEntry, Trailing};
use super::{GroupId, Rgb, TabDecor, TabId, TabKind};
use crate::browser::{BrowserAction, ObjectTab, ObjectTabContent};
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{env_chip, icon_button};
use crate::theme::{Appearance, env_accent};
use crate::workspace::{Effect, WorkspaceAction, WorkspaceView};

/// The height of a tab bar, in pixels.
const BAR_HEIGHT: f32 = 32.;

/// The payload of a drag that started on a tab.
#[derive(Clone)]
pub(crate) struct DragTab {
    pub(crate) id: TabId,
    pub(crate) title: SharedString,
}

impl Render for DragTab {
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
            .child(self.title.clone())
    }
}

fn kind_icon(kind: TabKind) -> AppIcon {
    match kind {
        TabKind::Grid => AppIcon::Table,
        TabKind::Ddl => AppIcon::FileText,
        TabKind::Properties => AppIcon::SlidersHorizontal,
        TabKind::History => AppIcon::Clock,
        _ => AppIcon::FileCode,
    }
}

/// The accessible name of a tab: its title plus the state words.
fn aria_title(decor: &TabDecor) -> String {
    let mut label = decor.header.title.clone();
    if decor.header.preview {
        label.push_str(", preview");
    }
    if decor.dirty {
        label.push_str(", unsaved");
    }
    label
}

/// Runs `actions` on the workbench, then `copy` (a title) through the clipboard effect.
fn run_entry(
    weak: &gpui::WeakEntity<WorkspaceView>,
    entry: MenuEntry,
    id: TabId,
    title: &SharedString,
    cx: &mut gpui::App,
) {
    let actions = entry.actions(id);
    let copy = (entry == MenuEntry::CopyTitle).then(|| title.to_string());
    weak.update(cx, |view, cx| {
        for action in actions {
            view.dispatch(action, cx);
        }
        if let Some(text) = copy {
            view.apply(Effect::Copy(text), cx);
        }
    })
    .ok();
}

/// The trailing slot of a tab: spinner, close button, dirty dot or an empty slot that turns into
/// the close button on hover.
fn trailing_slot(
    id: TabId,
    decor: &TabDecor,
    active: bool,
    hover_group: &SharedString,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let warning = cx.theme().warning;
    let close = |hidden: bool, cx: &mut Context<WorkspaceView>| {
        let button = div()
            .id(("tab-close", id.raw()))
            .size(px(16.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_sm()
            .text_color(muted)
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().muted))
            .role(Role::Button)
            .aria_label("Close tab")
            .child(Icon::new(AppIcon::X).size(px(12.)))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.dispatch(WorkspaceAction::CloseTab(id), cx);
                cx.stop_propagation();
            }));
        if hidden {
            button
                .absolute()
                .top_0()
                .left_0()
                .invisible()
                .group_hover(hover_group.clone(), gpui::Styled::visible)
                .into_any_element()
        } else {
            button.into_any_element()
        }
    };
    match bar::trailing(decor, false, active) {
        Trailing::Spinner => Spinner::new()
            .icon(AppIcon::LoaderCircle)
            .color(muted)
            .xsmall()
            .into_any_element(),
        Trailing::Close => close(false, cx),
        Trailing::Dirty => div()
            .relative()
            .size(px(16.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id(("tab-dirty", id.raw()))
                    .size(px(8.))
                    .rounded_full()
                    .bg(warning)
                    .tooltip(|window, cx| Tooltip::new("Unsaved changes").build(window, cx))
                    .group_hover(hover_group.clone(), gpui::Styled::invisible),
            )
            .child(close(true, cx))
            .into_any_element(),
        Trailing::None => div()
            .relative()
            .size(px(16.))
            .flex_none()
            .child(close(true, cx))
            .into_any_element(),
    }
}

/// One tab of a bar.
#[allow(clippy::too_many_lines)] // one declarative element tree
fn tab_item(
    view: &WorkspaceView,
    id: TabId,
    group: GroupId,
    index: usize,
    highlight: bool,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let ws = view.workspace();
    let decor = ws.tab_decor(id)?;
    let environment = ws.tabs().get(id)?.connection().environment;
    let active = highlight && ws.tabs().active_in(group).map(super::Tab::id) == Some(id);
    // In a split, the active tab of a group that does not hold the focus reads as resting.
    let dimmed = ws.tabs().groups().len() > 1 && ws.tabs().focused_group() != group;
    let appearance = if cx.theme().mode.is_dark() {
        Appearance::Dark
    } else {
        Appearance::Light
    };
    let theme = cx.theme();
    let hover_fill = theme.list_hover;
    let (fill, text, muted, border) = if active {
        (
            theme.tab_active,
            if dimmed {
                theme.tab_foreground
            } else {
                theme.tab_active_foreground
            },
            theme.muted_foreground,
            theme.border,
        )
    } else {
        (
            theme.tab,
            theme.tab_foreground,
            theme.muted_foreground,
            theme.border,
        )
    };
    let drop_bg = theme.drop_target;
    let top = match environment {
        Some(env) if active && decor.header.is_prod => env_accent(env, appearance)
            .solid
            .map_or(gpui::transparent_black(), |(solid, _)| rgb(solid.0).into()),
        _ => gpui::transparent_black(),
    };
    let dot = decor.header.accent.map_or(border, |Rgb(c)| rgb(c).into());
    let title: SharedString = decor.header.title.clone().into();
    let hover_group: SharedString = format!("tab-{}", id.raw()).into();
    let drag = DragTab {
        id,
        title: title.clone(),
    };

    let label = div()
        .min_w(px(40.))
        .max_w(px(220.))
        .truncate()
        .text_sm()
        .child(title.clone());
    let label = if decor.header.preview {
        label.italic()
    } else {
        label
    };
    let mut item = div()
        .id(("tab", id.raw()))
        .group(hover_group.clone())
        .h_full()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_2()
        .px_2()
        .bg(fill)
        .text_color(text)
        .relative()
        .border_r_1()
        .border_color(border)
        .cursor_pointer()
        .when(!active, |tab| {
            tab.border_b_1().hover(move |s| s.bg(hover_fill))
        })
        .role(Role::Tab)
        .aria_label(aria_title(&decor))
        .tooltip({
            let title = title.clone();
            move |window, cx| Tooltip::new(title.clone()).build(window, cx)
        })
        .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
            view.dispatch(WorkspaceAction::ActivateTab(id), cx);
            if event.click_count() >= 2 {
                view.dispatch(WorkspaceAction::PinTab(id), cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |view, _, _, cx| {
                view.dispatch(WorkspaceAction::CloseTab(id), cx);
                cx.stop_propagation();
            }),
        )
        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .drag_over::<DragTab>(move |style, _, _, _| style.bg(drop_bg))
        .on_drop(cx.listener(move |view, drag: &DragTab, _, cx| {
            view.dispatch(
                WorkspaceAction::MoveTab {
                    id: drag.id,
                    group,
                    index,
                },
                cx,
            );
            cx.stop_propagation();
        }))
        .child(div().size(px(6.)).flex_none().rounded_full().bg(dot))
        .child(
            Icon::new(kind_icon(decor.header.kind))
                .size(px(14.))
                .text_color(muted),
        )
        .child(label);
    // The stripe on top of an active prod tab; the other tabs draw it transparent.
    item = item.child(
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(2.))
            .bg(top),
    );
    if let Some(env) = environment {
        item = item.child(env_chip(env, cx));
    }
    item = item.child(trailing_slot(id, &decor, active, &hover_group, cx));

    let weak = cx.weak_entity();
    Some(
        item.context_menu(move |mut menu, _, _| {
            for entry in MENU {
                let (weak, title) = (weak.clone(), title.clone());
                menu = menu.item(PopupMenuItem::new(entry.label()).on_click(move |_, _, cx| {
                    run_entry(&weak, entry, id, &title, cx);
                }));
            }
            menu
        })
        .into_any_element(),
    )
}

/// Renders the tab bar of one group: the scrolling tab row, the tab list dropdown and (group 1)
/// the settings header and theme toggle.
pub(crate) fn tab_bar(
    view: &WorkspaceView,
    group: GroupId,
    scroll: &ScrollHandle,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let ws = view.workspace();
    let groups = ws.tabs().groups();
    let position = groups.iter().position(|g| g.id() == group).unwrap_or(0);
    let ids: Vec<TabId> = groups
        .get(position)
        .map(|g| g.tabs().to_vec())
        .unwrap_or_default();
    let first = position == 0;
    let highlight = !(first && ws.settings_tab_active());
    let theme = cx.theme();
    let (bar_bg, border) = (theme.tab_bar, theme.border);

    let items: Vec<AnyElement> = ids
        .iter()
        .enumerate()
        .filter_map(|(index, &id)| tab_item(view, id, group, index, highlight, cx))
        .collect();

    let list: Vec<(TabId, SharedString)> = ids
        .iter()
        .filter_map(|&id| {
            ws.tab_decor(id)
                .map(|d| (id, SharedString::from(d.header.title)))
        })
        .collect();
    let weak = cx.weak_entity();
    let tab_list = icon_button(
        ("tab-list", position),
        AppIcon::ChevronDown,
        "Show all tabs",
    )
    .dropdown_menu(move |mut menu, _, _| {
        for (id, title) in &list {
            let (weak, id) = (weak.clone(), *id);
            menu = menu.item(PopupMenuItem::new(title.clone()).on_click(move |_, _, cx| {
                weak.update(cx, |view, cx| {
                    view.dispatch(WorkspaceAction::ActivateTab(id), cx);
                })
                .ok();
            }));
        }
        menu
    });

    let row = div()
        .id(ElementId::from(("tab-row", position)))
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_row()
        .overflow_x_scroll()
        .track_scroll(scroll)
        .children(items);

    let mut bar = div()
        .id(ElementId::from(("tab-bar", position)))
        .role(Role::TabList)
        .w_full()
        .h(px(BAR_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .relative()
        .bg(bar_bg)
        .on_drop(cx.listener(move |view, drag: &DragTab, _, cx| {
            view.dispatch(
                WorkspaceAction::MoveTab {
                    id: drag.id,
                    group,
                    index: usize::MAX,
                },
                cx,
            );
        }))
        .child(
            // The rule under the bar; the active tab covers it.
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(1.))
                .bg(border),
        )
        .child(row)
        .child(tab_list);
    if first {
        bar = bar.children(crate::settings::view::strip_end(ws, cx));
    }
    bar.into_any_element()
}

/// Renders the body of the active tab when no other view claims it: the empty-state hint, or the
/// moment before the editor of a new tab exists.
pub(crate) fn tab_body(
    tabs: &super::TabStrip,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    let text = match tabs.active() {
        // Query tabs get their own body (`query::view::query_tab_body`); this is a fallback.
        Some(tab) => format!(
            "Connected to {} ({}).",
            tab.connection().name,
            tab.connection().driver
        ),
        None => "Double-click a connection in the sidebar to open a SQL editor, or run \"Editor: New SQL Editor\" (editor:new) from the command palette."
            .to_owned(),
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

#[allow(clippy::too_many_lines)] // one declarative element tree
/// Renders the body of a DDL or properties tab: read-only monospace text, or a two-column table
/// of fields, with a Copy button; or the loading or failure message.
pub(crate) fn object_tab_body(
    tab: &ObjectTab,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (muted, danger, border, mono) = (
        theme.muted_foreground,
        theme.danger,
        theme.border,
        theme.mono_font_family.clone(),
    );
    let message = |text: String, color| -> AnyElement {
        div()
            .id("object-tab-message")
            .flex_1()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(color)
            .child(text)
            .into_any_element()
    };
    let copy = |label: &'static str, cx: &mut Context<WorkspaceView>| {
        div()
            .id("object-tab-copy")
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(border)
            .text_sm()
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.dispatch_browser(BrowserAction::CopyTabText, cx);
            }))
    };
    let open_in_editor = |cx: &mut Context<WorkspaceView>| {
        div()
            .id("object-tab-open-in-editor")
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(border)
            .text_sm()
            .cursor_pointer()
            .child("Open in Editor")
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.dispatch_browser(BrowserAction::OpenTabInEditor, cx);
            }))
    };
    let body = match &tab.content {
        ObjectTabContent::Loading => message(format!("{}…", tab.title), muted),
        ObjectTabContent::Failed(error) => message(error.clone(), danger),
        ObjectTabContent::Script(text) => div()
            .id("object-tab-text")
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_1()
                    .p_1()
                    .child(open_in_editor(cx))
                    .child(copy("Copy", cx)),
            )
            .child(
                div()
                    .id("object-tab-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_3()
                    .font_family(mono)
                    .text_sm()
                    .child(text.clone()),
            )
            .into_any_element(),
        ObjectTabContent::Properties(fields) => {
            let rows = fields.iter().map(|field| {
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .py_0p5()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .w(px(180.))
                            .flex_none()
                            .text_color(muted)
                            .child(field.label.clone()),
                    )
                    .child(div().flex_1().min_w_0().child(field.value.clone()))
            });
            div()
                .id("object-tab-properties")
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .p_1()
                        .child(copy("Copy", cx)),
                )
                .child(
                    div()
                        .id("object-tab-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .pb_3()
                        .text_sm()
                        .children(rows),
                )
                .into_any_element()
        }
    };
    div()
        .id("object-tab")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(body)
}
