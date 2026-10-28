// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The body of an AI session tab (ADR-0053): the toolbar, the transcript, the approval card and
//! the composer. It only renders [`AiSessionTab`]; every button dispatches an [`AiAction`].

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Textarea};
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use sqler_ai_tools::session::{SqlEntry, ToolCallEntry, ToolCallStatus, TranscriptEntry};

use super::{AiSessionTab, SessionPhase, notice_text, sql_footer, state_label, tool_status_text};
use crate::ai::action::AiAction;
use crate::ai::approval::view::approval_card;
use crate::tabs::TabId;
use crate::tabs::view::editor_toolbar;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, toolbar_button};
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::ui_scale;

/// The title of the empty state while the session is built.
const STARTING_TITLE: &str = "Starting the session\u{2026}";
/// The line under it.
const STARTING_LINE: &str = "The model is being connected to the database tools.";
/// The title of the empty state when the session could not be built.
const FAILED_TITLE: &str = "The session could not start";
/// The first line of an empty transcript.
const HELLO_LINE: &str =
    "Ask about the data. Statements that change data always wait for your approval.";

/// The tab body for the AI session `tab`.
pub(crate) fn session_tab_body(
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let ws = view.workspace();
    let Some(session) = ws.ai_session(tab) else {
        return div().flex_1().into_any_element();
    };
    let toolbar = toolbar(view, tab, session, cx);
    let body = match session.phase() {
        SessionPhase::Starting => centred(
            empty_state(AppIcon::Sparkles, STARTING_TITLE, STARTING_LINE, cx),
            cx,
        ),
        SessionPhase::Failed(message) => centred(
            empty_state(AppIcon::CircleAlert, FAILED_TITLE, message.clone(), cx),
            cx,
        ),
        SessionPhase::Ready | SessionPhase::Running { .. } => chat(view, tab, session, cx),
    };
    div()
        .id("ai-session")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(toolbar)
        .child(body)
        .into_any_element()
}

/// An empty state in the middle of the tab.
pub(crate) fn centred(content: Div, cx: &Context<WorkspaceView>) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .px(px(SPACE[4]))
        .text_size(px(ui_scale(cx).sm))
        .child(content.text_center())
        .into_any_element()
}

/// Breadcrumbs, the provider chip, the state while the model works and Cancel.
fn toolbar(
    view: &WorkspaceView,
    tab: TabId,
    session: &AiSessionTab,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let size = ui_scale(cx).sm;
    let mut status_line = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[3]))
        .text_size(px(size))
        .text_color(muted);
    if let Some(status) = session.status() {
        status_line = status_line.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE[1]))
                .child(Icon::new(AppIcon::Sparkles).text_color(muted))
                .child(status.label()),
        );
    }
    let mut actions: Vec<AnyElement> = vec![status_line.into_any_element()];
    actions.push(
        toolbar_button(
            "ai-session-cancel",
            AppIcon::CircleStop,
            "Cancel",
            "Cancel the turn",
        )
        .disabled(!session.is_running())
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_ai(AiAction::CancelSession(tab), cx);
        }))
        .into_any_element(),
    );
    let crumbs = view.workspace().breadcrumbs(tab);
    editor_toolbar("ai-session-toolbar", crumbs.as_ref(), actions, cx).into_any_element()
}

/// The transcript, the approval card and the composer.
fn chat(
    view: &WorkspaceView,
    tab: TabId,
    session: &AiSessionTab,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let mut column = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .px(px(SPACE[4]))
        .py(px(SPACE[3]));
    if session.entries().is_empty() && session.streaming().is_empty() {
        column = column.child(
            div()
                .text_size(px(ui_scale(cx).sm))
                .text_color(cx.theme().muted_foreground)
                .child(HELLO_LINE),
        );
    }
    if session.dropped() > 0 {
        column = column.child(
            div()
                .text_size(px(ui_scale(cx).xs))
                .text_color(cx.theme().muted_foreground)
                .child(crate::ai::session::DROPPED_MARKER),
        );
    }
    for (index, entry) in session.entries().iter().enumerate() {
        if let Some(row) = entry_row(tab, session.dropped() + index, entry, cx) {
            column = column.child(row);
        }
    }
    if !session.streaming().is_empty() {
        column = column.child(streaming_row(session.streaming(), cx));
    }
    if let Some(ask) = session
        .approvals()
        .current()
        .filter(|ask| ask.request.stage == sqler_ai_tools::ApprovalStage::Confirm)
    {
        column = column.child(approval_card(view, tab, ask, cx));
    }
    let mut scroll = div()
        .id("ai-transcript")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(column);
    if let Some(handle) = view.ai_scroll(tab) {
        scroll = scroll.track_scroll(handle);
    }
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(scroll)
        .child(composer(view, tab, session, cx))
        .into_any_element()
}

/// One transcript entry; `None` for an entry kind this view does not know.
fn entry_row(
    tab: TabId,
    index: usize,
    entry: &TranscriptEntry,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let size = ui_scale(cx).sm;
    let muted = cx.theme().muted_foreground;
    Some(match entry {
        TranscriptEntry::User { text } => div()
            .flex()
            .flex_col()
            .gap(px(SPACE[1]))
            .text_size(px(size))
            .child(
                div()
                    .text_size(px(ui_scale(cx).xs))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .child("You"),
            )
            .child(text.clone())
            .into_any_element(),
        TranscriptEntry::Assistant { text } => div()
            .text_size(px(size))
            .child(text.clone())
            .into_any_element(),
        TranscriptEntry::ToolCall(call) => tool_row(call, tool_status_text, cx),
        TranscriptEntry::Sql(sql) => sql_card(tab, index, sql, cx),
        TranscriptEntry::Notice(notice) => div()
            .text_size(px(size))
            .text_color(muted)
            .child(notice_text(notice))
            .into_any_element(),
        _ => return None,
    })
}

/// The tool name and how the call ended, worded by `status_text`.
pub(crate) fn tool_row(
    call: &ToolCallEntry,
    status_text: fn(&ToolCallStatus) -> String,
    cx: &Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let color = match &call.status {
        ToolCallStatus::Error { .. } => theme.danger,
        ToolCallStatus::Cancelled => theme.warning,
        _ => theme.muted_foreground,
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .text_size(px(scale.sm))
        .text_color(theme.muted_foreground)
        .child(Icon::new(AppIcon::Sparkles).text_color(theme.muted_foreground))
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_size(px(scale.mono))
                .child(call.tool.clone()),
        )
        .child(div().text_color(color).child(status_text(&call.status)))
        .into_any_element()
}

/// A statement that ran: its SQL, the footer and the two ghost buttons.
fn sql_card(
    tab: TabId,
    index: usize,
    entry: &SqlEntry,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (border, muted, radius, code_bg) = (
        theme.border,
        theme.muted_foreground,
        theme.radius,
        theme.muted,
    );
    let footer_color = if entry.failed { theme.danger } else { muted };
    let mono = theme.mono_font_family.clone();
    let rerun = toolbar_button(
        "ai-rerun",
        AppIcon::Play,
        "Re-run",
        "Run this statement again",
    )
    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
        view.dispatch_ai(AiAction::RerunSql(tab, index), cx);
    }));
    let open = toolbar_button(
        "ai-open-in-editor",
        AppIcon::FileCode,
        "Open in editor",
        "Open this statement in a new editor tab",
    )
    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
        view.dispatch_ai(AiAction::OpenSqlInEditor(tab, index), cx);
    }));
    div()
        .id(("ai-sql", index))
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .p(px(SPACE[3]))
        .rounded(radius)
        .border_1()
        .border_color(border)
        .child(
            div()
                .p(px(SPACE[3]))
                .rounded(radius)
                .bg(code_bg)
                .font_family(mono)
                .text_size(px(scale.mono))
                .child(entry.display_sql.clone()),
        )
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
                        .text_color(footer_color)
                        .child(sql_footer(entry)),
                )
                .child(rerun)
                .child(open),
        )
        .into_any_element()
}

/// The answer the model is writing now.
pub(crate) fn streaming_row(text: &str, cx: &Context<WorkspaceView>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(SPACE[2]))
        .text_size(px(ui_scale(cx).sm))
        .child(div().flex_1().min_w_0().child(text.to_owned()))
        .child(Icon::new(AppIcon::LoaderCircle).text_color(muted))
        .into_any_element()
}

/// The composer row: the field takes the width left, the send element keeps its size and stays
/// on the last line of a growing field. It never wraps.
pub(crate) fn composer_row(field: impl IntoElement, send: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .items_end()
        .gap(px(SPACE[2]))
        .child(
            div()
                .flex_1()
                .min_w_0()
                // The field emits `PressEnter` and lets a plain Enter propagate; where the
                // platform then types the key's character, that would be a newline. Handling
                // the action here ends the dispatch, so only Shift+Enter inserts one.
                .on_action(|_: &Enter, _, _| {})
                .child(field),
        )
        .child(div().flex_none().child(send))
}

/// The input, the Send button and the state line.
pub(crate) fn composer(
    view: &WorkspaceView,
    tab: TabId,
    session: &AiSessionTab,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (border, muted, bg) = (theme.border, theme.muted_foreground, theme.background);
    let size = ui_scale(cx).xs;
    let field = match view.ai_composer(tab) {
        Some(input) => Textarea::new(input).into_any_element(),
        None => div().flex_1().into_any_element(),
    };
    let send = Button::new("ai-send")
        .label("Send")
        .primary()
        .small()
        .disabled(session.is_running())
        .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
            view.ai_send(tab, window, cx);
        }));
    let row = composer_row(field, send).debug_selector(|| "ai-composer".to_owned());
    let line = if session.is_running() {
        state_label(session.state())
    } else {
        ""
    };
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .px(px(SPACE[4]))
        .py(px(SPACE[3]))
        .bg(bg)
        .border_t_1()
        .border_color(border)
        .child(row)
        .child(
            div()
                .h(px(ui_scale(cx).xs * 1.5))
                .text_size(px(size))
                .text_color(muted)
                .child(line),
        )
        .into_any_element()
}
