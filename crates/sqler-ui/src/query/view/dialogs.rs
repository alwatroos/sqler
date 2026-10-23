// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The two overlays a run can open: the parameter prompt and the production confirmation.
//! They look like the DROP confirmation (`workspace/drop_dialog.rs`) and only render the
//! `QueryTab` phase; the buttons call back into `WorkspaceView`.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::{ActiveTheme as _, Sizable as _};
use sqler_driver_api::sqler_core::Environment;

use crate::query::{BoundStatement, ParamForm, ParamKind, Phase};
use crate::tabs::TabId;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::env_chip;
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{
    DIALOG_W_LG, DIALOG_W_MD, LABEL_WIDTH, LIST_MAX_H, dialog_footer, dialog_panel, dialog_title,
    ui_scale,
};

pub(crate) use crate::workspace::chrome::scrim;

/// Width of the kind column of a parameter row.
const KIND_W: f32 = 80.;

/// The overlay of the tab's current phase; `None` unless the tab is prompting or confirming.
pub(crate) fn query_overlay(
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let query = view.workspace().query_tab(tab)?;
    match query.phase() {
        Phase::Prompting(form) => Some(params_dialog(view, tab, form, cx)),
        Phase::Confirming(statements) => {
            let name = view
                .workspace()
                .tabs()
                .get(tab)
                .map_or_else(String::new, |t| t.connection().name.clone());
            Some(confirm_dialog(view, tab, &name, statements, cx))
        }
        _ => None,
    }
}

/// The next kind in the list, wrapping around.
fn next_kind(kind: ParamKind) -> ParamKind {
    let all = ParamKind::ALL;
    let at = all.iter().position(|k| *k == kind).unwrap_or(0);
    all[(at + 1) % all.len()]
}

#[allow(clippy::too_many_lines)] // one declarative element tree
fn params_dialog(
    view: &WorkspaceView,
    tab: TabId,
    form: &ParamForm,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (border, muted, danger, radius) = (
        theme.border,
        theme.muted_foreground,
        theme.danger,
        theme.radius,
    );
    let mono = theme.mono_font_family.clone();
    let mut rows = div().flex().flex_col().gap(px(SPACE[3]));
    for (i, row) in form.rows().iter().enumerate() {
        let kind = row.kind;
        let mut line = div().flex().flex_col().gap(px(SPACE[1])).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE[3]))
                .child(
                    div()
                        .w(px(LABEL_WIDTH))
                        .flex_none()
                        .font_family(mono.clone())
                        .text_size(px(scale.mono))
                        .child(row.key.label()),
                )
                .child(
                    div()
                        .id(("param-kind", i))
                        .w(px(KIND_W))
                        .flex_none()
                        .px(px(SPACE[3]))
                        .py(px(SPACE[0]))
                        .rounded(radius)
                        .border_1()
                        .border_color(border)
                        .text_size(px(scale.sm))
                        .cursor_pointer()
                        .child(kind.label())
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.query_set_kind(tab, i, next_kind(kind), cx);
                        })),
                )
                .children(
                    view.queries
                        .param_input(tab, i)
                        .map(|input| div().flex_1().child(Input::new(input))),
                ),
        );
        if let Some(error) = &row.error {
            line = line.child(
                div()
                    .text_size(px(scale.xs))
                    .text_color(danger)
                    .child(error.clone()),
            );
        }
        rows = rows.child(line);
    }
    let footer = dialog_footer(cx)
        .child(
            Button::new("param-cancel")
                .label("Cancel")
                .outline()
                .small()
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.query_dismiss(tab, cx);
                })),
        )
        .child(
            Button::new("param-run")
                .label("Run")
                .primary()
                .small()
                .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
                    if event.click_count() == 1 {
                        view.query_submit(tab, cx);
                    }
                })),
        );
    scrim("param-scrim", cx)
        .child(
            dialog_panel("param-dialog", DIALOG_W_MD, cx)
                .child(dialog_title(None, "Query parameters", cx))
                .child(
                    div()
                        .text_size(px(scale.sm))
                        .text_color(muted)
                        .child("Values are bound to the statements, never pasted into the SQL."),
                )
                .child(rows)
                .child(footer),
        )
        .into_any_element()
}

fn confirm_dialog(
    view: &WorkspaceView,
    tab: TabId,
    connection: &str,
    statements: &[BoundStatement],
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let cancel = cx.listener(move |view, _: &ClickEvent, _, cx| {
        view.query_dismiss(tab, cx);
    });
    let confirm = cx.listener(move |view, event: &ClickEvent, _, cx| {
        if event.click_count() == 1 {
            view.query_confirm(tab, cx);
        }
    });
    confirm_panel(
        connection,
        statements,
        None,
        view.queries.confirm_focus(),
        cancel,
        confirm,
        cx,
    )
}

/// The production confirmation as a dialog: the look of a run on a production connection. Shared with the approval
/// of plugin writes, whose second confirmation goes through the same gate. `intro` goes before the question; `focus`
/// is the handle of the Cancel wrapper (a stray Enter never confirms); the handlers see pointer clicks only.
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn confirm_panel(
    connection: &str,
    statements: &[BoundStatement],
    intro: Option<String>,
    focus: Option<&gpui::FocusHandle>,
    on_cancel: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    on_confirm: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    cx: &gpui::App,
) -> AnyElement {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (muted, code_bg, radius) = (theme.muted_foreground, theme.muted, theme.radius);
    let mono = theme.mono_font_family.clone();
    let writes: Vec<&BoundStatement> = statements
        .iter()
        .filter(|s| !s.class.is_read_only())
        .collect();
    let mut list = div()
        .id("confirm-statements")
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .max_h(px(LIST_MAX_H))
        .overflow_y_scroll();
    for s in &writes {
        let mut item = div()
            .p(px(SPACE[3]))
            .rounded(radius)
            .bg(code_bg)
            .font_family(mono.clone())
            .text_size(px(scale.mono))
            .child(
                div()
                    .text_size(px(scale.xs))
                    .text_color(muted)
                    .child(format!("#{} · {}", s.index + 1, s.class.as_str())),
            )
            .child(s.sql.clone());
        for (key, value) in &s.values {
            item = item.child(
                div()
                    .text_size(px(scale.xs))
                    .text_color(muted)
                    .child(format!("{} = {value}", key.label())),
            );
        }
        list = list.child(item);
    }
    let reads: Vec<&BoundStatement> = statements
        .iter()
        .filter(|s| s.class.is_read_only())
        .collect();
    for s in &reads {
        list = list.child(
            div()
                .p(px(SPACE[3]))
                .rounded(radius)
                .font_family(mono.clone())
                .text_size(px(scale.xs))
                .text_color(muted)
                .child(format!("#{} · read · {}", s.index + 1, s.sql)),
        );
    }
    let reads = reads.len();
    let mut body = format!(
        "{connection} is labelled production. Run {} statement(s) that may change data?",
        writes.len()
    );
    if reads > 0 {
        body = format!("{body} {reads} read-only statement(s) run with them.");
    }
    if let Some(intro) = intro {
        body = format!("{intro} {body}");
    }
    // The wrapper holds the default focus (Cancel), so a stray Enter never confirms.
    let mut cancel = div().child(
        Button::new("confirm-cancel")
            .label("Cancel")
            .outline()
            .small()
            .on_click(on_cancel),
    );
    if let Some(focus) = focus {
        cancel = cancel.track_focus(focus);
    }
    let confirm = Button::new("confirm-run")
        .label("Run")
        .primary()
        .danger()
        .small()
        .on_click(on_confirm);
    let title = dialog_title(Some(AppIcon::TriangleAlert), "Run on production?", cx)
        .child(env_chip(Environment::Prod, cx));
    scrim("confirm-scrim", cx)
        .child(
            dialog_panel("confirm-dialog", DIALOG_W_LG, cx)
                .child(title)
                .child(div().text_size(px(scale.sm)).text_color(muted).child(body))
                .child(list)
                .child(dialog_footer(cx).child(cancel).child(confirm)),
        )
        .into_any_element()
}
