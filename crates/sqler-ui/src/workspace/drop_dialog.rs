// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The DROP confirmation: a full-window overlay that shows the statement in monospace. It only
//! renders [`DropDialog`]; the buttons run `object:cancel-drop` and `object:confirm-drop`.

use gpui::{
    AnyElement, ClickEvent, Context, Div, IntoElement as _, ParentElement as _, Styled as _, div,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _};
use sqler_driver_api::sqler_core::Environment;

use super::WorkspaceView;
use super::chrome::{
    DIALOG_W_MD, ICON_MD, dialog_footer, dialog_panel, dialog_title, scrim, ui_scale,
};
use crate::browser::{BrowserAction, DropDialog};
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::env_chip;

/// The buttons: Cancel and the confirm button of the current step.
fn footer(label: Option<&'static str>, prod: bool, cx: &mut Context<WorkspaceView>) -> Div {
    let mut footer = dialog_footer(cx);
    if let Some(label) = label {
        // The production step has its own element id and sits left of Cancel, so the click
        // that passed the first step can not land on it, and a double click is ignored.
        let confirm = Button::new(if prod {
            "drop-confirm-prod"
        } else {
            "drop-confirm"
        })
        .label(label)
        .primary()
        .danger()
        .small()
        .on_click(cx.listener(|view, event: &ClickEvent, _, cx| {
            if event.click_count() == 1 {
                view.dispatch_browser(BrowserAction::ConfirmDrop, cx);
            }
        }));
        let cancel = Button::new("drop-cancel")
            .label("Cancel")
            .outline()
            .small()
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
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let (mono, code_bg, radius) = (theme.mono_font_family.clone(), theme.muted, theme.radius);
    let scale = ui_scale(cx);

    // `dialog_title` draws a muted icon; this one is a warning, so the icon is danger-coloured.
    let mut title = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[3]))
        .child(
            Icon::new(AppIcon::TriangleAlert)
                .size(px(ICON_MD))
                .text_color(danger),
        )
        .child(dialog_title(None, dialog.title, cx));
    if dialog.prod_step {
        title = title.child(env_chip(Environment::Prod, cx));
    }
    let footer = footer(dialog.confirm_label, dialog.prod_step, cx);

    Some(
        scrim("drop-scrim", cx)
            .child(
                dialog_panel("drop-dialog", DIALOG_W_MD, cx)
                    .child(title)
                    .child(
                        div()
                            .text_size(px(scale.sm))
                            .text_color(muted)
                            .child(dialog.body),
                    )
                    .child(
                        div()
                            .p(px(SPACE[3]))
                            .rounded(radius)
                            .bg(code_bg)
                            .font_family(mono)
                            .text_size(px(scale.mono))
                            .overflow_hidden()
                            .child(dialog.sql),
                    )
                    .child(footer),
            )
            .into_any_element(),
    )
}
