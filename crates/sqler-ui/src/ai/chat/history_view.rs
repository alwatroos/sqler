// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The list of saved chats in the chat panel (ADR-0077). It replaces the transcript while the
//! list is open. It only renders [`ChatList`](crate::workspace::ChatList); every button dispatches
//! an [`AiAction`]. Nothing here reads the store.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _};

use super::{ChatRow, HISTORY_CONFIRM, HISTORY_EMPTY, HISTORY_TITLE};
use crate::ai::action::AiAction;
use crate::ai::session::view::centred;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, icon_button};
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::ui_scale;

/// The line shown while the saved chats are read.
const LOADING: &str = "Loading saved chats\u{2026}";
/// The mark of the chat on screen.
const CURRENT: &str = "Current";

/// The body of the panel while the list is open: the title, the rows or why there are none, and
/// the footer with Clear history.
pub(crate) fn history_body(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let ws = view.workspace();
    let list = ws.chat_list();
    let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
    let scale = ui_scale(cx);
    let content = if list.is_loading() {
        div()
            .id("chat-history-loading")
            .px(px(SPACE[4]))
            .py(px(SPACE[3]))
            .text_size(px(scale.sm))
            .text_color(muted)
            .child(LOADING)
            .into_any_element()
    } else if let Some(why) = list.error() {
        centred(
            empty_state(AppIcon::Clock, HISTORY_TITLE, why.to_owned(), cx),
            cx,
        )
    } else if list.rows().is_empty() {
        centred(
            empty_state(AppIcon::Clock, HISTORY_TITLE, HISTORY_EMPTY, cx),
            cx,
        )
    } else {
        rows(ws.chat_rows(), cx)
    };
    let has_rows = !list.is_loading() && list.error().is_none() && !list.rows().is_empty();
    let mut body = div()
        .id("chat-history")
        .debug_selector(|| "chat-history".to_owned())
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .px(px(SPACE[4]))
                .py(px(SPACE[2]))
                .text_size(px(scale.xs))
                .font_weight(FontWeight::MEDIUM)
                .text_color(muted)
                .child(HISTORY_TITLE),
        )
        .child(div().flex_1().min_h_0().flex().flex_col().child(content));
    if list.confirming() {
        body = body.child(confirm_strip(cx));
    } else if has_rows {
        body = body.child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .px(px(SPACE[4]))
                .py(px(SPACE[2]))
                .border_t_1()
                .border_color(border)
                .child(
                    Button::new("chat-history-clear")
                        .label("Clear history")
                        .small()
                        .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                            view.dispatch_ai(AiAction::ClearChatHistory, cx);
                        })),
                ),
        );
    }
    body.into_any_element()
}

/// The scrolling list of rows.
fn rows(rows: Vec<ChatRow>, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let mut column = div().flex().flex_col().py(px(SPACE[1]));
    for (ix, row) in rows.into_iter().enumerate() {
        column = column.child(row_element(ix, row, cx));
    }
    div()
        .id("chat-history-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(column)
        .into_any_element()
}

/// One saved chat: the title and the connection and age open it, the cross deletes it.
fn row_element(ix: usize, row: ChatRow, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (muted, hover, primary) = (theme.muted_foreground, theme.muted, theme.primary);
    let ChatRow {
        id,
        title,
        line,
        current,
    } = row;
    let (open_id, delete_id) = (id.clone(), id);
    let mut meta = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .text_size(px(scale.xs))
        .text_color(muted);
    if current {
        meta = meta.child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(primary)
                .child(CURRENT),
        );
    }
    meta = meta.child(div().min_w_0().truncate().child(line));
    div()
        .id(("chat-past-row", ix))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .px(px(SPACE[4]))
        .hover(move |style| style.bg(hover))
        .child(
            div()
                .id(("chat-past", ix))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(SPACE[1]))
                .py(px(SPACE[2]))
                .cursor_pointer()
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::OpenPastChat(open_id.clone()), cx);
                }))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(scale.sm))
                        .child(title),
                )
                .child(meta),
        )
        .child(
            icon_button(("chat-past-delete", ix), AppIcon::X, "Delete this chat").on_click(
                cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::DeletePastChat(delete_id.clone()), cx);
                }),
            ),
        )
        .into_any_element()
}

/// The inline strip that asks before everything is deleted.
fn confirm_strip(cx: &mut Context<WorkspaceView>) -> AnyElement {
    let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
    div()
        .id("chat-history-confirm")
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .px(px(SPACE[4]))
        .py(px(SPACE[3]))
        .border_t_1()
        .border_color(border)
        .child(
            div()
                .text_size(px(ui_scale(cx).sm))
                .text_color(muted)
                .child(HISTORY_CONFIRM),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE[2]))
                .child(
                    Button::new("chat-history-delete-all")
                        .label("Delete all")
                        .small()
                        .danger()
                        .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                            view.dispatch_ai(AiAction::ConfirmClearChatHistory, cx);
                        })),
                )
                .child(
                    Button::new("chat-history-cancel")
                        .label("Cancel")
                        .small()
                        .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                            view.dispatch_ai(AiAction::CancelClearChatHistory, cx);
                        })),
                ),
        )
        .into_any_element()
}
