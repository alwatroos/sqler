// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The DROP confirmation: a full-window overlay that shows the statement in monospace. It only
//! renders [`DropDialog`]; the buttons run `object:cancel-drop` and `object:confirm-drop`.

use gpui::{
    AnyElement, ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::ActiveTheme as _;

use super::WorkspaceView;
use crate::browser::{BrowserAction, DropDialog};

/// The buttons: Cancel and the confirm button of the current step.
fn footer(label: Option<&'static str>, cx: &mut Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    let (border, danger, danger_fg) = (theme.border, theme.danger, theme.danger_foreground);
    let mut footer = div().flex().flex_row().justify_end().gap_2().p_4();
    if let Some(label) = label {
        // The production step has its own element id and sits left of Cancel, so the click
        // that passed the first step can not land on it, and a double click is ignored.
        let prod = label != "Drop";
        let confirm = div()
            .id(if prod {
                "drop-confirm-prod"
            } else {
                "drop-confirm"
            })
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(danger)
            .bg(danger)
            .text_color(danger_fg)
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(|view, event: &ClickEvent, _, cx| {
                if event.click_count() == 1 {
                    view.dispatch_browser(BrowserAction::ConfirmDrop, cx);
                }
            }));
        let cancel = div()
            .id("drop-cancel")
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(border)
            .cursor_pointer()
            .child("Cancel")
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.dispatch_browser(BrowserAction::CancelDrop, cx);
            }));
        footer = if prod {
            footer.child(confirm).child(cancel)
        } else {
            footer.child(cancel).child(confirm)
        };
    }
    footer
}

/// Renders the dialog; `None` when no DROP is waiting for confirmation.
pub(crate) fn drop_overlay(
    dialog: Option<DropDialog>,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let dialog = dialog?;
    let theme = cx.theme();
    let (panel_bg, border, fg, muted) = (
        theme.popover,
        theme.border,
        theme.popover_foreground,
        theme.muted_foreground,
    );
    let mono = theme.mono_font_family.clone();
    let code_bg = theme.muted;

    let footer = footer(dialog.confirm_label, cx);

    Some(
        div()
            .id("drop-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.45))
            .occlude()
            .child(
                div()
                    .id("drop-dialog")
                    .w(px(580.))
                    .max_h(px(720.))
                    .flex()
                    .flex_col()
                    .bg(panel_bg)
                    .text_color(fg)
                    .border_1()
                    .border_color(border)
                    .rounded_lg()
                    .shadow_lg()
                    .child(div().px_4().pt_4().pb_2().text_lg().child(dialog.title))
                    .child(
                        div()
                            .px_4()
                            .pb_2()
                            .text_sm()
                            .text_color(muted)
                            .child(dialog.body),
                    )
                    .child(
                        div()
                            .mx_4()
                            .p_2()
                            .rounded_md()
                            .bg(code_bg)
                            .font_family(mono)
                            .text_sm()
                            .overflow_hidden()
                            .child(dialog.sql),
                    )
                    .child(footer),
            )
            .into_any_element(),
    )
}
