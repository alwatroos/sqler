// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The editing views of the results grid: the cell editor, the cell marks, the edit bar and the
//! dialogs (preview, production confirmation, keyless warning, close prompt).
//!
//! The views only render the state of [`GridEditor`](super::GridEditor) and of the workspace.
//! Every change goes through the headless editor, and every apply goes through
//! `Workspace::request_apply` (ADR-0033): nothing here builds a plan on its own authority or
//! formats a plan, a statement or a value with `{:?}`.

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, Focusable as _, Hsla,
    InteractiveElement as _, IntoElement as _, ListAlignment, ListState, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, list,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::table::TableState;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, Theme, WindowExt as _};

use super::{
    CellMark, DisplayCell, EditState, EditorInput, EditorKind, GridKey, KEYLESS_WARNING,
    PreviewStatement,
};
use crate::grid::view::{GridDelegate, Table, apply_action};
use crate::grid::{CellPos, DisplayMap, GridAction, Motion, RowOrder as _};
use crate::query::view::scrim;
use crate::workspace::{CloseTarget, WorkspaceView};

/// The largest selection a bulk edit (Set NULL, Revert, Delete) works on in one go.
const MAX_BULK: usize = 100_000;
/// Height of the statement list in the preview and the confirmation, in pixels.
const LIST_HEIGHT: f32 = 380.;

/// The input of the open cell editor.
pub(crate) struct CellInput {
    /// The text field; `None` for a boolean cell (it has buttons).
    input: Option<Entity<InputState>>,
    _subs: Vec<Subscription>,
}

// ---- marks -------------------------------------------------------------------------------------

/// How a cell is drawn on top of the plain text.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct MarkStyle {
    pub(crate) background: Option<Hsla>,
    pub(crate) border: Option<Hsla>,
    pub(crate) text: Option<Hsla>,
    pub(crate) italic: bool,
    pub(crate) strike: bool,
}

/// The drawing of a cell from its mark. Colors come from the theme, so light and dark both work.
pub(crate) fn mark_style(cell: &DisplayCell, theme: &Theme) -> MarkStyle {
    let mut style = match cell.mark {
        CellMark::Changed => MarkStyle {
            background: Some(theme.warning.opacity(0.25)),
            ..MarkStyle::default()
        },
        CellMark::New => MarkStyle {
            background: Some(theme.success.opacity(0.2)),
            ..MarkStyle::default()
        },
        CellMark::Unset => MarkStyle {
            background: Some(theme.success.opacity(0.08)),
            text: Some(theme.muted_foreground),
            italic: true,
            ..MarkStyle::default()
        },
        CellMark::Deleted => MarkStyle {
            background: Some(theme.danger.opacity(0.18)),
            strike: true,
            ..MarkStyle::default()
        },
        CellMark::DeletedApplied => MarkStyle {
            text: Some(theme.muted_foreground),
            strike: true,
            ..MarkStyle::default()
        },
        CellMark::Failed => MarkStyle {
            background: Some(theme.danger.opacity(0.22)),
            border: Some(theme.danger),
            ..MarkStyle::default()
        },
        _ => MarkStyle::default(),
    };
    if cell.null && style.text.is_none() {
        style.text = Some(theme.muted_foreground);
        style.italic = true;
    }
    style
}

/// The text a cell shows: an unset cell of a new row says so.
pub(crate) fn shown_text(cell: &DisplayCell) -> String {
    if cell.mark == CellMark::Unset {
        "(default)".to_owned()
    } else {
        cell.text.clone()
    }
}

// ---- the cell editor ---------------------------------------------------------------------------

/// Whether a cell is being edited in this table.
pub(crate) fn is_editing(state: &TableState<GridDelegate>) -> bool {
    state.delegate().tab.edit().editing().is_some()
}

/// Tells the user something about the grid.
fn notify(window: &mut Window, cx: &mut Context<TableState<GridDelegate>>, message: String) {
    window.push_notification(SharedString::from(message), cx);
}

/// Opens the editor on the active cell. Returns whether it opened.
pub(crate) fn start_cell_edit(
    state: &mut TableState<GridDelegate>,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) -> bool {
    let delegate = state.delegate_mut();
    let Some(pos) = delegate.controls.selection().active() else {
        notify(window, cx, "Select a cell first.".to_owned());
        return false;
    };
    if delegate.tab.grid_rows().display_row(pos.row).is_none() {
        notify(window, cx, "Select a cell first.".to_owned());
        return false;
    }
    let opened = {
        let (model, editor) = delegate.tab.edit_parts();
        editor
            .start_edit(model, pos)
            .map(|e| (e.kind, e.initial.clone()))
    };
    let (kind, initial) = match opened {
        Ok(opened) => opened,
        Err(message) => {
            notify(window, cx, message);
            return false;
        }
    };
    if kind == EditorKind::Boolean {
        delegate.cell_editor = Some(CellInput {
            input: None,
            _subs: Vec::new(),
        });
        window.focus(&state.focus_handle(cx), cx);
        cx.notify();
        return true;
    }
    let text = match &initial {
        EditorInput::Text(t) => t.clone(),
        EditorInput::Null | EditorInput::Bool(_) => String::new(),
    };
    if text.contains(['\n', '\r']) {
        // The single-line input would change the value on commit.
        delegate.tab.edit_parts().1.cancel_edit();
        notify(
            window,
            cx,
            "This value has line breaks; the cell editor is single-line and would change it."
                .to_owned(),
        );
        return false;
    }
    let placeholder = if initial == EditorInput::Null {
        "NULL".to_owned()
    } else {
        kind.hint().unwrap_or("value").to_owned()
    };
    let input = cx.new(|cx| {
        let mut input = InputState::new(window, cx).placeholder(placeholder);
        input.set_value(text, window, cx);
        input
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        |state, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => {
                commit_cell_edit(state, None, window, cx);
            }
            InputEvent::Blur => blur_cell_edit(state, window, cx),
            _ => {}
        },
    );
    window.focus(&input.focus_handle(cx), cx);
    state.delegate_mut().cell_editor = Some(CellInput {
        input: Some(input),
        _subs: vec![subscription],
    });
    cx.notify();
    true
}

/// What the open editor holds, as the editor input: the text, or the value a boolean cell had.
fn current_input(state: &TableState<GridDelegate>, cx: &gpui::App) -> Option<EditorInput> {
    let delegate = state.delegate();
    let editor = delegate.tab.edit().editing()?;
    Some(
        match delegate.cell_editor.as_ref().and_then(|c| c.input.as_ref()) {
            Some(input) => EditorInput::Text(input.read(cx).value().to_string()),
            None => editor.initial.clone(),
        },
    )
}

/// Stages `input` in the open editor. Returns whether the editor closed.
pub(crate) fn commit_with(
    state: &mut TableState<GridDelegate>,
    input: EditorInput,
    then: Option<Motion>,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) -> bool {
    let delegate = state.delegate_mut();
    let result = {
        let (model, editor) = delegate.tab.edit_parts();
        editor.commit_edit(model, input)
    };
    if let Err(message) = result {
        if delegate.tab.edit().editing().is_none() {
            // The cell can no longer be changed: the editor is gone.
            delegate.cell_editor = None;
            window.focus(&state.focus_handle(cx), cx);
        }
        notify(window, cx, message);
        cx.notify();
        return false;
    }
    delegate.cell_editor = None;
    window.focus(&state.focus_handle(cx), cx);
    if let Some(to) = then {
        apply_action(state, GridAction::Move { to, extend: false }, window, cx);
    }
    cx.notify();
    true
}

/// Accepts the typed value (Enter, Tab). On an invalid value the editor stays open.
pub(crate) fn commit_cell_edit(
    state: &mut TableState<GridDelegate>,
    then: Option<Motion>,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) -> bool {
    let Some(input) = current_input(state, cx) else {
        return false;
    };
    commit_with(state, input, then, window, cx)
}

/// Closes the editor and drops the input (Escape).
pub(crate) fn cancel_cell_edit(
    state: &mut TableState<GridDelegate>,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) {
    let delegate = state.delegate_mut();
    delegate.tab.edit_parts().1.cancel_edit();
    delegate.cell_editor = None;
    window.focus(&state.focus_handle(cx), cx);
    cx.notify();
}

/// The input lost the focus: a valid value is kept, an invalid one is dropped with a notice.
fn blur_cell_edit(
    state: &mut TableState<GridDelegate>,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) {
    let Some(input) = current_input(state, cx) else {
        return;
    };
    if state.delegate().cell_editor.is_none() {
        return;
    }
    let delegate = state.delegate_mut();
    let result = {
        let (model, editor) = delegate.tab.edit_parts();
        editor.commit_edit(model, input)
    };
    delegate.cell_editor = None;
    if let Err(message) = result {
        delegate.tab.edit_parts().1.cancel_edit();
        notify(window, cx, format!("The edit was dropped. {message}"));
    }
    cx.notify();
}

/// The editor element for a cell, if that cell is the one being edited.
pub(crate) fn editor_element(
    delegate: &GridDelegate,
    row: usize,
    model_col: usize,
    _window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) -> Option<AnyElement> {
    let editor = delegate.tab.edit().editing()?;
    // `row` is a display row; the editor's cell is in grid rows (the model row's identity).
    if editor.pos
        != (CellPos {
            row: delegate.tab.grid_row(row)?,
            col: model_col,
        })
    {
        return None;
    }
    let cell = delegate.cell_editor.as_ref()?;
    if let Some(input) = &cell.input {
        return Some(Input::new(input).small().into_any_element());
    }
    let current = match &editor.initial {
        EditorInput::Bool(b) => Some(*b),
        _ => None,
    };
    let nullable = editor.nullable;
    let mut row = div().flex().flex_row().items_center().gap_1();
    for (ix, (label, value)) in [("true", Some(true)), ("false", Some(false)), ("NULL", None)]
        .into_iter()
        .enumerate()
    {
        if value.is_none() && !nullable {
            continue;
        }
        let mut button = Button::new(("cell-bool", ix)).label(label).small();
        if value == current {
            button = button.primary();
        } else {
            button = button.ghost();
        }
        row = row.child(
            button.on_click(cx.listener(move |state, _: &ClickEvent, window, cx| {
                let input = value.map_or(EditorInput::Null, EditorInput::Bool);
                commit_with(state, input, None, window, cx);
            })),
        );
    }
    Some(row.into_any_element())
}

// ---- table-local edit actions ------------------------------------------------------------------

/// The cells of the selection, in grid rows and model columns.
fn selected_cells(delegate: &GridDelegate) -> Result<Vec<CellPos>, String> {
    let order = delegate.tab.grid_rows();
    let map = DisplayMap {
        rows: &order,
        columns: delegate.controls.layout(),
    };
    let Some(rect) = delegate.controls.selection().rect(&map) else {
        return Err("Select a cell first.".to_owned());
    };
    if rect.rows.len().saturating_mul(rect.cols.len()) > MAX_BULK {
        return Err(format!(
            "The selection is too large for one change (at most {MAX_BULK} cells)."
        ));
    }
    let mut cells = Vec::with_capacity(rect.rows.len() * rect.cols.len());
    for row in rect.rows {
        for col in rect.cols.clone() {
            if let Some(cell) = map.cell_at(row, col) {
                cells.push(cell);
            }
        }
    }
    Ok(cells)
}

/// The grid rows of the selection.
fn selected_rows(delegate: &GridDelegate) -> Result<Vec<usize>, String> {
    let order = delegate.tab.grid_rows();
    let map = DisplayMap {
        rows: &order,
        columns: delegate.controls.layout(),
    };
    let Some(rect) = delegate.controls.selection().rect(&map) else {
        return Err("Select a row first.".to_owned());
    };
    if rect.rows.len() > MAX_BULK {
        return Err(format!(
            "The selection is too large for one change (at most {MAX_BULK} rows)."
        ));
    }
    // The rectangle is in display rows; the editor works on grid rows (model identity).
    Ok(rect.rows.filter_map(|d| order.model_row(d)).collect())
}

/// Runs an edit action that only touches this table. `None` for every other action.
pub(crate) fn apply_edit_action(
    state: &mut TableState<GridDelegate>,
    action: GridAction,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) -> Option<bool> {
    let outcome = match action {
        GridAction::EditCell => return Some(start_cell_edit(state, window, cx)),
        GridAction::SetNull => {
            let delegate = state.delegate_mut();
            selected_cells(delegate).and_then(|cells| {
                let (model, editor) = delegate.tab.edit_parts();
                editor.set_null(model, &cells).map(|_| ())
            })
        }
        GridAction::RevertSelection => {
            let delegate = state.delegate_mut();
            selected_cells(delegate).map(|cells| {
                let (model, editor) = delegate.tab.edit_parts();
                editor.revert(model, &cells);
            })
        }
        GridAction::DeleteRows => {
            let delegate = state.delegate_mut();
            selected_rows(delegate).and_then(|rows| {
                let (model, editor) = delegate.tab.edit_parts();
                editor.delete_rows(model, &rows).map(|_| ())
            })
        }
        GridAction::AddRow => {
            let delegate = state.delegate_mut();
            let added = {
                let (model, editor) = delegate.tab.edit_parts();
                editor.add_row(model)
            };
            if let Ok(grid_row) = added {
                // The new row is last in the display, below the sorted and filtered rows.
                let rows = delegate.tab.grid_rows();
                if let Some(row) = rows.display_row(grid_row) {
                    delegate.controls.click_cell(row, 0, &rows, false);
                    state
                        .vertical_scroll_handle
                        .scroll_to_item_strict(row, gpui::ScrollStrategy::Top);
                }
            }
            added.map(|_| ())
        }
        GridAction::DiscardChanges => {
            let delegate = state.delegate_mut();
            delegate.cell_editor = None;
            delegate.tab.edit_parts().1.discard();
            Ok(())
        }
        _ => return None,
    };
    if let Err(message) = outcome {
        notify(window, cx, message);
    }
    cx.notify();
    Some(true)
}

// ---- the edit bar ------------------------------------------------------------------------------

/// What the bar needs from the grid, copied out so the table is not borrowed while building.
struct BarState {
    state: EditState,
    pending: usize,
    status: Option<String>,
    blocked: Option<String>,
    reason: Option<String>,
    keyless: bool,
    can_opt_in: bool,
    production: bool,
}

fn bar_state(table: &Table, cx: &gpui::App) -> Option<BarState> {
    let grid = &table.read(cx).delegate().tab;
    let edit = grid.edit();
    let context = edit.context()?;
    let can_opt_in = matches!(
        edit.state(),
        EditState::ReadOnly(reason) if reason.can_opt_in()
    );
    Some(BarState {
        state: grid.edit_state(),
        pending: edit.pending(),
        status: edit.status_text(),
        blocked: edit.apply_blocked().map(str::to_owned),
        reason: edit.read_only_message(),
        keyless: edit.is_keyless(),
        can_opt_in,
        production: context.production,
    })
}

/// The edit bar of a grid: status, the read-only reason, and the buttons. `None` for a grid with
/// no source (nothing to edit, nothing to say).
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn edit_bar(
    view: &WorkspaceView,
    key: GridKey,
    table: &Table,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let bar = bar_state(table, cx)?;
    if bar.state == EditState::Loading {
        return None;
    }
    let theme = cx.theme();
    let (border, muted, danger, warning) = (
        theme.border,
        theme.muted_foreground,
        theme.danger,
        theme.warning,
    );
    let applying = view.workspace().is_applying(key.tab);
    let editable = bar.state == EditState::Editable;
    let mut row = div()
        .id(("edit-bar", key.tab.raw()))
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(border);
    if editable {
        let text = bar.status.clone().unwrap_or_else(|| "Editable".to_owned());
        row = row.child(div().text_sm().child(text));
        if bar.keyless {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(warning)
                    .child("rows matched on all values"),
            );
        }
    } else {
        let reason = bar.reason.clone().unwrap_or_default();
        row = row.child(
            div()
                .id(("edit-reason", key.tab.raw()))
                .text_sm()
                .text_color(muted)
                .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx))
                .child("Read-only"),
        );
    }
    if bar.production {
        row = row.child(div().text_xs().text_color(danger).child("production"));
    }
    if let Some(blocked) = bar.blocked.as_ref().filter(|_| editable) {
        row = row.child(div().text_xs().text_color(danger).child(blocked.clone()));
    }
    row = row.child(div().flex_1());
    let table = table.clone();
    if applying {
        row = row
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(format!("Applying {} statements…", bar.pending)),
            )
            .child(
                Button::new("edit-cancel-apply")
                    .label("Cancel")
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.edit_cancel_apply(key.tab, cx);
                    })),
            );
        return Some(row.into_any_element());
    }
    if bar.can_opt_in {
        let t = table.clone();
        row = row.child(
            Button::new("edit-without-key")
                .label("Edit without a key…")
                .ghost()
                .small()
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.edit_action_for(key, &t, GridAction::EditWithoutKey, window, cx);
                })),
        );
    }
    if editable {
        for (id, label, action, enabled) in [
            ("edit-add", "Add row", GridAction::AddRow, true),
            (
                "edit-preview",
                "Preview",
                GridAction::PreviewChanges,
                bar.pending > 0,
            ),
            (
                "edit-discard",
                "Discard",
                GridAction::DiscardChanges,
                bar.pending > 0,
            ),
        ] {
            let t = table.clone();
            row = row.child(
                Button::new(id)
                    .label(label)
                    .ghost()
                    .small()
                    .disabled(!enabled)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.edit_action_for(key, &t, action, window, cx);
                    })),
            );
        }
        let t = table;
        row = row.child(
            Button::new("edit-apply")
                .label("Apply")
                .primary()
                .small()
                .disabled(bar.pending == 0 || bar.blocked.is_some())
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.edit_action_for(key, &t, GridAction::ApplyChanges, window, cx);
                })),
        );
    }
    Some(row.into_any_element())
}

// ---- dialogs -----------------------------------------------------------------------------------

/// The list state for a statement list of `count` items.
pub(crate) fn new_statement_list() -> ListState {
    ListState::new(0, ListAlignment::Top, px(400.))
}

fn dialog_button(
    id: &'static str,
    label: &str,
    danger: bool,
    focus: Option<&gpui::FocusHandle>,
    theme: &Theme,
    cx: &mut Context<WorkspaceView>,
    on_click: impl Fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>) + 'static,
) -> AnyElement {
    let mut button = div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if danger { theme.danger } else { theme.border })
        .cursor_pointer();
    if danger {
        button = button.bg(theme.danger).text_color(theme.danger_foreground);
    }
    if let Some(focus) = focus {
        button = button.track_focus(focus);
    }
    button
        .child(label.to_owned())
        .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
            if event.click_count() == 1 {
                on_click(view, window, cx);
            }
        }))
        .into_any_element()
}

/// The frame of every edit dialog: scrim, panel, title, body and a row of buttons.
fn dialog_frame(
    id: &'static str,
    title: &str,
    width: f32,
    body: Vec<AnyElement>,
    footer: Vec<AnyElement>,
    theme: &Theme,
) -> AnyElement {
    scrim(id)
        .child(
            div()
                .w(px(width))
                .max_h(px(760.))
                .flex()
                .flex_col()
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .border_1()
                .border_color(theme.border)
                .rounded_lg()
                .shadow_lg()
                .child(div().px_4().pt_4().pb_2().text_lg().child(title.to_owned()))
                .children(body)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .p_4()
                        .children(footer),
                ),
        )
        .into_any_element()
}

fn note(text: String, muted: Hsla) -> AnyElement {
    div()
        .px_4()
        .pb_2()
        .text_sm()
        .text_color(muted)
        .child(text)
        .into_any_element()
}

/// One statement of the preview or the confirmation: the SQL, then each bound value.
fn statement_row(s: &PreviewStatement, theme: &Theme) -> AnyElement {
    let mut item =
        div()
            .mx_4()
            .mb_2()
            .p_2()
            .rounded_md()
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_sm()
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                match s.expected_rows {
                    Some(n) => format!("#{} · expects {n} row(s)", s.ordinal),
                    None => format!("#{}", s.ordinal),
                },
            ))
            .child(s.sql.clone());
    for p in &s.params {
        item = item.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{} {} ({}) = {}",
                    p.marker, p.column, p.type_label, p.value
                )),
        );
    }
    item.into_any_element()
}

/// Which statement list a dialog shows.
#[derive(Clone, Copy)]
enum Source {
    Preview,
    Confirm,
}

fn statements_of(view: &WorkspaceView, source: Source) -> &[PreviewStatement] {
    match source {
        Source::Preview => view
            .workspace()
            .edit_preview()
            .map_or(&[][..], |p| p.statements.as_slice()),
        Source::Confirm => view
            .workspace()
            .edit_confirm()
            .map_or(&[][..], |c| c.statements.as_slice()),
    }
}

/// The virtualized list of statements of a dialog.
fn statement_list(
    state: &ListState,
    source: Source,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    div()
        .h(px(LIST_HEIGHT))
        .mx_0()
        .child(
            list(
                state.clone(),
                cx.processor(move |view, ix: usize, _window, cx| {
                    let theme = cx.theme();
                    statements_of(view, source)
                        .get(ix)
                        .map_or_else(|| div().into_any_element(), |s| statement_row(s, theme))
                }),
            )
            .size_full(),
        )
        .into_any_element()
}

/// The overlay to show, if any: the production confirmation, else the preview, else the keyless
/// warning, else the close prompt.
#[allow(clippy::too_many_lines)] // one declarative element tree per dialog
pub(crate) fn edit_overlay(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let ws = view.workspace();
    let theme = cx.theme().clone();
    let muted = theme.muted_foreground;
    let (cancel_focus, preview_list, confirm_list) = view.edit_dialog_parts();
    if let Some(confirm) = ws.edit_confirm() {
        let body = vec![
            note(
                format!(
                    "{} is labelled production. These {} statement(s) will run in one transaction.",
                    confirm.connection,
                    confirm.data_statements()
                ),
                muted,
            ),
            statement_list(confirm_list, Source::Confirm, cx),
            note(confirm.notice().to_owned(), muted),
        ];
        let cancel = dialog_button(
            "edit-confirm-cancel",
            "Cancel",
            false,
            Some(cancel_focus),
            &theme,
            cx,
            |v, _, cx| {
                v.edit_confirm_dismiss(cx);
            },
        );
        let footer = vec![
            cancel,
            dialog_button(
                "edit-confirm-apply",
                "Apply",
                true,
                None,
                &theme,
                cx,
                |v, window, cx| {
                    v.edit_confirm_apply(window, cx);
                },
            ),
        ];
        return Some(dialog_frame(
            "edit-confirm-scrim",
            "Apply to production?",
            680.,
            body,
            footer,
            &theme,
        ));
    }
    if let Some(preview) = ws.edit_preview() {
        let body = vec![
            note(
                format!(
                    "{} statement(s), in this order. Values are bound, never pasted into the SQL.",
                    preview.statements.len()
                ),
                muted,
            ),
            statement_list(preview_list, Source::Preview, cx),
            note(super::LAST_WRITER_WINS.to_owned(), muted),
        ];
        let close = dialog_button(
            "edit-preview-close",
            "Close",
            false,
            Some(cancel_focus),
            &theme,
            cx,
            |v, _, cx| {
                v.edit_preview_close(cx);
            },
        );
        let footer = vec![
            dialog_button(
                "edit-preview-copy",
                "Copy SQL",
                false,
                None,
                &theme,
                cx,
                |v, _, cx| {
                    v.edit_preview_copy(cx);
                },
            ),
            close,
            dialog_button(
                "edit-preview-apply",
                "Apply",
                false,
                None,
                &theme,
                cx,
                |v, window, cx| {
                    v.edit_preview_apply(window, cx);
                },
            ),
        ];
        return Some(dialog_frame(
            "edit-preview-scrim",
            "Pending changes",
            680.,
            body,
            footer,
            &theme,
        ));
    }
    if view.edit_keyless_pending() {
        let body = vec![note(KEYLESS_WARNING.to_owned(), muted)];
        let cancel = dialog_button(
            "edit-keyless-cancel",
            "Cancel",
            false,
            Some(cancel_focus),
            &theme,
            cx,
            |v, _, cx| {
                v.edit_keyless_dismiss(cx);
            },
        );
        let footer = vec![
            cancel,
            dialog_button(
                "edit-keyless-accept",
                "Edit without a key",
                true,
                None,
                &theme,
                cx,
                WorkspaceView::edit_keyless_accept,
            ),
        ];
        return Some(dialog_frame(
            "edit-keyless-scrim",
            "Edit without a key?",
            560.,
            body,
            footer,
            &theme,
        ));
    }
    if let Some(prompt) = ws.close_prompt() {
        let what = match prompt.target {
            CloseTarget::Tab(_) => "Closing this tab",
            CloseTarget::Quit => "Quitting",
        };
        let body = vec![note(
            format!(
                "{what} discards {} pending change(s). Nothing has been written to the database.",
                prompt.pending
            ),
            muted,
        )];
        let keep = dialog_button(
            "edit-close-keep",
            "Keep open",
            false,
            Some(cancel_focus),
            &theme,
            cx,
            |v, _, cx| {
                v.edit_close_dismiss(cx);
            },
        );
        let footer = vec![
            keep,
            dialog_button(
                "edit-close-discard",
                "Discard changes",
                true,
                None,
                &theme,
                cx,
                |v, _, cx| {
                    v.edit_close_confirm(cx);
                },
            ),
        ];
        return Some(dialog_frame(
            "edit-close-scrim",
            "Discard pending changes?",
            520.,
            body,
            footer,
            &theme,
        ));
    }
    None
}
