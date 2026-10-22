// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The body of an agent tab (ADR-0051): the toolbar, the picker, the transcript, the statement
//! card, the permission card, the failure card and the composer. It only renders [`AgentTab`];
//! every button dispatches an [`AiAction`] that names the ask it answers. The default focus of
//! both cards is the button that refuses, so a stray Enter never approves, and the second
//! confirmation on a production connection is the run gate overlay, never a button here.

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use sqler_acp::{PermissionOptionKind, PlanEntry, ToolCallStatus};

use crate::ai::AiAction;
use crate::ai::agent::{
    AgentEntry, AgentFailure, AgentPhase, AgentTab, Chunked, DROPPED_MARKER, PendingPermission,
    ToolEntry,
};
use crate::ai::approval::view::approval_card;
use crate::tabs::TabId;
use crate::tabs::view::editor_toolbar;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, env_chip, toolbar_button};
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{LIST_MAX_H, RowState, list_row, ui_scale};

/// The title of the empty state while the agent starts.
const STARTING_TITLE: &str = "Starting the agent\u{2026}";
/// The line under it.
const STARTING_LINE: &str = "The agent is being connected to SQLer's database tools.";
/// The title of the empty state while the agent list loads.
const LOADING_TITLE: &str = "Loading the agents\u{2026}";
/// The first line of an empty transcript.
const HELLO_LINE: &str =
    "Ask the agent about the data. Statements that change data always wait for your approval.";
/// The line above the picker.
const PICK_LINE: &str =
    "Choose the agent that works on this connection. Add your own in agents.toml.";
/// The failure title when the agent could not start.
const FAILED_TITLE: &str = "The agent could not start";
/// The failure title when the agent ended.
const STOPPED_TITLE: &str = "The agent stopped";
/// The hint under an install command.
const PATH_HINT: &str =
    "Installed but not found? Set an absolute `command` for the agent in agents.toml.";
/// The note under a permission request.
const PERMISSION_NOTE: &str = "The agent waits for your answer.";
/// The mark of text that was cut at the limit.
const CUT_MARK: &str = "(shortened)";

/// The tab body for the agent tab `tab`.
pub(crate) fn agent_tab_body(
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let Some(agent) = view.workspace().agent(tab) else {
        return div().flex_1().into_any_element();
    };
    let toolbar = toolbar(view, tab, agent, cx);
    let body = match agent.phase() {
        AgentPhase::Picking => picker(view, tab, cx),
        AgentPhase::Starting => centred(
            empty_state(AppIcon::Bot, STARTING_TITLE, STARTING_LINE, cx),
            cx,
        ),
        AgentPhase::Failed(failure) => centred(failure_card(failure, FAILED_TITLE, cx), cx),
        AgentPhase::Ready | AgentPhase::Running { .. } | AgentPhase::Stopped(_) => {
            chat(view, tab, agent, cx)
        }
    };
    div()
        .id("agent-tab")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(toolbar)
        .child(body)
        .into_any_element()
}

/// Something in the middle of the tab.
fn centred(content: impl gpui::IntoElement, cx: &Context<WorkspaceView>) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .px(px(SPACE[4]))
        .text_size(px(ui_scale(cx).sm))
        .child(content)
        .into_any_element()
}

/// Breadcrumbs, the state line, Cancel while a turn runs and Restart when the agent ended.
fn toolbar(
    view: &WorkspaceView,
    tab: TabId,
    agent: &AgentTab,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let size = ui_scale(cx).sm;
    let status_line = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[1]))
        .text_size(px(size))
        .text_color(muted)
        .child(Icon::new(AppIcon::Bot).text_color(muted))
        .child(agent.label());
    let mut actions: Vec<AnyElement> = vec![status_line.into_any_element()];
    if agent.is_running() {
        actions.push(
            toolbar_button(
                "agent-cancel",
                AppIcon::CircleStop,
                "Cancel",
                "Cancel the turn",
            )
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.dispatch_ai(AiAction::CancelAgent(tab), cx);
            }))
            .into_any_element(),
        );
    }
    if matches!(
        agent.phase(),
        AgentPhase::Stopped(_) | AgentPhase::Failed(_)
    ) {
        actions.push(
            toolbar_button(
                "agent-restart",
                AppIcon::RefreshCw,
                "Restart",
                "Start the agent again",
            )
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.dispatch_ai(AiAction::RestartAgent(tab), cx);
            }))
            .into_any_element(),
        );
    }
    let crumbs = view.workspace().breadcrumbs(tab);
    editor_toolbar("agent-toolbar", crumbs.as_ref(), actions, cx).into_any_element()
}

/// The agents to choose from.
fn picker(view: &WorkspaceView, tab: TabId, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let Some(profiles) = view.workspace().agent_profiles() else {
        return centred(
            empty_state(AppIcon::Bot, LOADING_TITLE, PICK_LINE, cx).text_center(),
            cx,
        );
    };
    let (muted, mono) = (
        cx.theme().muted_foreground,
        cx.theme().mono_font_family.clone(),
    );
    let scale = ui_scale(cx);
    let mut list = div().flex().flex_col().gap(px(SPACE[1]));
    for profile in &profiles.list {
        let id = profile.id.clone();
        list = list.child(
            list_row(
                ("agent-pick", profile_key(&profile.id)),
                RowState::default(),
                cx,
            )
            .text_size(px(scale.md))
            .child(Icon::new(AppIcon::Bot).text_color(muted))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(profile.name.clone()),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(mono.clone())
                    .text_size(px(scale.mono))
                    .text_color(muted)
                    .child(profile.command.clone()),
            )
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.dispatch_ai(AiAction::PickAgent(tab, id.clone()), cx);
            })),
        );
    }
    let mut column = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .px(px(SPACE[4]))
        .py(px(SPACE[4]))
        .child(
            div()
                .text_size(px(scale.sm))
                .text_color(muted)
                .child(PICK_LINE),
        )
        .child(list);
    if let Some(error) = &profiles.error {
        column = column.child(
            div()
                .text_size(px(scale.sm))
                .text_color(cx.theme().danger)
                .child(error.clone()),
        );
    }
    div()
        .id("agent-picker")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(column)
        .into_any_element()
}

/// A stable numeric key for an element id from a profile id.
fn profile_key(id: &str) -> usize {
    id.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(usize::from(b))
    })
}

/// The transcript, the cards and the composer.
fn chat(
    view: &WorkspaceView,
    tab: TabId,
    agent: &AgentTab,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let mut column = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .px(px(SPACE[4]))
        .py(px(SPACE[3]));
    let transcript = agent.transcript();
    if transcript.entries().is_empty() {
        column = column.child(
            div()
                .text_size(px(ui_scale(cx).sm))
                .text_color(cx.theme().muted_foreground)
                .child(HELLO_LINE),
        );
    }
    if transcript.dropped() > 0 {
        column = column.child(
            div()
                .text_size(px(ui_scale(cx).xs))
                .text_color(cx.theme().muted_foreground)
                .child(DROPPED_MARKER),
        );
    }
    for (index, entry) in transcript.entries().iter().enumerate() {
        column = column.child(entry_row(index, entry, cx));
    }
    if let AgentPhase::Stopped(failure) = agent.phase() {
        column = column.child(failure_card(failure, STOPPED_TITLE, cx));
    }
    if let Some(ask) = agent
        .approvals()
        .current()
        .filter(|ask| ask.request.stage == sqler_ai_tools::ApprovalStage::Confirm)
    {
        // The card carries its exact SQL: palette commands may now answer this ask.
        agent.approvals().mark_shown(ask.id);
        column = column.child(approval_card(view, tab, ask, cx));
    }
    if let Some(ask) = agent.permissions().current() {
        agent.permissions().mark_shown(ask.id);
        column = column.child(permission_card(view, tab, agent, ask, cx));
    }
    let mut scroll = div()
        .id("agent-transcript")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(column);
    if let Some(handle) = view.agent_scroll(tab) {
        scroll = scroll.track_scroll(handle);
    }
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(scroll)
        .child(composer(view, tab, agent, cx))
        .into_any_element()
}

/// One transcript entry.
fn entry_row(index: usize, entry: &AgentEntry, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let size = ui_scale(cx).sm;
    let muted = cx.theme().muted_foreground;
    match entry {
        AgentEntry::User(text) => div()
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
            .child(chunked(text, cx))
            .into_any_element(),
        AgentEntry::Message(text) => div()
            .text_size(px(size))
            .child(chunked(text, cx))
            .into_any_element(),
        AgentEntry::Thought(text) => div()
            .text_size(px(size))
            .text_color(muted)
            .italic()
            .child(chunked(text, cx))
            .into_any_element(),
        AgentEntry::Tool(tool) => tool_row(index, tool, cx),
        AgentEntry::Plan(steps) => plan_card(steps, cx),
        AgentEntry::Notice(text) => div()
            .text_size(px(size))
            .text_color(muted)
            .child(text.clone())
            .into_any_element(),
    }
}

/// Chunked text, with the mark when it was cut.
fn chunked(text: &Chunked, cx: &Context<WorkspaceView>) -> Div {
    let mut out = div().child(text.text.clone());
    if text.cut {
        out = out.child(
            div()
                .text_size(px(ui_scale(cx).xs))
                .text_color(cx.theme().muted_foreground)
                .child(CUT_MARK),
        );
    }
    out
}

/// The words for where a tool call is.
fn tool_status_label(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "running",
        ToolCallStatus::Completed => "done",
        ToolCallStatus::Failed => "failed",
    }
}

/// A tool call: its title, the kind, how it stands and the text the agent attached.
fn tool_row(index: usize, tool: &ToolEntry, cx: &Context<WorkspaceView>) -> AnyElement {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let muted = theme.muted_foreground;
    let color = if tool.status == ToolCallStatus::Failed {
        theme.danger
    } else {
        muted
    };
    let mut head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .text_size(px(scale.sm))
        .text_color(muted)
        .child(Icon::new(AppIcon::Wrench).text_color(muted))
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_family(theme.mono_font_family.clone())
                .text_size(px(scale.mono))
                .child(tool.title.clone()),
        );
    if let Some(kind) = &tool.kind {
        head = head.child(div().text_size(px(scale.xs)).child(kind.clone()));
    }
    head = head.child(
        div()
            .text_color(color)
            .child(tool_status_label(tool.status)),
    );
    let mut row = div().flex().flex_col().gap(px(SPACE[1])).child(head);
    if !tool.text.is_empty() {
        row = row.child(
            div()
                .id(("agent-tool-text", index))
                .max_h(px(LIST_MAX_H))
                .overflow_y_scroll()
                .p(px(SPACE[2]))
                .rounded(theme.radius)
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_size(px(scale.mono))
                .text_color(muted)
                .child(tool.text.join("\n")),
        );
    }
    row.into_any_element()
}

/// The plan of the turn as a checklist.
fn plan_card(steps: &[PlanEntry], cx: &Context<WorkspaceView>) -> AnyElement {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let (muted, success) = (theme.muted_foreground, theme.success);
    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .p(px(SPACE[3]))
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .text_size(px(scale.xs))
                .font_weight(FontWeight::MEDIUM)
                .text_color(muted)
                .child("Plan"),
        );
    for step in steps {
        let (icon, color) = match step.status.as_str() {
            "completed" => (AppIcon::Check, success),
            "in_progress" => (AppIcon::LoaderCircle, theme.foreground),
            _ => (AppIcon::Ellipsis, muted),
        };
        let text_color = if step.status == "completed" {
            muted
        } else {
            theme.foreground
        };
        card = card.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE[2]))
                .text_size(px(scale.sm))
                .child(Icon::new(icon).text_color(color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(text_color)
                        .child(step.content.clone()),
                ),
        );
    }
    card.into_any_element()
}

/// The card for the permission request the user answers now. The options are in the order the
/// agent sent them; the first one that rejects holds the default focus.
fn permission_card(
    view: &WorkspaceView,
    tab: TabId,
    agent: &AgentTab,
    ask: &PendingPermission,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let id = ask.id;
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (border, muted, radius) = (theme.border, theme.muted_foreground, theme.radius);
    let (code_bg, warning, bg) = (theme.muted, theme.warning, theme.popover);
    let mono = theme.mono_font_family.clone();
    let call = &ask.request.tool_call;
    let title = call
        .title
        .clone()
        .unwrap_or_else(|| "The agent asks for permission".to_owned());
    let mut head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .text_size(px(scale.md))
        .font_weight(FontWeight::MEDIUM)
        .child(Icon::new(AppIcon::TriangleAlert).text_color(warning))
        .child(div().min_w_0().truncate().child(title));
    if let Some(env) = agent.info().environment {
        head = head.child(env_chip(env, cx));
    }
    let mut card = div()
        .id("agent-permission-card")
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .p(px(SPACE[4]))
        .rounded(radius)
        .border_1()
        .border_color(border)
        .bg(bg)
        .child(head);
    if !call.text.is_empty() {
        card = card.child(
            div()
                .id("agent-permission-text")
                .max_h(px(LIST_MAX_H))
                .overflow_y_scroll()
                .p(px(SPACE[3]))
                .rounded(radius)
                .bg(code_bg)
                .font_family(mono)
                .text_size(px(scale.mono))
                .child(call.text.join("\n")),
        );
    }
    let mut buttons = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .justify_end()
        .gap(px(SPACE[2]));
    let mut focus_taken = false;
    for (index, option) in ask.request.options.iter().enumerate() {
        let rejects = matches!(
            option.kind,
            PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
        );
        let option_id = option.id.clone();
        let button = Button::new(("agent-permission-option", index))
            .label(option.name.clone())
            .small();
        let button = if rejects {
            button
                .outline()
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::RejectAgentRequest(tab, id, option_id.clone()), cx);
                }))
        } else {
            button
                .primary()
                .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
                    if event.click_count() == 1 {
                        view.dispatch_ai(
                            AiAction::AllowAgentRequest(tab, id, option_id.clone()),
                            cx,
                        );
                    }
                }))
        };
        let mut slot = div().child(button);
        if rejects
            && !focus_taken
            && let Some(focus) = view.agent_reject_focus(tab)
        {
            focus_taken = true;
            slot = slot.track_focus(focus);
        }
        buttons = buttons.child(slot);
    }
    card.child(
        div()
            .text_size(px(scale.sm))
            .text_color(muted)
            .child(PERMISSION_NOTE),
    )
    .child(buttons)
    .into_any_element()
}

/// Why the agent did not start or stopped: the message, how to install it, what it wrote to
/// stderr.
fn failure_card(
    failure: &AgentFailure,
    title: &'static str,
    cx: &Context<WorkspaceView>,
) -> gpui::Stateful<Div> {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let (muted, radius) = (theme.muted_foreground, theme.radius);
    let mono = theme.mono_font_family.clone();
    let mut card = div()
        .id("agent-failure")
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .p(px(SPACE[4]))
        .rounded(radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE[2]))
                .text_size(px(scale.md))
                .font_weight(FontWeight::MEDIUM)
                .child(Icon::new(AppIcon::CircleAlert).text_color(theme.danger))
                .child(title),
        )
        .child(div().text_size(px(scale.sm)).child(failure.message.clone()));
    if let Some(hint) = &failure.install_hint {
        card = card
            .child(
                div()
                    .font_family(mono.clone())
                    .text_size(px(scale.mono))
                    .child(format!("Install: {hint}")),
            )
            .child(
                div()
                    .text_size(px(scale.sm))
                    .text_color(muted)
                    .child(PATH_HINT),
            );
    }
    if !failure.stderr_tail.is_empty() {
        card = card.child(
            div()
                .id("agent-stderr")
                .max_h(px(LIST_MAX_H))
                .overflow_y_scroll()
                .p(px(SPACE[3]))
                .rounded(radius)
                .bg(theme.muted)
                .font_family(mono)
                .text_size(px(scale.mono))
                .text_color(muted)
                .child(failure.stderr_tail.join("\n")),
        );
    }
    card
}

/// The input, the Send button and the state line.
fn composer(
    view: &WorkspaceView,
    tab: TabId,
    agent: &AgentTab,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (border, muted, bg) = (theme.border, theme.muted_foreground, theme.background);
    let size = ui_scale(cx).xs;
    let ready = agent.phase() == &AgentPhase::Ready;
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .child(match view.agent_composer(tab) {
            Some(input) => div().flex_1().min_w_0().child(Input::new(input)),
            None => div().flex_1(),
        })
        .child(
            Button::new("agent-send")
                .label("Send")
                .primary()
                .small()
                .disabled(!ready)
                .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                    view.agent_send(tab, window, cx);
                })),
        );
    let line = if agent.is_running() {
        agent.label()
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
