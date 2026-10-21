// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI side view: the AI state of the connection the user works on, the configured
//! providers and the way to the settings. Each section is a plain function so later AI surfaces
//! append one.

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _};

use super::{HubConnection, HubModel, ProviderRow, set_up_action};
use crate::ai::action::AiAction;
use crate::ai::indicator::AiStatus;
use crate::ai::kind_label;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, env_chip, section_header};
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::ui_scale;

/// The text of the empty state's line.
const EMPTY_LINE: &str = "Add a provider in Settings > AI. Ollama runs on this computer and is the default; nothing is sent until you turn AI on for a connection.";

/// The AI side view for the connection the user works on.
pub(crate) fn ai_view(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let model = view.workspace().hub_model();
    let theme = cx.theme();
    let (bg, fg, border) = (
        theme.sidebar,
        theme.sidebar_foreground,
        theme.sidebar_border,
    );
    let mut body = div().flex().flex_col().gap(px(SPACE[3])).pb(px(SPACE[4]));
    if let Some(error) = &model.load_error {
        body = body.child(notice(error.clone(), cx));
    }
    if model.providers.is_empty() {
        body = body.child(empty(view, cx));
    } else {
        body = body
            .child(connection_section(&model, cx))
            .child(providers_section(&model, cx))
            .child(buttons_section(cx));
    }
    div()
        .id("ai-view")
        .size_full()
        .flex()
        .flex_col()
        .bg(bg)
        .text_color(fg)
        .border_r_1()
        .border_color(border)
        .child(section_header("AI", cx))
        .child(
            div()
                .id("ai-view-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(body),
        )
        .into_any_element()
}

/// A muted caption above a section.
fn caption(title: &'static str, cx: &Context<WorkspaceView>) -> Div {
    div()
        .px(px(SPACE[4]))
        .text_size(px(ui_scale(cx).xs))
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().muted_foreground)
        .child(title)
}

/// A paragraph of muted text.
fn muted(text: impl Into<SharedString>, cx: &Context<WorkspaceView>) -> Div {
    div()
        .px(px(SPACE[4]))
        .text_size(px(ui_scale(cx).sm))
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// A load problem, in the danger colour.
fn notice(text: String, cx: &Context<WorkspaceView>) -> Div {
    div()
        .px(px(SPACE[4]))
        .text_size(px(ui_scale(cx).sm))
        .text_color(cx.theme().danger)
        .child(text)
}

/// One label and value line of the connection section.
fn fact(label: &'static str, value: impl Into<SharedString>, cx: &Context<WorkspaceView>) -> Div {
    let size = ui_scale(cx).sm;
    div()
        .px(px(SPACE[4]))
        .flex()
        .flex_row()
        .justify_between()
        .gap(px(SPACE[3]))
        .text_size(px(size))
        .child(div().text_color(cx.theme().muted_foreground).child(label))
        .child(div().min_w_0().child(value.into()))
}

/// A small outline button that runs an AI action.
fn action_button(
    id: &'static str,
    label: &'static str,
    action: AiAction,
    primary: bool,
    cx: &mut Context<WorkspaceView>,
) -> Button {
    let button = Button::new(id).label(label).small();
    let button = if primary {
        button.primary()
    } else {
        button.outline()
    };
    button.on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
        view.dispatch_ai(action.clone(), cx);
    }))
}

/// No provider yet: the empty state and the way to add one. It reads like the Connections
/// empty state: centred text and an outline button.
fn empty(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> Div {
    let action = set_up_action(view.workspace().ai());
    let label = if matches!(action, AiAction::OpenSettings) {
        "Open AI settings"
    } else {
        "Set up a provider"
    };
    empty_state(AppIcon::Sparkles, "No AI provider yet", EMPTY_LINE, cx)
        .py(px(SPACE[5]))
        .px(px(SPACE[4]))
        .text_center()
        .child(
            div()
                .pt(px(SPACE[3]))
                .child(action_button("ai-set-up", label, action, false, cx)),
        )
}

/// What AI does for the connection the user works on.
fn connection_section(model: &HubModel, cx: &mut Context<WorkspaceView>) -> Div {
    let mut section = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .child(caption("CONNECTION", cx));
    let Some(connection) = &model.connection else {
        return section.child(muted(
            "Open or select a connection to see its AI state.",
            cx,
        ));
    };
    section = section.child(connection_head(connection, cx));
    if let Some(AiStatus::On {
        provider,
        kind,
        model: name,
        local,
        also_remote,
        rows,
    }) = &connection.status
    {
        let place = match (local, also_remote) {
            (true, true) => "this computer and a remote service",
            (true, false) => "this computer",
            _ => "a remote service",
        };
        section = section
            .child(fact(
                "Provider",
                format!("{provider} ({})", kind_label(*kind)),
                cx,
            ))
            .child(fact("Model", name.clone(), cx))
            .child(fact("Data goes to", place, cx))
            .child(fact("Result rows", if *rows { "yes" } else { "no" }, cx));
    } else if connection.enabled {
        section = section.child(muted(
            "AI is on, but no model is chosen yet. Choose one in Settings > AI.",
            cx,
        ));
    }
    let (id, label) = if connection.enabled {
        ("ai-toggle", "Turn off")
    } else {
        ("ai-toggle", "Turn on for this connection")
    };
    section.child(div().px(px(SPACE[4])).pt(px(SPACE[1])).child(action_button(
        id,
        label,
        AiAction::ToggleConnection,
        !connection.enabled,
        cx,
    )))
}

/// The connection name, its environment chip and whether AI is on.
fn connection_head(connection: &HubConnection, cx: &Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    let (on, off) = (theme.success, theme.muted_foreground);
    let mut head = div()
        .px(px(SPACE[4]))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .text_size(px(ui_scale(cx).md))
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(connection.name.clone()),
        );
    if let Some(env) = connection.env {
        head = head.child(env_chip(env, cx));
    }
    head.child(div().flex_1()).child(
        div()
            .text_size(px(ui_scale(cx).sm))
            .text_color(if connection.enabled { on } else { off })
            .child(if connection.enabled {
                "AI on"
            } else {
                "AI off"
            }),
    )
}

/// The configured providers.
fn providers_section(model: &HubModel, cx: &Context<WorkspaceView>) -> Div {
    let mut section = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .child(caption("PROVIDERS", cx));
    for row in &model.providers {
        section = section.child(provider_row(row, cx));
    }
    section
}

/// One provider: id, kind, where it runs and whether a key is kept.
fn provider_row(row: &ProviderRow, cx: &Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    let muted_fg = theme.muted_foreground;
    let size = ui_scale(cx).sm;
    let place = if row.local { "local" } else { "remote" };
    let mut line = div()
        .px(px(SPACE[4]))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .text_size(px(size))
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(row.id.to_string()),
        )
        .child(
            div()
                .text_color(muted_fg)
                .child(format!("{} \u{b7} {place}", kind_label(row.kind))),
        )
        .child(div().flex_1());
    if row.key_stored {
        line = line.child(div().text_color(muted_fg).child("key stored"));
    }
    line
}

/// The buttons at the foot of the view.
fn buttons_section(cx: &mut Context<WorkspaceView>) -> Div {
    div()
        .px(px(SPACE[4]))
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(SPACE[2]))
        .child(action_button(
            "ai-settings",
            "AI settings",
            AiAction::OpenSettings,
            false,
            cx,
        ))
        .child(action_button(
            "ai-test",
            "Test provider",
            AiAction::TestProvider,
            false,
            cx,
        ))
}
