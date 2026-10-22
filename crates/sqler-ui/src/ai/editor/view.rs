// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the editor AI actions (ADR-0052): the prompt bar under the query toolbar
//! and the body of the Assistant view in the bottom panel.
//!
//! Both only render the [`AssistState`] of a tab. Every button dispatches an `AiAction`; nothing
//! here runs SQL.

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Escape as InputEscape, Input};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use sqler_config::contains_redaction;

use super::{
    AssistKind, AssistOutcome, AssistState, DiffTone, NOT_APPLICABLE_REDACTED, NOT_APPLICABLE_SAME,
    REDACTED_SQL, diff_lines, status_line,
};
use crate::ai::AiAction;
use crate::tabs::TabId;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, icon_button};
use crate::workspace::chrome::{ICON_MD, ICON_SM, TOOLBAR_HEIGHT, ui_scale};
use crate::workspace::{Effect, WorkspaceView};

/// The line under a generated statement that went into the editor.
const NOT_INSERTED: &str = "Not inserted: the editor was not available. Copy it from here.";
const INSERTED: &str = "Inserted into the editor. Review it, then run it.";
/// The line under a fix.
const FIX_NOTE: &str = "Accepting replaces the statement in the editor. Nothing runs.";
/// The line after an accepted fix.
const FIX_APPLIED: &str = "Fix applied to the editor. Run it when ready.";

/// The running line: a spinner, what is happening, and Cancel.
fn running_row(
    kind: AssistKind,
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> Div {
    let status = view.workspace().status_bar(None).ai;
    let text = status_line(kind, status.as_ref());
    let theme = cx.theme();
    let (muted, size) = (theme.muted_foreground, ui_scale(cx).sm);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .child(
            Spinner::new()
                .icon(AppIcon::LoaderCircle)
                .color(muted)
                .xsmall(),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(size))
                .text_color(muted)
                .child(text),
        )
        .child(
            Button::new(("assist-cancel", tab.raw()))
                .label("Cancel")
                .ghost()
                .small()
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::CancelAssist, cx);
                })),
        )
}

/// A one-line note in the muted or the danger colour.
/// The note under a Generate answer, and whether it is a warning.
fn sql_note(sql: &str, inserted: bool) -> (SharedString, bool) {
    if inserted {
        (SharedString::from(INSERTED), false)
    } else if contains_redaction(sql) {
        (SharedString::from(REDACTED_SQL), true)
    } else {
        (SharedString::from(NOT_INSERTED), false)
    }
}

fn note_line(text: impl Into<SharedString>, danger: bool, cx: &Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    div()
        .flex_1()
        .min_w_0()
        .text_size(px(ui_scale(cx).sm))
        .text_color(if danger {
            theme.danger
        } else {
            theme.muted_foreground
        })
        .child(text.into())
}

/// The "AI fix" button of failed statement `position` of `tab`: it dispatches `FixStatement` and
/// is disabled, with the reason as its tooltip, when the gate is closed.
pub(crate) fn fix_button(
    id: impl Into<gpui::ElementId>,
    view: &WorkspaceView,
    tab: TabId,
    position: usize,
    cx: &mut Context<WorkspaceView>,
) -> Button {
    let reason = view.workspace().ai_toolbar(tab).fix;
    Button::new(id)
        .label("AI fix")
        .icon(AppIcon::Wrench)
        .ghost()
        .small()
        .disabled(reason.is_some())
        .tooltip(reason.unwrap_or("Ask the AI to fix this statement"))
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_ai(AiAction::FixStatement(position), cx);
        }))
}

/// The prompt bar of the query tab `tab`: a `TOOLBAR_HEIGHT` row under the toolbar. `None` while
/// the bar is closed.
pub(crate) fn prompt_bar(
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let assist = view.workspace().assist(tab)?;
    assist.prompt.as_ref()?;
    let theme = cx.theme();
    let (bg, border, muted) = (theme.background, theme.border, theme.muted_foreground);
    let row = div()
        .id(("assist-prompt", tab.raw()))
        .role(Role::Group)
        .aria_label("AI prompt")
        .w_full()
        .h(px(TOOLBAR_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .px(px(SPACE[4]))
        .bg(bg)
        .border_b_1()
        .border_color(border);
    let content = match &assist.state {
        AssistState::Running {
            kind: AssistKind::Generate,
            ..
        } => row
            .child(
                Icon::new(AppIcon::Sparkles)
                    .size(px(ICON_MD))
                    .text_color(muted),
            )
            .child(running_row(AssistKind::Generate, view, tab, cx).flex_1()),
        state => {
            let mut row = row
                .capture_action(cx.listener(|view, _: &InputEscape, _, cx| {
                    view.dispatch_ai(AiAction::ClosePrompt, cx);
                    cx.stop_propagation();
                }))
                .child(
                    Icon::new(AppIcon::Sparkles)
                        .size(px(ICON_MD))
                        .text_color(muted),
                );
            if let Some(input) = view.assist_input(tab) {
                row = row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(input).appearance(false).cleanable(false)),
                );
            }
            let row = match state {
                AssistState::Done(AssistOutcome::Sql { sql, inserted, .. }) => {
                    let (text, danger) = sql_note(sql, *inserted);
                    row.child(note_line(text, danger, cx).flex_none())
                }
                AssistState::Failed {
                    kind: AssistKind::Generate,
                    message,
                } => row.child(note_line(message.clone(), true, cx).flex_none()),
                AssistState::Cancelled(AssistKind::Generate) => {
                    row.child(note_line(super::CANCELLED, false, cx).flex_none())
                }
                _ => row,
            };
            row.child(
                Button::new(("assist-generate", tab.raw()))
                    .label("Generate")
                    .primary()
                    .small()
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                        view.dispatch_ai(AiAction::SubmitPrompt, cx);
                    })),
            )
            .child(
                icon_button(("assist-close", tab.raw()), AppIcon::X, "Close the prompt")
                    .accessibility_label("Close the prompt")
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                        view.dispatch_ai(AiAction::ClosePrompt, cx);
                    })),
            )
        }
    };
    Some(content.into_any_element())
}

/// SQL in the mono font on a muted block.
fn code_block(sql: &str, cx: &Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    div()
        .w_full()
        .px(px(SPACE[3]))
        .py(px(SPACE[2]))
        .rounded(theme.radius)
        .bg(theme.muted)
        .font_family(theme.mono_font_family.clone())
        .text_size(px(ui_scale(cx).sm))
        .text_color(theme.foreground)
        .child(sql.to_owned())
}

/// A paragraph of body text.
fn paragraph(text: &str, cx: &Context<WorkspaceView>) -> Div {
    div()
        .text_size(px(ui_scale(cx).sm))
        .text_color(cx.theme().foreground)
        .child(text.to_owned())
}

/// The scrolling column every answer sits in.
fn column(id: &'static str) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .px(px(SPACE[4]))
        .py(px(SPACE[3]))
}

/// The first non-blank line of `sql`, for a header.
fn first_line(sql: &str) -> &str {
    sql.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// A header line: the first SQL line in mono and the Copy button.
fn header(id: &'static str, sql: &str, copy: String, cx: &mut Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    let (mono, muted) = (theme.mono_font_family.clone(), theme.muted_foreground);
    let size = ui_scale(cx).sm;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .font_family(mono)
                .text_size(px(size))
                .text_color(muted)
                .child(first_line(sql).to_owned()),
        )
        .child(
            Button::new(id)
                .label("Copy")
                .icon(AppIcon::Copy)
                .ghost()
                .small()
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.apply(Effect::Copy(copy.clone()), cx);
                })),
        )
}

/// The diff of a fix, one mono row per line.
fn diff_block(diff: &str, cx: &Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    let (success, danger, muted, fg) = (
        theme.success,
        theme.danger,
        theme.muted_foreground,
        theme.foreground,
    );
    let mut block = div()
        .w_full()
        .py(px(SPACE[1]))
        .rounded(theme.radius)
        .bg(theme.muted)
        .font_family(theme.mono_font_family.clone())
        .text_size(px(ui_scale(cx).sm))
        .flex()
        .flex_col();
    for (tone, line) in diff_lines(diff) {
        let (color, tint) = match tone {
            DiffTone::Added => (success, Some(success)),
            DiffTone::Removed => (danger, Some(danger)),
            DiffTone::Header => (muted, None),
            DiffTone::Context => (fg, None),
        };
        let mut row = div()
            .px(px(SPACE[3]))
            .whitespace_nowrap()
            .text_color(color)
            .child(if line.is_empty() { " " } else { line }.to_owned());
        if let Some(tint) = tint {
            row = row.bg(tint.opacity(0.12));
        }
        block = block.child(row);
    }
    block
}

/// The accept and reject buttons of a fix.
fn fix_buttons(
    tab: TabId,
    applicable: bool,
    unchanged: bool,
    cx: &mut Context<WorkspaceView>,
) -> Div {
    let tooltip = if applicable {
        None
    } else if unchanged {
        Some(NOT_APPLICABLE_SAME)
    } else {
        Some(NOT_APPLICABLE_REDACTED)
    };
    let mut accept = Button::new(("assist-accept", tab.raw()))
        .label("Accept fix")
        .primary()
        .small()
        .disabled(!applicable)
        .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
            view.dispatch_ai(AiAction::AcceptFix, cx);
        }));
    if let Some(tooltip) = tooltip {
        accept = accept.tooltip(tooltip);
    }
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .child(accept)
        .child(
            Button::new(("assist-reject", tab.raw()))
                .label("Reject")
                .outline()
                .small()
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::RejectFix, cx);
                })),
        )
}

/// A centred message: the empty state of the Assistant view.
fn centred(content: impl gpui::IntoElement) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(content)
        .into_any_element()
}

/// The body of the Assistant view for the active tab.
pub(crate) fn assistant_body(
    view: &WorkspaceView,
    tab: Option<TabId>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let idle = || {
        centred(empty_state(
            AppIcon::Sparkles,
            "No AI answer yet",
            "Use AI: Explain Query or AI: Fix Failed Query in a SQL editor.",
            cx,
        ))
    };
    let Some(tab) = tab else { return idle() };
    let Some(assist) = view.workspace().assist(tab) else {
        return idle();
    };
    match &assist.state {
        AssistState::Idle => idle(),
        AssistState::Running { kind, .. } => {
            let kind = *kind;
            column("assistant-running")
                .child(running_row(kind, view, tab, cx))
                .into_any_element()
        }
        AssistState::Done(AssistOutcome::Explanation { sql, text }) => column("assistant-explain")
            .child(header("assistant-copy", sql, text.clone(), cx))
            .child(paragraph(text, cx))
            .into_any_element(),
        AssistState::Done(AssistOutcome::Sql {
            sql,
            rationale,
            inserted,
            ..
        }) => {
            let (note, danger) = sql_note(sql, *inserted);
            column("assistant-sql")
                .child(header("assistant-copy", sql, sql.clone(), cx))
                .child(code_block(sql, cx))
                .child(paragraph(rationale, cx))
                .child(div().flex().child(note_line(note, danger, cx)))
                .into_any_element()
        }
        AssistState::Done(AssistOutcome::Fix {
            sql,
            diff,
            rationale,
            applicable,
            unchanged,
            ..
        }) => column("assistant-fix")
            .child(paragraph(rationale, cx))
            .child(diff_block(diff, cx))
            .child(fix_buttons(tab, *applicable, *unchanged, cx))
            .child(div().flex().child(note_line(FIX_NOTE, false, cx)))
            .child(header("assistant-copy", sql, sql.clone(), cx))
            .into_any_element(),
        AssistState::FixApplied => column("assistant-applied")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE[2]))
                    .child(
                        Icon::new(AppIcon::Check)
                            .size(px(ICON_SM))
                            .text_color(cx.theme().success),
                    )
                    .child(paragraph(FIX_APPLIED, cx).font_weight(FontWeight::MEDIUM)),
            )
            .into_any_element(),
        AssistState::Failed { message, .. } => column("assistant-failed")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE[2]))
                    .child(
                        Icon::new(AppIcon::CircleAlert)
                            .size(px(ICON_SM))
                            .text_color(cx.theme().danger),
                    )
                    .child(note_line(message.clone(), true, cx)),
            )
            .into_any_element(),
        AssistState::Cancelled(_) => column("assistant-cancelled")
            .child(note_line(super::CANCELLED, false, cx))
            .into_any_element(),
    }
}
