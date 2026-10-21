// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The toolbar above the SQL editor: breadcrumbs and environment on the left; Run (with a menu
//! of the other run modes), Run All, Cancel, Explain, Format and Save on the right.
//!
//! Every button runs its command through `run_command_id`, so a click takes the same path as the
//! key, including the production confirmation. The enabled state comes from the headless
//! `ToolbarContext`.

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, IntoElement as _, Styled as _, WeakEntity,
    Window, div, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{Disableable as _, Sizable as _};

use crate::query::{RunState, ToolbarContext, ToolbarItem, toolbar_tooltip};
use crate::tabs::TabId;
use crate::tabs::view::editor_toolbar;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::icon_button;
use crate::workbench::view::run_command_id;
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{ICON_MD, hinted_item};

/// Width of the rule between two button groups.
const RULE: f32 = 1.;

/// The icon of a toolbar item.
fn item_icon(item: ToolbarItem) -> AppIcon {
    match item {
        ToolbarItem::Run => AppIcon::Play,
        ToolbarItem::RunAll => AppIcon::SquarePlay,
        ToolbarItem::Cancel => AppIcon::CircleStop,
        ToolbarItem::Explain => AppIcon::ListTree,
        ToolbarItem::Format => AppIcon::WandSparkles,
        ToolbarItem::Save => AppIcon::Save,
    }
}

/// The element ID prefix of an item; the tab ID is the second part.
fn item_key(item: ToolbarItem) -> &'static str {
    match item {
        ToolbarItem::Run => "query-run",
        ToolbarItem::RunAll => "query-run-all",
        ToolbarItem::Cancel => "query-cancel",
        ToolbarItem::Explain => "query-explain",
        ToolbarItem::Format => "query-format",
        ToolbarItem::Save => "query-save",
    }
}

/// The run modes behind the caret next to Run: the label, the command and the icon.
const RUN_MENU: [(&str, &str, AppIcon); 3] = [
    ("Run Selection", "query:run-selection", AppIcon::Play),
    (
        "Run Statement at Cursor",
        "query:run-statement",
        AppIcon::Play,
    ),
    ("Run All", "query:run-all", AppIcon::SquarePlay),
];

/// The button of `item`, wired to its command.
fn item_button(
    view: &WorkspaceView,
    tab: TabId,
    item: ToolbarItem,
    ctx: ToolbarContext,
    cx: &mut Context<WorkspaceView>,
) -> Button {
    let id = item.command_id();
    let reason = ctx.disabled_reason(item);
    let hint = view.shortcut_hint(&id);
    let key = ElementId::from((item_key(item), tab.raw()));
    let button = if item == ToolbarItem::Run {
        Button::new(key)
            .primary()
            .small()
            .icon(item_icon(item))
            .label("Run")
    } else {
        icon_button(key, item_icon(item), "")
    };
    button
        .tab_stop(true)
        .accessibility_label(item.title())
        .disabled(reason.is_some())
        .tooltip(toolbar_tooltip(item, hint.as_deref(), reason))
        .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
            run_command_id(view, id.as_str(), window, cx);
        }))
}

/// The caret next to Run: a menu of Run Selection, Run Statement at Cursor and Run All.
fn run_menu(
    view: &WorkspaceView,
    tab: TabId,
    reason: Option<&'static str>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let weak = cx.weak_entity();
    let hint = view.shortcut_hint(&ToolbarItem::Run.command_id());
    let tooltip = crate::theme::tooltip_text("More Run Options", hint.as_deref());
    icon_button(
        ElementId::from(("query-run-menu", tab.raw())),
        AppIcon::ChevronDown,
        match reason {
            Some(reason) => format!("More Run Options: {reason}"),
            None => tooltip,
        },
    )
    .accessibility_label("More Run Options")
    .disabled(reason.is_some())
    .dropdown_menu(move |mut menu, _, cx| {
        for (label, id, icon) in RUN_MENU {
            let hint = weak.upgrade().and_then(|view| {
                crate::command::CommandId::new(id)
                    .ok()
                    .and_then(|id| view.read(cx).shortcut_hint(&id))
            });
            let weak = weak.clone();
            menu = menu.item(hinted_item(label, Some(icon), hint).on_click(
                move |_, window, cx| {
                    run_from_menu(&weak, id, window, cx);
                },
            ));
        }
        menu
    })
    .into_any_element()
}

/// Runs command `id` on the view from a menu click, which has no view context of its own.
///
/// The menu handler runs inside the window's event dispatch, where the window is taken out of
/// the app; it must use the live `window` it is given instead of re-entering via its handle.
fn run_from_menu(
    weak: &WeakEntity<WorkspaceView>,
    id: &'static str,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    weak.update(cx, |view, cx| run_command_id(view, id, window, cx))
        .ok();
}

/// A thin vertical rule between two groups of buttons.
fn divider(cx: &Context<WorkspaceView>) -> Div {
    div()
        .w(px(RULE))
        .h(px(ICON_MD))
        .flex_none()
        .mx(px(SPACE[1]))
        .bg(cx.theme().border)
}

/// The toolbar of the query tab `tab`.
pub(super) fn query_toolbar(
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let crumbs = view.workspace().breadcrumbs(tab);
    let ctx = view
        .workspace()
        .query_toolbar(tab)
        .unwrap_or(ToolbarContext {
            run: RunState::Idle,
            connected: false,
            explain: false,
        });
    let actions = vec![
        item_button(view, tab, ToolbarItem::Run, ctx, cx).into_any_element(),
        run_menu(view, tab, ctx.disabled_reason(ToolbarItem::Run), cx),
        item_button(view, tab, ToolbarItem::RunAll, ctx, cx).into_any_element(),
        item_button(view, tab, ToolbarItem::Cancel, ctx, cx).into_any_element(),
        divider(cx).into_any_element(),
        item_button(view, tab, ToolbarItem::Explain, ctx, cx).into_any_element(),
        item_button(view, tab, ToolbarItem::Format, ctx, cx).into_any_element(),
        item_button(view, tab, ToolbarItem::Save, ctx, cx).into_any_element(),
    ];
    editor_toolbar("query-toolbar", crumbs.as_ref(), actions, cx).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_item_has_an_icon_and_a_key() {
        let mut keys = std::collections::HashSet::new();
        for item in ToolbarItem::ALL {
            let _ = item_icon(item);
            assert!(keys.insert(item_key(item)), "{item:?}");
        }
    }

    #[test]
    fn the_run_menu_commands_are_valid() {
        for (_, id, _) in RUN_MENU {
            assert!(crate::command::CommandId::new(id).is_ok(), "{id}");
        }
    }
}
