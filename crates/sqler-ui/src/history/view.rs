// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The views of the query history and the saved queries: the history tab body, the "saved
//! queries" section of the sidebar, the name and confirmation dialogs, and the picker of the
//! `query:open-saved` command.
//!
//! These only render state. Clicks call back into `WorkspaceView`, which goes through the
//! `Workspace` methods of `workspace/history.rs`.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::SystemTime;

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Div, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _};

use super::{HistoryCommand, HistoryDialog, HistoryRow, HistoryScope, NamePurpose, SavedLoad};
use crate::tabs::TabId;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::empty_state;
use crate::workspace::{Effect, Workspace, WorkspaceView};

/// Height of one history row, in pixels.
const ROW_HEIGHT: f32 = 28.;
/// Rows of the picker visible before it scrolls.
const PICKER_ROWS: f32 = 12.;

/// A text input and the subscription that reports its changes.
struct TextInput {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The view state of the history feature: the search box of each history tab, and the input of
/// the dialog or picker on screen.
#[derive(Default)]
pub(crate) struct HistoryViews {
    searches: HashMap<TabId, TextInput>,
    name: Option<TextInput>,
    picker: Option<TextInput>,
    overlay_open: bool,
}

impl HistoryViews {
    /// Makes the inputs match the workspace. `true` when a dialog or the picker just closed (the
    /// focus should go back to the root).
    pub(crate) fn materialize(
        &mut self,
        ws: &Workspace,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) -> bool {
        let panel = ws.panel_history();
        for tab in ws.tabs().iter().map(crate::tabs::Tab::id).chain(panel) {
            if ws.history_tab(tab).is_some() && !self.searches.contains_key(&tab) {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("Search the SQL (every word must occur)")
                });
                let subscription =
                    cx.subscribe(&input, move |view, input, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let text = input.read(cx).value().to_string();
                            view.history_do(|ws| ws.set_history_text(tab, text), cx);
                        }
                    });
                self.searches.insert(
                    tab,
                    TextInput {
                        input,
                        _subscription: subscription,
                    },
                );
            }
        }

        let wants_name = matches!(ws.history_dialog(), Some(HistoryDialog::Name { .. }));
        if !wants_name {
            self.name = None;
        } else if self.name.is_none()
            && let Some(HistoryDialog::Name { text, .. }) = ws.history_dialog()
        {
            let value = text.clone();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Name")
                    .default_value(value)
            });
            let subscription =
                cx.subscribe(&input, |view, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        view.history_do(
                            |ws| {
                                ws.set_history_dialog_text(text);
                                Effect::None
                            },
                            cx,
                        );
                    }
                    InputEvent::PressEnter { .. } => {
                        view.history_do(Workspace::submit_history_dialog, cx);
                    }
                    _ => {}
                });
            window.focus(&input.focus_handle(cx), cx);
            self.name = Some(TextInput {
                input,
                _subscription: subscription,
            });
        }

        if ws.saved_picker().is_none() {
            self.picker = None;
        } else if self.picker.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find a saved query"));
            let subscription =
                cx.subscribe(&input, |view, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        view.history_do(
                            |ws| {
                                ws.set_picker_filter(text);
                                Effect::None
                            },
                            cx,
                        );
                    }
                    InputEvent::PressEnter { .. } => {
                        view.history_do(Workspace::confirm_picker, cx);
                    }
                    _ => {}
                });
            window.focus(&input.focus_handle(cx), cx);
            self.picker = Some(TextInput {
                input,
                _subscription: subscription,
            });
        }

        let open = ws.history_dialog().is_some() || ws.saved_picker().is_some();
        let closed = self.overlay_open && !open;
        self.overlay_open = open;
        closed
    }

    /// Drops the inputs of history tabs that are gone.
    pub(crate) fn retain(&mut self, ws: &Workspace) {
        self.searches
            .retain(|tab, _| ws.history_tab(*tab).is_some());
    }
}

/// A full-window dimmed layer that centres its child and takes the clicks.
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

/// The body of a history tab: the search box, the scope toggle, the buttons and the list.
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn history_body(
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let ws = view.workspace();
    let Some(history) = ws.history_tab(tab) else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let (border, muted) = (theme.border, theme.muted_foreground);
    let all = history.scope() == HistoryScope::All;
    let selected = history.selected();
    let status = if history.searching() {
        "Searching…".to_owned()
    } else if history.results().is_empty() {
        if history.text().trim().is_empty() {
            "No history yet. Statements you run are recorded here.".to_owned()
        } else {
            "Nothing matches.".to_owned()
        }
    } else {
        format!("{} entries", history.results().len())
    };
    let rows = Rc::new(ws.history_rows(tab, SystemTime::now()));
    let count = rows.len();

    let mut toolbar = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .p_2()
        .border_b_1()
        .border_color(border);
    if let Some(search) = view.history_views.searches.get(&tab) {
        toolbar = toolbar.child(div().flex_1().min_w_0().child(Input::new(&search.input)));
    }
    toolbar = toolbar
        .child(
            Checkbox::new("history-all")
                .label("All connections")
                .checked(all)
                .on_click(cx.listener(move |view, checked: &bool, _, cx| {
                    let scope = if *checked {
                        HistoryScope::All
                    } else {
                        HistoryScope::Connection
                    };
                    view.history_do(|ws| ws.set_history_scope(tab, scope), cx);
                })),
        )
        .child(
            Button::new("history-open")
                .label("Open")
                .ghost()
                .small()
                .disabled(selected.is_none())
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(index) = selected {
                        view.history_do(|ws| ws.open_history_entry(tab, index), cx);
                    }
                })),
        )
        .child(
            Button::new("history-run")
                .label("Run again")
                .ghost()
                .small()
                .disabled(selected.is_none())
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(index) = selected {
                        view.history_do(|ws| ws.run_history_entry(tab, index), cx);
                    }
                })),
        );

    let list = uniform_list(
        "history-rows",
        count,
        cx.processor(move |view, range: Range<usize>, _window, cx| {
            let selected = view
                .workspace()
                .history_tab(tab)
                .and_then(super::HistoryTab::selected);
            range
                .filter_map(|ix| rows.get(ix).map(|row| (ix, row)))
                .map(|(ix, row)| history_row(ix, row, selected == Some(ix), all, tab, cx))
                .collect::<Vec<_>>()
        }),
    )
    .flex_1()
    .min_h_0()
    .w_full();

    div()
        .id("history-tab")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(toolbar)
        .child(
            div()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(muted)
                .child(status),
        )
        .child(list)
        .into_any_element()
}

/// One line of the history list.
fn history_row(
    ix: usize,
    row: &HistoryRow,
    selected: bool,
    show_connection: bool,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let (selected_bg, hover_bg) = (theme.list_active, theme.list_hover);
    let mono = theme.mono_font_family.clone();
    let outcome_color = if row.outcome == "Error" {
        danger
    } else {
        muted
    };
    let mut line = div()
        .id(("history-row", ix))
        .h(px(ROW_HEIGHT))
        .w_full()
        .flex_none()
        .px_3()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
            if event.click_count() >= 2 {
                view.history_do(
                    |ws| {
                        ws.select_history_entry(tab, ix);
                        ws.open_history_entry(tab, ix)
                    },
                    cx,
                );
            } else {
                view.history_do(
                    |ws| {
                        ws.select_history_entry(tab, ix);
                        Effect::None
                    },
                    cx,
                );
            }
        }))
        .child(
            div()
                .w(px(80.))
                .flex_none()
                .text_xs()
                .text_color(muted)
                .child(row.when.clone()),
        )
        .child(
            div()
                .w(px(70.))
                .flex_none()
                .text_xs()
                .text_color(outcome_color)
                .child(row.outcome),
        );
    if show_connection {
        line = line.child(
            div()
                .w(px(130.))
                .flex_none()
                .text_xs()
                .truncate()
                .child(row.connection.clone()),
        );
    }
    line = line
        .child(
            div()
                .w(px(150.))
                .flex_none()
                .text_xs()
                .text_color(muted)
                .child(row.detail.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .font_family(mono)
                .text_sm()
                .truncate()
                .child(row.sql.clone()),
        );
    if row.truncated {
        line = line.child(div().flex_none().text_xs().text_color(muted).child("cut"));
    }
    if selected {
        line = line.bg(selected_bg);
    }
    line.into_any_element()
}

/// The "saved queries" section under the connection list of the sidebar.
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn saved_section(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let saved = view.workspace().saved();
    let theme = cx.theme();
    let (fg, muted, danger) = (
        theme.sidebar_foreground,
        theme.muted_foreground,
        theme.danger,
    );
    let (selected_bg, hover_bg) = (theme.sidebar_accent, theme.list_hover);
    let mut section = div()
        .id("saved-section")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .p_2();
    match saved.load() {
        SavedLoad::Loading => {
            section = section.child(div().text_xs().text_color(muted).child("Loading…"));
        }
        SavedLoad::Failed(err) => {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(format!("Not available: {err}")),
            );
        }
        SavedLoad::Ready => {
            let queries = saved.queries().queries();
            if queries.is_empty() {
                section = section.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("No saved queries. Save the editor text with Query: Save Query"),
                );
            }
            let mut list = div()
                .id("saved-list")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .overflow_y_scroll();
            for (ix, query) in queries.iter().enumerate() {
                let id = query.id().clone();
                let label = match query.folder() {
                    Some(folder) => format!("{folder}/{}", query.name()),
                    None => query.name().to_owned(),
                };
                let mut row = div()
                    .id(("saved-row", ix))
                    .flex_none()
                    .px_1()
                    .py_0p5()
                    .rounded_sm()
                    .text_sm()
                    .truncate()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
                        let id = id.clone();
                        let double = event.click_count() >= 2;
                        view.history_do(
                            move |ws| {
                                ws.select_saved(&id);
                                if double {
                                    ws.open_saved(&id)
                                } else {
                                    Effect::None
                                }
                            },
                            cx,
                        );
                    }))
                    .child(label);
                if saved.selected() == Some(query.id()) {
                    row = row.bg(selected_bg);
                }
                list = list.child(row);
            }
            section = section.child(list);
            if saved.selected().is_some() {
                section = section.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .child(saved_button(
                            "saved-rename",
                            "Rename",
                            fg,
                            HistoryCommand::RenameSaved,
                            cx,
                        ))
                        .child(saved_button(
                            "saved-delete",
                            "Delete",
                            fg,
                            HistoryCommand::DeleteSaved,
                            cx,
                        )),
                );
            }
        }
    }
    section
}

/// The History view of the bottom panel: the panel's history, or an empty state when none is
/// bound yet.
pub(crate) fn history_panel_body(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    if view.workspace().panel_history().is_some() {
        return history_body(view, TabId::PANEL_HISTORY, cx);
    }
    div()
        .id("history-panel")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(empty_state(
            AppIcon::Clock,
            "No history for this connection yet",
            "Statements you run are recorded here.",
            cx,
        ))
        .into_any_element()
}

/// A small text button that runs a history command.
fn saved_button(
    id: &'static str,
    label: &'static str,
    color: gpui::Hsla,
    command: HistoryCommand,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .text_xs()
        .px_1()
        .rounded_sm()
        .text_color(color)
        .cursor_pointer()
        .child(label)
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.history_command(command, cx);
            cx.stop_propagation();
        }))
}

/// The overlay of the open history dialog or picker; `None` when neither is open.
pub(crate) fn history_overlay(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let ws = view.workspace();
    if let Some(picker) = ws.saved_picker() {
        return Some(picker_overlay(view, picker, cx));
    }
    match ws.history_dialog()? {
        HistoryDialog::Name {
            purpose,
            text: _,
            error,
        } => {
            let (title, action) = match purpose {
                NamePurpose::Save { .. } => ("Save query", "Save"),
                NamePurpose::Rename(_) => ("Rename saved query", "Rename"),
            };
            let input = view.history_views.name.as_ref().map(|n| n.input.clone());
            Some(name_dialog(
                title,
                action,
                input.as_ref(),
                error.as_deref(),
                cx,
            ))
        }
        HistoryDialog::ConfirmDelete { name, .. } => Some(confirm_dialog(
            "Delete saved query?",
            &format!("\"{name}\" will be removed from queries.toml."),
            "Delete",
            cx,
        )),
        HistoryDialog::ConfirmClear => Some(confirm_dialog(
            "Clear the query history?",
            "Every recorded statement is deleted from history.jsonl. This cannot be undone.",
            "Clear history",
            cx,
        )),
    }
}

/// The footer button that closes the dialog without a change.
fn cancel_button(cx: &mut Context<WorkspaceView>) -> Button {
    Button::new("history-dialog-cancel")
        .label("Cancel")
        .small()
        .on_click(cx.listener(|view, _, _, cx| {
            view.history_do(
                |ws| {
                    ws.cancel_history_dialog();
                    Effect::None
                },
                cx,
            );
        }))
}

fn name_dialog(
    title: &'static str,
    action: &'static str,
    input: Option<&Entity<InputState>>,
    error: Option<&str>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (panel_bg, border, fg, danger) = (
        theme.popover,
        theme.border,
        theme.popover_foreground,
        theme.danger,
    );
    let mut body = div().flex().flex_col().gap_2().px_4().py_2();
    if let Some(input) = input {
        body = body.child(Input::new(input));
    }
    if let Some(error) = error {
        body = body.child(div().text_xs().text_color(danger).child(error.to_owned()));
    }
    let submit = Button::new("history-dialog-ok")
        .label(action)
        .small()
        .primary()
        .on_click(cx.listener(|view, event: &ClickEvent, _, cx| {
            if event.click_count() == 1 {
                view.history_do(Workspace::submit_history_dialog, cx);
            }
        }));
    scrim("history-scrim")
        .child(
            div()
                .id("history-dialog")
                .w(px(460.))
                .flex()
                .flex_col()
                .bg(panel_bg)
                .text_color(fg)
                .border_1()
                .border_color(border)
                .rounded_lg()
                .shadow_lg()
                .child(div().px_4().pt_4().pb_1().text_lg().child(title))
                .child(body)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .p_4()
                        .child(cancel_button(cx))
                        .child(submit),
                ),
        )
        .into_any_element()
}

fn confirm_dialog(
    title: &'static str,
    question: &str,
    action: &'static str,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (panel_bg, border, fg, muted) = (
        theme.popover,
        theme.border,
        theme.popover_foreground,
        theme.muted_foreground,
    );
    let confirm = Button::new("history-dialog-ok")
        .label(action)
        .small()
        .danger()
        .on_click(cx.listener(|view, event: &ClickEvent, _, cx| {
            if event.click_count() == 1 {
                view.history_do(Workspace::submit_history_dialog, cx);
            }
        }));
    scrim("history-scrim")
        .child(
            div()
                .id("history-dialog")
                .w(px(460.))
                .flex()
                .flex_col()
                .bg(panel_bg)
                .text_color(fg)
                .border_1()
                .border_color(border)
                .rounded_lg()
                .shadow_lg()
                .child(div().px_4().pt_4().pb_2().text_lg().child(title))
                .child(
                    div()
                        .px_4()
                        .pb_2()
                        .text_sm()
                        .text_color(muted)
                        .child(question.to_owned()),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .p_4()
                        .child(cancel_button(cx))
                        .child(confirm),
                ),
        )
        .into_any_element()
}

/// The `query:open-saved` picker: a filter box over the saved queries, like the palette.
fn picker_overlay(
    view: &WorkspaceView,
    picker: &super::SavedPicker,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (bg, fg, border, muted) = (
        theme.popover,
        theme.popover_foreground,
        theme.border,
        theme.muted_foreground,
    );
    let (selected_bg, hover_bg) = (theme.list_active, theme.list_hover);
    let queries = view.workspace().saved().queries();
    let matches = picker.matches(queries);
    let current = picker.selected();
    let mut list = div()
        .id("picker-list")
        .flex()
        .flex_col()
        .max_h(px(ROW_HEIGHT * PICKER_ROWS))
        .overflow_y_scroll();
    if matches.is_empty() {
        list = list.child(
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(muted)
                .child("No saved query matches."),
        );
    }
    for (ix, query) in matches.iter().enumerate() {
        let label = match query.folder() {
            Some(folder) => format!("{folder}/{}", query.name()),
            None => query.name().to_owned(),
        };
        let mut row = div()
            .id(("picker-row", ix))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .px_3()
            .flex()
            .flex_row()
            .items_center()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.history_do(
                    |ws| {
                        // Move the highlight to the clicked row, then open it.
                        let from = ws.saved_picker().map_or(0, super::SavedPicker::selected);
                        ws.move_picker(
                            isize::try_from(ix).unwrap_or(0) - isize::try_from(from).unwrap_or(0),
                        );
                        ws.confirm_picker()
                    },
                    cx,
                );
            }))
            .child(div().text_sm().child(label));
        if ix == current {
            row = row.bg(selected_bg);
        }
        list = list.child(row);
    }
    let mut panel = div()
        .id("picker")
        .w(px(600.))
        .flex()
        .flex_col()
        .bg(bg)
        .text_color(fg)
        .border_1()
        .border_color(border)
        .rounded_md()
        .shadow_lg();
    if let Some(input) = &view.history_views.picker {
        panel = panel.child(div().p_2().child(Input::new(&input.input)));
    }
    scrim("picker-scrim")
        .items_start()
        .pt(px(48.))
        .child(panel.child(list))
        .into_any_element()
}
