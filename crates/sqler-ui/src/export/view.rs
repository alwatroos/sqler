// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` views of the export: the dialog and the progress bar. They only render the state
//! in `Workspace`; every button calls back into `WorkspaceView` (`workspace/view/export.rs`).

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, ElementId, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _};

use super::{Delimiter, ExportDialog, ExportScope, FreshRun, LoadedRows};
use crate::grid::group_thousands;
use crate::tabs::TabId;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{
    DIALOG_W_MD, TOOLBAR_HEIGHT, dialog_footer, dialog_panel, dialog_title, field_row, scrim,
    ui_scale,
};

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

/// A button-like choice; the selected one has a filled background.
fn choice(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &Context<WorkspaceView>,
) -> gpui::Stateful<Div> {
    let theme = cx.theme();
    let (border, active, ring) = (theme.border, theme.list_active, theme.ring);
    let radius = theme.radius;
    let mut chip = div()
        .id(id)
        .px(px(SPACE[3]))
        .py(px(SPACE[1]))
        .rounded(radius)
        .border_1()
        .border_color(if selected { ring } else { border })
        .text_size(px(ui_scale(cx).sm))
        .cursor_pointer()
        .child(label.into());
    if selected {
        chip = chip.bg(active);
    }
    chip
}

fn labelled(label: &'static str, input: &gpui::Entity<InputState>, cx: &App) -> Div {
    field_row(label, Input::new(input), cx)
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
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let scale = ui_scale(cx);
    let form = &dialog.form;

    let mut formats = div().flex().flex_row().flex_wrap().gap(px(SPACE[2]));
    for (i, (id, label, _)) in dialog.formats.iter().enumerate() {
        let id = *id;
        formats = formats.child(
            choice(("export-format", i), *label, form.format == id, cx).on_click(cx.listener(
                move |view, _: &ClickEvent, window, cx| view.export_set_format(id, window, cx),
            )),
        );
    }

    let mut scopes = div().flex().flex_col().gap(px(SPACE[1]));
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
                .items_center()
                .gap(px(SPACE[2]))
                .text_size(px(scale.sm))
                .cursor_pointer()
                .child(mark)
                .child(scope_label(dialog, scope))
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.export_set_scope(scope, cx);
                })),
        );
    }
    if let FreshRun::Refused(reason) = &dialog.fresh {
        scopes = scopes.child(
            div()
                .text_size(px(scale.xs))
                .text_color(muted)
                .child(reason.clone()),
        );
    }

    let mut options = div().flex().flex_col().gap(px(SPACE[2]));
    match form.format.as_str() {
        sqler_export::CSV => {
            let mut delimiters = div().flex().flex_row().gap(px(SPACE[2]));
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
            options = options.child(field_row("Delimiter", delimiters, cx)).child(
                div()
                    .id("export-header")
                    .flex()
                    .flex_row()
                    .gap(px(SPACE[2]))
                    .text_size(px(scale.sm))
                    .cursor_pointer()
                    .child(if header { "[x]" } else { "[ ]" })
                    .child("Header row")
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        view.export_set_header(!header, cx);
                    })),
            );
            if let Some(inputs) = inputs {
                options = options.child(labelled("NULL text", &inputs.null_text, cx));
            }
        }
        sqler_export::MARKDOWN => {
            if let Some(inputs) = inputs {
                options = options.child(labelled("NULL text", &inputs.null_text, cx));
            }
        }
        sqler_export::SQL => {
            if let Some(inputs) = inputs {
                options = options
                    .child(labelled("Table", &inputs.table, cx))
                    .child(labelled("Schema (optional)", &inputs.schema, cx))
                    .child(labelled("Rows per INSERT", &inputs.rows_per_insert, cx));
            }
        }
        _ => {}
    }

    let footer = dialog_footer(cx)
        .child(
            Button::new("export-cancel")
                .label("Cancel")
                .small()
                .outline()
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.export_dismiss(cx))),
        )
        .child(
            Button::new("export-run")
                .label("Export\u{2026}")
                .small()
                .primary()
                .on_click(cx.listener(|view, event: &ClickEvent, _, cx| {
                    if event.click_count() == 1 {
                        view.export_submit(cx);
                    }
                })),
        );

    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .child(formats)
        .child(scopes)
        .child(options);
    if let Some(error) = &dialog.error {
        body = body.child(
            div()
                .text_size(px(scale.sm))
                .text_color(danger)
                .child(error.clone()),
        );
    }
    Some(
        scrim("export-scrim", cx)
            .child(
                dialog_panel("export-dialog", DIALOG_W_MD, cx)
                    .child(dialog_title(None, "Export results", cx))
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
    let scale = ui_scale(cx);
    let cancelling = status.cancelling;
    let cancel = Button::new("export-bar-cancel")
        .label("Cancel")
        .icon(AppIcon::CircleStop)
        .ghost()
        .small()
        .disabled(cancelling)
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| view.export_cancel(tab, cx)));
    Some(
        div()
            .id("export-bar")
            .flex_none()
            .w_full()
            .h(px(TOOLBAR_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE[3]))
            .px(px(SPACE[3]))
            .border_b_1()
            .border_color(border)
            .child(
                Spinner::new()
                    .icon(AppIcon::LoaderCircle)
                    .color(muted)
                    .xsmall(),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(scale.sm))
                    .text_color(muted)
                    .child(status.status_text()),
            )
            .child(cancel)
            .into_any_element(),
    )
}
