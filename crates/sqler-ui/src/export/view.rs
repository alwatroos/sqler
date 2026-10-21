// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` views of the export: the dialog and the progress bar. They only render the state
//! in `Workspace`; every button calls back into `WorkspaceView` (`workspace/view/export.rs`).

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Input, InputState};

use super::{Delimiter, ExportDialog, ExportScope, FreshRun, LoadedRows};
use crate::grid::group_thousands;
use crate::tabs::TabId;
use crate::workspace::WorkspaceView;

/// The label of a scope choice.
fn scope_label(dialog: &ExportDialog, scope: ExportScope) -> String {
    match scope {
        ExportScope::Loaded if dialog.complete => {
            format!("All {} rows", group_thousands(dialog.loaded_rows))
        }
        ExportScope::Loaded => format!("Loaded rows ({})", group_thousands(dialog.loaded_rows)),
        ExportScope::Selection => match &dialog.selection {
            Some(LoadedRows::Picked { rows, columns }) => format!(
                "Selection ({} \u{d7} {})",
                group_thousands(rows.len()),
                group_thousands(columns.len())
            ),
            _ => "Selection".to_owned(),
        },
        ExportScope::Fresh => "All rows (runs the query again)".to_owned(),
    }
}

/// The scopes the dialog offers, in order.
fn offered_scopes(dialog: &ExportDialog) -> Vec<ExportScope> {
    let mut scopes = vec![ExportScope::Loaded];
    if dialog.selection.is_some() {
        scopes.push(ExportScope::Selection);
    }
    if dialog.fresh == FreshRun::Allowed {
        scopes.push(ExportScope::Fresh);
    }
    scopes
}

fn scrim() -> gpui::Stateful<Div> {
    div()
        .id("export-scrim")
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

/// A button-like choice; the selected one has a filled background.
fn choice(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &Context<WorkspaceView>,
) -> gpui::Stateful<Div> {
    let theme = cx.theme();
    let (border, active) = (theme.border, theme.list_active);
    let mut chip = div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(border)
        .text_sm()
        .cursor_pointer()
        .child(label.into());
    if selected {
        chip = chip.bg(active);
    }
    chip
}

fn labelled(label: &'static str, input: &gpui::Entity<InputState>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(div().w(px(120.)).flex_none().text_sm().child(label))
        .child(div().flex_1().child(Input::new(input)))
}

/// The export dialog; `None` when none is open.
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn export_overlay(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let dialog = view.workspace().export_dialog()?;
    let inputs = view.export_inputs();
    let theme = cx.theme();
    let (panel_bg, border, fg, muted, danger) = (
        theme.popover,
        theme.border,
        theme.popover_foreground,
        theme.muted_foreground,
        theme.danger,
    );
    let form = &dialog.form;

    let mut formats = div().flex().flex_row().flex_wrap().gap_2();
    for (i, (id, label, _)) in dialog.formats.iter().enumerate() {
        let id = *id;
        formats = formats.child(
            choice(("export-format", i), *label, form.format == id, cx).on_click(cx.listener(
                move |view, _: &ClickEvent, window, cx| view.export_set_format(id, window, cx),
            )),
        );
    }

    let mut scopes = div().flex().flex_col().gap_1();
    for (i, scope) in offered_scopes(dialog).into_iter().enumerate() {
        let mark = if form.scope == scope {
            "(\u{2022})"
        } else {
            "( )"
        };
        scopes = scopes.child(
            div()
                .id(("export-scope", i))
                .flex()
                .flex_row()
                .gap_2()
                .text_sm()
                .cursor_pointer()
                .child(mark)
                .child(scope_label(dialog, scope))
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.export_set_scope(scope, cx);
                })),
        );
    }
    if let FreshRun::Refused(reason) = &dialog.fresh {
        scopes = scopes.child(div().text_xs().text_color(muted).child(reason.clone()));
    }

    let mut options = div().flex().flex_col().gap_2();
    match form.format.as_str() {
        sqler_export::CSV => {
            let mut delimiters = div().flex().flex_row().gap_2();
            for (i, delimiter) in Delimiter::ALL.into_iter().enumerate() {
                delimiters = delimiters.child(
                    choice(
                        ("export-delimiter", i),
                        delimiter.label(),
                        form.delimiter == delimiter,
                        cx,
                    )
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, _, cx| view.export_set_delimiter(delimiter, cx),
                    )),
                );
            }
            let header = form.header;
            options = options
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(div().w(px(120.)).flex_none().text_sm().child("Delimiter"))
                        .child(delimiters),
                )
                .child(
                    div()
                        .id("export-header")
                        .flex()
                        .flex_row()
                        .gap_2()
                        .text_sm()
                        .cursor_pointer()
                        .child(if header { "[x]" } else { "[ ]" })
                        .child("Header row")
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.export_set_header(!header, cx);
                        })),
                );
            if let Some(inputs) = inputs {
                options = options.child(labelled("NULL text", &inputs.null_text));
            }
        }
        sqler_export::MARKDOWN => {
            if let Some(inputs) = inputs {
                options = options.child(labelled("NULL text", &inputs.null_text));
            }
        }
        sqler_export::SQL => {
            if let Some(inputs) = inputs {
                options = options
                    .child(labelled("Table", &inputs.table))
                    .child(labelled("Schema (optional)", &inputs.schema))
                    .child(labelled("Rows per INSERT", &inputs.rows_per_insert));
            }
        }
        _ => {}
    }

    let footer = div()
        .flex()
        .flex_row()
        .justify_end()
        .gap_2()
        .p_4()
        .child(
            div()
                .id("export-cancel")
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .child("Cancel")
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.export_dismiss(cx))),
        )
        .child(
            div()
                .id("export-run")
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .child("Export\u{2026}")
                .on_click(cx.listener(|view, event: &ClickEvent, _, cx| {
                    if event.click_count() == 1 {
                        view.export_submit(cx);
                    }
                })),
        );

    let mut body = div()
        .flex()
        .flex_col()
        .gap_3()
        .px_4()
        .py_2()
        .child(formats)
        .child(scopes)
        .child(options);
    if let Some(error) = &dialog.error {
        body = body.child(div().text_sm().text_color(danger).child(error.clone()));
    }
    Some(
        scrim()
            .child(
                div()
                    .id("export-dialog")
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
                    .child(div().px_4().pt_4().pb_1().text_lg().child("Export results"))
                    .child(body)
                    .child(footer),
            )
            .into_any_element(),
    )
}

/// The progress line of the export running in `tab`; `None` when none runs.
pub(crate) fn export_bar(
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let status = view.workspace().export_status(tab)?;
    let theme = cx.theme();
    let (border, muted) = (theme.border, theme.muted_foreground);
    let cancelling = status.cancelling;
    let mut cancel = div()
        .id("export-bar-cancel")
        .px_2()
        .rounded_md()
        .border_1()
        .border_color(border)
        .text_sm()
        .child("Cancel");
    if !cancelling {
        cancel = cancel
            .cursor_pointer()
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| view.export_cancel(tab, cx)));
    }
    Some(
        div()
            .id("export-bar")
            .flex_none()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(muted)
                    .child(status.status_text()),
            )
            .child(cancel)
            .into_any_element(),
    )
}
