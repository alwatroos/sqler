// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The approval card of an AI session (ADR-0053): the exact statement the model wants to run and
//! the two buttons. Nothing runs until the user approves. "Deny" holds the default focus, so a
//! stray Enter never approves. On a production connection a second confirmation follows in the
//! run gate (ADR-0024).

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _};
use sqler_driver_api::sqler_core::Environment;

use super::{PendingApproval, card_title};
use crate::ai::action::AiAction;
use crate::tabs::TabId;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::env_chip;
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{LIST_MAX_H, ui_scale};

/// The line under the statement.
const NOTE: &str = "Nothing runs until you approve.";
/// What follows the note on a production connection.
const NOTE_PROD: &str = " A second confirmation follows.";

/// The card for the ask the user answers now.
pub(crate) fn approval_card(
    view: &WorkspaceView,
    tab: TabId,
    ask: &PendingApproval,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (border, muted, radius) = (theme.border, theme.muted_foreground, theme.radius);
    let (code_bg, warning, bg) = (theme.muted, theme.warning, theme.popover);
    let mono = theme.mono_font_family.clone();
    let env = ask.request.connection.environment;
    let prod = env == Some(Environment::Prod);
    let mut note = NOTE.to_owned();
    if prod {
        note.push_str(NOTE_PROD);
    }
    let mut head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .text_size(px(scale.md))
        .font_weight(FontWeight::MEDIUM)
        .child(gpui_component::Icon::new(AppIcon::TriangleAlert).text_color(warning))
        .child(div().min_w_0().truncate().child(card_title(&ask.request)));
    if let Some(env) = env {
        head = head.child(env_chip(env, cx));
    }
    let sql = div()
        .id("approval-sql")
        .max_h(px(LIST_MAX_H))
        .overflow_y_scroll()
        .p(px(SPACE[3]))
        .rounded(radius)
        .bg(code_bg)
        .font_family(mono)
        .text_size(px(scale.mono))
        .child(ask.request.sql.clone());
    // The wrapper holds the default focus (Deny), so a stray Enter never approves.
    let mut deny = div().child(
        Button::new("approval-deny")
            .label("Deny")
            .outline()
            .small()
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.dispatch_ai(AiAction::DenyStatement(tab), cx);
            })),
    );
    if let Some(focus) = view.ai_deny_focus(tab) {
        deny = deny.track_focus(focus);
    }
    let approve = Button::new("approval-approve")
        .label("Approve")
        .primary()
        .small()
        .when_prod(prod)
        .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
            if event.click_count() == 1 {
                view.dispatch_ai(AiAction::ApproveStatement(tab), cx);
            }
        }));
    div()
        .id("approval-card")
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .p(px(SPACE[4]))
        .rounded(radius)
        .border_1()
        .border_color(border)
        .bg(bg)
        .child(head)
        .child(sql)
        .child(div().text_size(px(scale.sm)).text_color(muted).child(note))
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(SPACE[2]))
                .child(deny)
                .child(approve),
        )
        .into_any_element()
}

/// Makes the approve button `danger` on a production connection.
trait WhenProd {
    fn when_prod(self, prod: bool) -> Self;
}

impl WhenProd for Button {
    fn when_prod(self, prod: bool) -> Self {
        if prod { self.danger() } else { self }
    }
}
