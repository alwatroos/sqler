// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The Plugins side view (ADR-0060): the plugins found with their state and permissions, the detail of the selected
//! one (what it asks for, what it may do, its last error and settings, enable, review and revoke) and the folders
//! that did not load. It only renders what [`PluginsState`](super::PluginsState) holds; every button runs a command.

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _};
use sqler_plugin_api::{CapabilityKind, SettingValue};

use super::RowGrant;
use crate::plugins::PluginCommand;
use crate::plugins::backend::{PluginProblem, PluginRow, RowState};
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, icon_button, section_header};
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{RowState as ListState, list_row, ui_scale};

/// The line of the empty state when the build has no plugin host.
const NO_SUPPORT: &str =
    "This build has no plugin support. Plugins need a build with the plugins feature.";
/// The line of the empty state when nothing was found.
const NO_PLUGINS: &str = "Put a plugin folder in the plugins folder of your configuration directory, or start SQLer with --plugin-dir <folder>. Then choose Rescan.";

/// Opacity of the text of a row that is switched off.
const OFF_OPACITY: f32 = 0.7;
/// How strongly a chip is tinted with its colour.
const CHIP_TINT: f32 = 0.12;

/// The Plugins side view.
pub(crate) fn plugins_view(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let theme = cx.theme();
    let (bg, fg, border) = (
        theme.sidebar,
        theme.sidebar_foreground,
        theme.sidebar_border,
    );
    let state = view.workspace().plugins();
    let mut header = section_header("PLUGINS", cx);
    if state.available() {
        header = header.child(
            icon_button("plugins-rescan", AppIcon::RefreshCw, "Rescan plugins").on_click(
                cx.listener(|view, _: &ClickEvent, window, cx| {
                    view.run_command(&PluginCommand::Refresh.command_id(), window, cx);
                }),
            ),
        );
    }
    let mut body = div().flex().flex_col().gap(px(SPACE[3])).pb(px(SPACE[4]));
    if !state.available() {
        body = body.child(
            empty_state(AppIcon::Plug, "No plugin support", NO_SUPPORT, cx)
                .py(px(SPACE[5]))
                .px(px(SPACE[4]))
                .text_center(),
        );
    } else if state.loading() {
        body = body.child(muted("Looking for plugins…", cx));
    } else if state.rows().is_empty() {
        body = body.child(
            empty_state(AppIcon::Plug, "No plugins found", NO_PLUGINS, cx)
                .py(px(SPACE[5]))
                .px(px(SPACE[4]))
                .text_center(),
        );
    } else {
        body = body.child(list(view, cx));
        if let Some(row) = state.selected_row() {
            body = body.child(detail(view, row, cx));
        }
    }
    if !state.problems().is_empty() {
        body = body.child(problems(state.problems(), cx));
    }
    div()
        .id("plugins-view")
        .track_focus(view.sidebar_focus())
        .size_full()
        .flex()
        .flex_col()
        .bg(bg)
        .text_color(fg)
        .border_r_1()
        .border_color(border)
        .child(header)
        .child(
            div()
                .id("plugins-view-body")
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

/// A small outlined label tinted with `color`.
fn chip(text: impl Into<SharedString>, color: gpui::Hsla, cx: &Context<WorkspaceView>) -> Div {
    div()
        .flex_none()
        .px(px(SPACE[2]))
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(color)
        .bg(color.opacity(CHIP_TINT))
        .text_color(color)
        .text_size(px(ui_scale(cx).xs))
        .child(text.into())
}

/// The state chip of a row: what the plugin is doing.
fn state_chip(row: &PluginRow, cx: &Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    match row.state {
        RowState::Live => chip("Running", theme.success, cx),
        RowState::Off => chip("Off", theme.muted_foreground, cx),
        RowState::Faulted { count } => {
            chip(format!("Stopped after {count} faults"), theme.danger, cx)
        }
        RowState::LoadFailed => chip("Failed to load", theme.danger, cx),
        _ => chip("Not started", theme.muted_foreground, cx),
    }
}

/// The grant chip of a row: what the user decided.
fn grant_chip(grant: &RowGrant, cx: &Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    match grant {
        RowGrant::Approved => chip("Allowed", theme.success, cx),
        RowGrant::Denied => chip("Denied", theme.muted_foreground, cx),
        RowGrant::NeedsPrompt => chip("Needs your decision", theme.warning, cx),
        RowGrant::Unavailable(_) => chip("Permissions unavailable", theme.danger, cx),
    }
}

/// The plugins, one two-line row each: name, version and state; then the permissions it asks for.
fn list(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> Div {
    let state = view.workspace().plugins();
    let mut list = div().flex().flex_col().gap(px(SPACE[1])).px(px(SPACE[2]));
    for (ix, row) in state.rows().iter().enumerate() {
        let id = row.manifest.id().clone();
        let selected = state.selected() == Some(&id);
        let muted_fg = cx.theme().muted_foreground;
        let size = ui_scale(cx).sm;
        let mut caps = div().flex().flex_row().flex_wrap().gap(px(SPACE[1]));
        for cap in row.manifest.capabilities().iter() {
            let granted = row.effective.contains(cap);
            // `db.read` and `net.http` are marked: the first reads the user's data, the second can send it away.
            let marked = matches!(cap.kind(), CapabilityKind::DbRead | CapabilityKind::NetHttp);
            let color = if !granted {
                muted_fg
            } else if marked {
                cx.theme().warning
            } else {
                cx.theme().foreground
            };
            caps = caps.child(
                div()
                    .px(px(SPACE[1]))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(if marked { color } else { cx.theme().border })
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(ui_scale(cx).mono))
                    .text_color(color)
                    .child(cap.to_string()),
            );
        }
        let line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE[2]))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(row.manifest.name().to_owned()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(muted_fg)
                    .child(row.manifest.version().to_string()),
            )
            .child(div().flex_1())
            .child(state_chip(row, cx));
        let mut text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(SPACE[1]))
            .py(px(SPACE[1]))
            .child(line)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(SPACE[2]))
                    .child(grant_chip(&state.grant_of(row), cx))
                    .child(caps),
            );
        if !row.enabled {
            text = text.opacity(OFF_OPACITY);
        }
        let click_id = id.clone();
        list = list.child(
            list_row(
                ("plugin-row", ix),
                ListState {
                    selected,
                    focused: false,
                    disabled: false,
                },
                cx,
            )
            .h_auto()
            .text_size(px(size))
            .child(text)
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.plugin_select(&click_id, cx);
            })),
        );
    }
    list
}

/// A `label  value` line of the detail.
fn fact(label: &'static str, value: impl Into<SharedString>, cx: &Context<WorkspaceView>) -> Div {
    div()
        .px(px(SPACE[4]))
        .flex()
        .flex_row()
        .justify_between()
        .gap(px(SPACE[3]))
        .text_size(px(ui_scale(cx).sm))
        .child(div().text_color(cx.theme().muted_foreground).child(label))
        .child(div().min_w_0().child(value.into()))
}

/// A command button of the detail.
fn command_button(
    id: &'static str,
    label: &'static str,
    command: PluginCommand,
    disabled: bool,
    cx: &mut Context<WorkspaceView>,
) -> Button {
    Button::new(id)
        .label(label)
        .small()
        .outline()
        .disabled(disabled)
        .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
            view.run_command(&command.command_id(), window, cx);
        }))
}

/// The selected plugin: what it is, what it asks for, how it fares, its settings and the buttons.
fn detail(view: &WorkspaceView, row: &PluginRow, cx: &mut Context<WorkspaceView>) -> Div {
    let state = view.workspace().plugins();
    let theme = cx.theme();
    let danger = theme.danger;
    let grant = state.grant_of(row);
    let manifest = &row.manifest;
    let mut section = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .pt(px(SPACE[2]))
        .border_t_1()
        .border_color(theme.sidebar_border)
        .debug_selector(|| "plugin-detail".to_owned())
        .child(caption("DETAILS", cx));
    if let Some(description) = manifest.description() {
        section = section.child(muted(description.to_owned(), cx));
    }
    section = section.child(fact("Plugin", manifest.id().to_string(), cx));
    if !manifest.authors().is_empty() {
        section = section.child(fact("Authors", manifest.authors().join(", "), cx));
    }
    if let Some(license) = manifest.license() {
        section = section.child(fact("License", license.to_owned(), cx));
    }
    if row.dev {
        section = section.child(fact("Loaded from", "a --plugin-dir folder", cx));
    }
    section = section.child(fact("Enabled", if row.enabled { "yes" } else { "no" }, cx));
    let permissions = match &grant {
        RowGrant::Approved => format!(
            "{} of {} in use",
            row.effective.len(),
            manifest.capabilities().len()
        ),
        RowGrant::Denied => "denied".to_owned(),
        RowGrant::NeedsPrompt => "waiting for your decision".to_owned(),
        RowGrant::Unavailable(why) => format!("unavailable: {why}"),
    };
    section = section.child(fact("Permissions", permissions, cx));
    if let Some(error) = state.error_of(row) {
        section = section.child(
            div()
                .px(px(SPACE[4]))
                .text_size(px(ui_scale(cx).sm))
                .text_color(danger)
                .child(format!("Last error: {error}")),
        );
    }
    for issue in &row.scope_issues {
        section = section.child(
            div()
                .px(px(SPACE[4]))
                .text_size(px(ui_scale(cx).sm))
                .text_color(danger)
                .child(issue.clone()),
        );
    }
    section = section.child(settings(view, row, cx));

    let (enable_id, enable_label, enable_cmd) = if row.enabled {
        ("plugin-disable", "Turn off", PluginCommand::Disable)
    } else {
        ("plugin-enable", "Turn on", PluginCommand::Enable)
    };
    section.child(
        div()
            .px(px(SPACE[4]))
            .pt(px(SPACE[1]))
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(SPACE[2]))
            .child(command_button(
                enable_id,
                enable_label,
                enable_cmd,
                false,
                cx,
            ))
            .child(command_button(
                "plugin-review",
                "Review permissions",
                PluginCommand::ReviewPermissions,
                false,
                cx,
            ))
            .child(command_button(
                "plugin-revoke",
                "Revoke",
                PluginCommand::Revoke,
                grant != RowGrant::Approved,
                cx,
            )),
    )
}

/// The settings the plugin declares, an editor by type.
fn settings(view: &WorkspaceView, row: &PluginRow, cx: &mut Context<WorkspaceView>) -> Div {
    let mut section = div().flex().flex_col().gap(px(SPACE[2]));
    if row.settings.is_empty() {
        return section;
    }
    section = section.child(caption("SETTINGS", cx));
    let id = row.manifest.id().clone();
    for (spec, value) in &row.settings {
        let key = spec.key.to_string();
        let editor: AnyElement = match value {
            SettingValue::Boolean(on) => {
                let weak = cx.weak_entity();
                let (id, key) = (id.clone(), key.clone());
                Switch::new(SharedString::from(format!("plugin-setting-{key}")))
                    .checked(*on)
                    .accessibility_label(spec.label.clone())
                    .on_click(move |_, _, cx| {
                        weak.update(cx, |view, cx| view.plugin_toggle_setting(&id, &key, cx))
                            .ok();
                    })
                    .into_any_element()
            }
            _ => match view.plugin_setting_input(&id, &key) {
                Some(input) => setting_input(input, &spec.label),
                None => div().into_any_element(),
            },
        };
        let mut line = div()
            .px(px(SPACE[4]))
            .flex()
            .flex_col()
            .gap(px(SPACE[1]))
            .child(
                div()
                    .text_size(px(ui_scale(cx).sm))
                    .child(spec.label.clone()),
            )
            .child(editor);
        if let Some(description) = &spec.description {
            line = line.child(
                div()
                    .text_size(px(ui_scale(cx).xs))
                    .text_color(cx.theme().muted_foreground)
                    .child(description.clone()),
            );
        }
        section = section.child(line);
    }
    section
}

/// The input of a text, integer or float setting; Enter or leaving the field keeps the value.
fn setting_input(input: &gpui::Entity<InputState>, label: &str) -> AnyElement {
    Input::new(input)
        .small()
        .aria_label(label.to_owned())
        .into_any_element()
}

/// The folders that did not load, with the reason.
fn problems(problems: &[PluginProblem], cx: &Context<WorkspaceView>) -> Div {
    let theme = cx.theme();
    let (danger, muted_fg) = (theme.danger, theme.muted_foreground);
    let mut section = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .child(caption("COULD NOT LOAD", cx));
    for problem in problems {
        let name = problem.id.as_deref().unwrap_or(problem.dir.as_str());
        section = section.child(
            div()
                .px(px(SPACE[4]))
                .flex()
                .flex_col()
                .text_size(px(ui_scale(cx).sm))
                .child(
                    div()
                        .font_family(theme.mono_font_family.clone())
                        .text_size(px(ui_scale(cx).mono))
                        .text_color(muted_fg)
                        .child(name.to_owned()),
                )
                .child(div().text_color(danger).child(problem.message.clone())),
        );
    }
    section
}
