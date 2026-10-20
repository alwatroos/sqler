// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The two overlays a run can open: the parameter prompt and the production confirmation.
//! They look like the DROP confirmation (`workspace/drop_dialog.rs`) and only render the
//! `QueryTab` phase; the buttons call back into `WorkspaceView`.

use gpui::{
    AnyElement, ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::Input;

use crate::query::{BoundStatement, ParamForm, ParamKind, Phase};
use crate::tabs::TabId;
use crate::workspace::WorkspaceView;

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

fn scrim(id: &'static str) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::black().opacity(0.45))
        .occlude()
}

#[allow(clippy::too_many_lines)] // one declarative element tree
fn params_dialog(
    view: &WorkspaceView,
    tab: TabId,
    form: &ParamForm,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (panel_bg, border, fg, muted, danger) = (
        theme.popover,
        theme.border,
        theme.popover_foreground,
        theme.muted_foreground,
        theme.danger,
    );
    let mono = theme.mono_font_family.clone();
    let mut rows = div().flex().flex_col().gap_2().px_4().py_2();
    for (i, row) in form.rows().iter().enumerate() {
        let kind = row.kind;
        let mut line = div().flex().flex_col().gap_1().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .font_family(mono.clone())
                        .text_sm()
                        .child(row.key.label()),
                )
                .child(
                    div()
                        .id(("param-kind", i))
                        .w(px(80.))
                        .flex_none()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .text_sm()
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
            line = line.child(div().text_xs().text_color(danger).child(error.clone()));
        }
        rows = rows.child(line);
    }
    let footer = div()
        .flex()
        .flex_row()
        .justify_end()
        .gap_2()
        .p_4()
        .child(
            div()
                .id("param-cancel")
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .child("Cancel")
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.query_dismiss(tab, cx);
                })),
        )
        .child(
            div()
                .id("param-run")
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .child("Run")
                .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
                    if event.click_count() == 1 {
                        view.query_submit(tab, cx);
                    }
                })),
        );
    scrim("param-scrim")
        .child(
            div()
                .id("param-dialog")
                .w(px(520.))
                .max_h(px(640.))
                .flex()
                .flex_col()
                .bg(panel_bg)
                .text_color(fg)
                .border_1()
                .border_color(border)
                .rounded_lg()
                .shadow_lg()
                .child(
                    div()
                        .px_4()
                        .pt_4()
                        .pb_1()
                        .text_lg()
                        .child("Query parameters"),
                )
                .child(
                    div()
                        .px_4()
                        .pb_1()
                        .text_sm()
                        .text_color(muted)
                        .child("Values are bound to the statements, never pasted into the SQL."),
                )
                .child(rows)
                .child(footer),
        )
        .into_any_element()
}

#[allow(clippy::too_many_lines)] // one declarative element tree
fn confirm_dialog(
    view: &WorkspaceView,
    tab: TabId,
    connection: &str,
    statements: &[BoundStatement],
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (panel_bg, border, fg, muted) = (
        theme.popover,
        theme.border,
        theme.popover_foreground,
        theme.muted_foreground,
    );
    let (danger, danger_fg) = (theme.danger, theme.danger_foreground);
    let mono = theme.mono_font_family.clone();
    let code_bg = theme.muted;
    let writes: Vec<&BoundStatement> = statements
        .iter()
        .filter(|s| !s.class.is_read_only())
        .collect();
    let mut list = div()
        .id("confirm-statements")
        .flex()
        .flex_col()
        .gap_2()
        .mx_4()
        .max_h(px(360.))
        .overflow_y_scroll();
    for s in &writes {
        let mut item = div()
            .p_2()
            .rounded_md()
            .bg(code_bg)
            .font_family(mono.clone())
            .text_sm()
            .child(div().text_xs().text_color(muted).child(format!(
                "#{} · {}",
                s.index + 1,
                s.class.as_str()
            )))
            .child(s.sql.clone());
        for (key, value) in &s.values {
            item = item.child(
                div()
                    .text_xs()
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
                .p_2()
                .rounded_md()
                .font_family(mono.clone())
                .text_xs()
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
    let mut cancel = div()
        .id("confirm-cancel")
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .child("Cancel")
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.query_dismiss(tab, cx);
        }));
    if let Some(focus) = view.queries.confirm_focus() {
        cancel = cancel.track_focus(focus);
    }
    let confirm = div()
        .id("confirm-run")
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(danger)
        .bg(danger)
        .text_color(danger_fg)
        .cursor_pointer()
        .child("Run")
        .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
            if event.click_count() == 1 {
                view.query_confirm(tab, cx);
            }
        }));
    scrim("confirm-scrim")
        .child(
            div()
                .id("confirm-dialog")
                .w(px(620.))
                .max_h(px(720.))
                .flex()
                .flex_col()
                .bg(panel_bg)
                .text_color(fg)
                .border_1()
                .border_color(border)
                .rounded_lg()
                .shadow_lg()
                .child(
                    div()
                        .px_4()
                        .pt_4()
                        .pb_2()
                        .text_lg()
                        .child("Run on production?"),
                )
                .child(div().px_4().pb_2().text_sm().text_color(muted).child(body))
                .child(list)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .p_4()
                        .child(cancel)
                        .child(confirm),
                ),
        )
        .into_any_element()
}
