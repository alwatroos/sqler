// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The body of the AI chat panel (ADR-0076): the header with the caps, the transcript with its
//! copy and open buttons, and the composer. It only renders the chat of the workspace; every
//! button dispatches an [`AiAction`]. Nothing here runs SQL.

use gpui::{
    AnyElement, ClickEvent, Context, Div, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement as _, ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use sqler_ai_tools::session::{SqlEntry, ToolCallStatus, TranscriptEntry};

use super::history_view::history_body;
use super::{
    CHAT_HELLO, CHAT_REFUSED_NOTE, CHAT_TITLE, HISTORY_CANNOT_CONTINUE, RESTORED_NOTE, caps_label,
    chat_sql_blocks, chat_tool_status_text, other_connection_text,
};
use crate::ai::action::AiAction;
use crate::ai::session::view::{centred, composer, streaming_row, tool_row};
use crate::ai::session::{
    AiSessionTab, DROPPED_MARKER, SESSION_AI_OFF, SessionPhase, notice_text, sql_footer,
};
use crate::tabs::TabId;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, icon_button, toolbar_button};
use crate::workspace::chrome::ui_scale;
use crate::workspace::{NO_PROVIDER, WorkspaceView};

/// The title of the empty state while the chat is built.
const STARTING_TITLE: &str = "Starting the chat\u{2026}";
/// The line under it.
const STARTING_LINE: &str = "The model is being connected to the database tools.";
/// The title of the empty state when the chat could not be built.
const FAILED_TITLE: &str = "The chat could not start";

/// The chat panel: header, then the empty state, the starting or failed state, or the
/// transcript and composer. `focus` marks the region for `F6`.
pub(crate) fn chat_panel(
    view: &WorkspaceView,
    focus: &FocusHandle,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let ws = view.workspace();
    let border = cx.theme().border;
    let bg = cx.theme().background;
    let body = if ws.chat_list().is_open() {
        history_body(view, cx)
    } else {
        match ws.chat() {
            None => empty(view, cx),
            Some(chat) => match chat.phase() {
                SessionPhase::Starting => fitted(
                    empty_state(AppIcon::Sparkles, STARTING_TITLE, STARTING_LINE, cx),
                    cx,
                ),
                // A restored chat that cannot continue still shows what was said.
                SessionPhase::Failed(_) if !chat.entries().is_empty() => {
                    conversation(view, chat, cx)
                }
                SessionPhase::Failed(message) => fitted(
                    empty_state(AppIcon::CircleAlert, FAILED_TITLE, message.clone(), cx),
                    cx,
                ),
                SessionPhase::Ready | SessionPhase::Running { .. } => conversation(view, chat, cx),
            },
        }
    };
    div()
        .id("chat-panel")
        .debug_selector(|| "chat-panel".to_owned())
        .track_focus(focus)
        .size_full()
        .flex()
        .flex_col()
        .bg(bg)
        .border_l_1()
        .border_color(border)
        .child(header(view, cx))
        .child(body)
        .into_any_element()
}

/// The title, the connection and provider, the caps and the New chat, Cancel and Close buttons.
fn header(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let ws = view.workspace();
    let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
    let scale = ui_scale(cx);
    let chat = ws.chat();
    let mut info = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[3]))
        .text_size(px(scale.xs))
        .text_color(muted);
    if let Some(chat) = chat {
        info = info.child(chat.info().name.clone());
        if let Some(status) = chat.status() {
            info = info.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE[1]))
                    .child(Icon::new(AppIcon::Sparkles).text_color(muted))
                    .child(status.label()),
            );
        }
    }
    let caps = caps_label(&ws.ai().config().limits);
    let running = chat.is_some_and(AiSessionTab::is_running);
    let buttons = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[1]))
        .child(
            icon_button("chat-history", AppIcon::Clock, "Chat history").on_click(cx.listener(
                |view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::ToggleChatHistory, cx);
                },
            )),
        )
        .child(
            icon_button("chat-new", AppIcon::Plus, "Start a new chat").on_click(
                cx.listener(|view, _: &ClickEvent, _, cx| view.dispatch_ai(AiAction::NewChat, cx)),
            ),
        )
        .child(
            icon_button("chat-cancel", AppIcon::CircleStop, "Cancel the turn")
                .disabled(!running)
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::CancelChat, cx);
                })),
        )
        .child(
            icon_button("chat-close", AppIcon::X, "Close the chat panel").on_click(cx.listener(
                |view, _: &ClickEvent, _, cx| view.dispatch_ai(AiAction::ToggleChat, cx),
            )),
        );
    let title = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(scale.sm))
                .child(CHAT_TITLE),
        )
        .child(buttons);
    div()
        .id("chat-header")
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .px(px(SPACE[4]))
        .py(px(SPACE[2]))
        .border_b_1()
        .border_color(border)
        .child(title)
        .child(info)
        .child(
            div()
                .id("chat-caps")
                .text_size(px(scale.xs))
                .text_color(muted)
                .child(caps),
        )
        .into_any_element()
}

/// The empty state: why a chat cannot start, or the Start chat button.
fn empty(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let (line, label, action) = match view.workspace().chat_gate() {
        Err(NO_PROVIDER) => (NO_PROVIDER, "Open AI settings", AiAction::OpenSettings),
        Err(SESSION_AI_OFF) => (SESSION_AI_OFF, "Turn on AI", AiAction::ToggleConnection),
        Err(other) => (other, "Start chat", AiAction::NewChat),
        Ok(()) => (CHAT_HELLO, "Start chat", AiAction::NewChat),
    };
    let button = Button::new("chat-start")
        .label(label)
        .primary()
        .small()
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_ai(action.clone(), cx);
        }));
    fitted(
        empty_state(AppIcon::MessageSquare, CHAT_TITLE, line, cx).child(button),
        cx,
    )
}

/// An empty state of the chat panel, sized to the panel so its text wraps at any width.
fn fitted(content: Div, cx: &Context<WorkspaceView>) -> AnyElement {
    centred(
        content
            .w_full()
            .min_w_0()
            .debug_selector(|| "chat-empty".to_owned()),
        cx,
    )
}

/// The transcript, the other-connection note and the composer.
fn conversation(
    view: &WorkspaceView,
    chat: &AiSessionTab,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let size = ui_scale(cx).sm;
    let mut column = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .px(px(SPACE[4]))
        .py(px(SPACE[3]));
    if chat.entries().is_empty() && chat.streaming().is_empty() {
        column = column.child(
            div()
                .text_size(px(size))
                .text_color(muted)
                .child(CHAT_HELLO),
        );
    }
    if chat.is_restored() {
        column = column.child(
            div()
                .id("chat-restored-note")
                .text_size(px(ui_scale(cx).xs))
                .text_color(muted)
                .child(RESTORED_NOTE),
        );
    }
    if chat.dropped() > 0 {
        column = column.child(
            div()
                .text_size(px(ui_scale(cx).xs))
                .text_color(muted)
                .child(DROPPED_MARKER),
        );
    }
    for (index, entry) in chat.entries().iter().enumerate() {
        if let Some(row) = entry_row(chat.dropped() + index, entry, cx) {
            column = column.child(row);
        }
    }
    if !chat.streaming().is_empty() {
        column = column.child(streaming_row(chat.streaming(), cx));
    }
    let mut scroll = div()
        .id("chat-transcript")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(column);
    if let Some(handle) = view.ai_scroll(TabId::CHAT_PANEL) {
        scroll = scroll.track_scroll(handle);
    }
    let mut body = div().flex_1().min_h_0().flex().flex_col().child(scroll);
    if let Some(note) = other_connection(view, chat, cx) {
        body = body.child(note);
    }
    if let SessionPhase::Failed(reason) = chat.phase() {
        return body.child(cannot_continue(reason, cx)).into_any_element();
    }
    body.child(composer(view, TabId::CHAT_PANEL, chat, cx))
        .into_any_element()
}

/// The note and the New chat button that replace the composer of a restored chat that cannot
/// continue.
fn cannot_continue(reason: &str, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
    div()
        .id("chat-cannot-continue")
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .px(px(SPACE[4]))
        .py(px(SPACE[3]))
        .border_t_1()
        .border_color(border)
        .text_size(px(ui_scale(cx).xs))
        .text_color(muted)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(format!("{HISTORY_CANNOT_CONTINUE} {reason}")),
        )
        .child(
            Button::new("chat-failed-new")
                .label("New chat")
                .primary()
                .small()
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::NewChat, cx);
                })),
        )
        .into_any_element()
}

/// The line and button shown when the active connection is not the chat's.
fn other_connection(
    view: &WorkspaceView,
    chat: &AiSessionTab,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let target = view.workspace().ai_target()?;
    if Some(&target) == chat.conn() {
        return None;
    }
    let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
    Some(
        div()
            .id("chat-other-connection")
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE[2]))
            .px(px(SPACE[4]))
            .py(px(SPACE[2]))
            .border_t_1()
            .border_color(border)
            .text_size(px(ui_scale(cx).xs))
            .text_color(muted)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(other_connection_text(&chat.info().name)),
            )
            .child(
                Button::new("chat-other-new")
                    .label("New chat")
                    .small()
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                        view.dispatch_ai(AiAction::NewChat, cx);
                    })),
            )
            .into_any_element(),
    )
}

/// One transcript entry; `None` for an entry kind this view does not know.
fn entry_row(
    index: usize,
    entry: &TranscriptEntry,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let size = ui_scale(cx).sm;
    let muted = cx.theme().muted_foreground;
    Some(match entry {
        TranscriptEntry::User { text } => user_row(text, cx),
        TranscriptEntry::Assistant { text } => assistant_row(index, text, cx),
        TranscriptEntry::ToolCall(call) => {
            let row = tool_row(call, chat_tool_status_text, cx);
            if refused(&call.status) {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE[1]))
                    .child(row)
                    .child(
                        div()
                            .id(("chat-refused", index))
                            .text_size(px(ui_scale(cx).xs))
                            .text_color(muted)
                            .child(CHAT_REFUSED_NOTE),
                    )
                    .into_any_element()
            } else {
                row
            }
        }
        TranscriptEntry::Sql(sql) => sql_card(index, sql, cx),
        TranscriptEntry::Notice(notice) => div()
            .text_size(px(size))
            .text_color(muted)
            .child(notice_text(notice))
            .into_any_element(),
        _ => return None,
    })
}

/// Whether the guard refused the call because the chat is read-only.
fn refused(status: &ToolCallStatus) -> bool {
    matches!(status, ToolCallStatus::Error { code, .. } if code == "read_only" || code == "no_approver")
}

fn user_row(text: &str, cx: &Context<WorkspaceView>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .text_size(px(ui_scale(cx).sm))
        .child(
            div()
                .text_size(px(ui_scale(cx).xs))
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().muted_foreground)
                .child("You"),
        )
        .child(text.to_owned())
        .into_any_element()
}

/// An answer, with a Copy and an Open in editor button under each SQL block it contains.
fn assistant_row(index: usize, text: &str, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let mut column = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .text_size(px(ui_scale(cx).sm))
        .child(text.to_owned());
    for (block, sql) in chat_sql_blocks(text).into_iter().enumerate() {
        column = column.child(sql_block(index, block, &sql, None, cx));
    }
    column.into_any_element()
}

/// A statement that ran: its SQL, the footer, Copy and Open in editor. Never Re-run.
fn sql_card(index: usize, entry: &SqlEntry, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let footer = (sql_footer(entry), entry.failed);
    sql_block(index, 0, &entry.display_sql, Some(footer), cx)
}

/// A SQL text in a card with Copy and Open in editor; `footer` is the line under a statement
/// that ran, with whether it failed.
fn sql_block(
    entry: usize,
    block: usize,
    sql: &str,
    footer: Option<(String, bool)>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (border, muted, radius, code_bg, danger) = (
        theme.border,
        theme.muted_foreground,
        theme.radius,
        theme.muted,
        theme.danger,
    );
    let mono = theme.mono_font_family.clone();
    let key = entry * 16 + block;
    let copy = toolbar_button("chat-copy", AppIcon::Copy, "Copy", "Copy this SQL").on_click(
        cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_ai(AiAction::CopyChatSql { entry, block }, cx);
        }),
    );
    let open = toolbar_button(
        "chat-open",
        AppIcon::FileCode,
        "Open in editor",
        "Open this SQL in a new editor tab without running it",
    )
    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
        view.dispatch_ai(AiAction::OpenChatSql { entry, block }, cx);
    }));
    let (line, failed) = footer.unwrap_or_default();
    div()
        .id(("chat-sql", key))
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .p(px(SPACE[3]))
        .rounded(radius)
        .border_1()
        .border_color(border)
        .child(code(sql, mono, scale.mono, radius, code_bg))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE[2]))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(scale.xs))
                        .text_color(if failed { danger } else { muted })
                        .child(line),
                )
                .child(copy)
                .child(open),
        )
        .into_any_element()
}

fn code(
    sql: &str,
    mono: gpui::SharedString,
    size: f32,
    radius: gpui::Pixels,
    bg: gpui::Hsla,
) -> Div {
    div()
        .p(px(SPACE[3]))
        .rounded(radius)
        .bg(bg)
        .font_family(mono)
        .text_size(px(size))
        .child(sql.to_owned())
}
