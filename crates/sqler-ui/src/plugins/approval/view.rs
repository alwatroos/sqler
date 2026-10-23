// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The approval dialog of a plugin write (AC6): the plugin, the connection and the exact statement, with Deny as the
//! default focus. Nothing runs until the user clicks Approve. On a production connection the second confirmation is
//! the query editor's production dialog, drawn from the queue's run gate.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _};
use sqler_driver_api::sqler_core::Environment;

use super::{WriteStage, card_title};
use crate::query::view::dialogs::confirm_panel;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::env_chip;
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{
    DIALOG_W_LG, LIST_MAX_H, dialog_footer, dialog_panel, dialog_title, scrim, ui_scale,
};

/// The line under the statement.
const NOTE: &str = "Nothing runs until you approve.";
/// What follows the note on a production connection.
const NOTE_PROD: &str = " A second confirmation follows.";

/// The dialog for the ask the user answers now; `None` when no ask waits.
pub(crate) fn write_overlay(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let queue = view.workspace().plugin_writes();
    let ask = queue.current()?;
    let (id, request) = (ask.id, &ask.request);
    match request.stage {
        WriteStage::ProdSecond => {
            let statements = queue.gate_statements()?;
            let cancel = cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.plugin_write_answer(id, WriteStage::ProdSecond, false, cx);
            });
            let confirm = cx.listener(move |view, event: &ClickEvent, _, cx| {
                if event.click_count() == 1 {
                    view.plugin_write_answer(id, WriteStage::ProdSecond, true, cx);
                }
            });
            let intro = format!("{} asks to run this.", request.plugin_name);
            Some(confirm_panel(
                &request.connection,
                statements,
                Some(intro),
                Some(view.plugin_write_focus()),
                cancel,
                confirm,
                cx,
            ))
        }
        _ => Some(first_dialog(view, id, request, cx)),
    }
}

fn first_dialog(
    view: &WorkspaceView,
    id: u64,
    request: &super::PluginWriteRequest,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (muted, code_bg, radius) = (theme.muted_foreground, theme.muted, theme.radius);
    let mono = theme.mono_font_family.clone();
    let mut note = NOTE.to_owned();
    if request.prod {
        note.push_str(NOTE_PROD);
    }
    let mut who = format!("On {}.", request.connection);
    if request.param_count > 0 {
        who = format!("{who} {} bound parameter(s).", request.param_count);
    }
    let sql = div()
        .id("plugin-write-sql")
        .max_h(px(LIST_MAX_H))
        .overflow_y_scroll()
        .p(px(SPACE[3]))
        .rounded(radius)
        .bg(code_bg)
        .font_family(mono)
        .text_size(px(scale.mono))
        .child(request.sql.clone());
    // The wrapper holds the default focus (Deny), so a stray Enter never approves.
    let deny = div()
        .child(
            Button::new("plugin-write-deny")
                .label("Deny")
                .outline()
                .small()
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.plugin_write_answer(id, WriteStage::First, false, cx);
                })),
        )
        .track_focus(view.plugin_write_focus());
    let mut approve = Button::new("plugin-write-approve")
        .label("Approve")
        .primary()
        .small()
        .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
            if event.click_count() == 1 {
                view.plugin_write_answer(id, WriteStage::First, true, cx);
            }
        }));
    if request.prod {
        approve = approve.danger();
    }
    let mut title = dialog_title(Some(AppIcon::TriangleAlert), card_title(request), cx);
    if request.prod {
        title = title.child(env_chip(Environment::Prod, cx));
    }
    scrim("plugin-write-scrim", cx)
        .child(
            dialog_panel("plugin-write-dialog", DIALOG_W_LG, cx)
                .child(title)
                .child(div().text_size(px(scale.sm)).text_color(muted).child(who))
                .child(sql)
                .child(div().text_size(px(scale.sm)).text_color(muted).child(note))
                .child(dialog_footer(cx).child(deny).child(approve)),
        )
        .into_any_element()
}
