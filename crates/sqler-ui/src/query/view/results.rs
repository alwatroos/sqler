// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The results area of a query tab, and the view state behind the query tabs (the result tables
//! and the inputs of the parameter prompt).

use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement as _, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::InputState;
use gpui_component::table::{DataTable, TableState};
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _};

use crate::grid::view::{GridDelegate, Table, start_polling};
use crate::grid::{GridTab, IngestState};
use crate::query::{Phase, QueryTab, StatementStatus};
use crate::tabs::{TabId, TabKind, TabStrip};
use crate::workspace::{Workspace, WorkspaceView};

/// The tables of the last run of one query tab.
struct Results {
    /// One table per statement, in run order.
    tables: Vec<Table>,
    /// The result the user picked; `None` follows the run.
    chosen: Option<usize>,
}

/// The inputs of the parameter prompt on screen.
struct ParamInputs {
    tab: TabId,
    inputs: Vec<Entity<InputState>>,
}

/// What a closed overlay hands back to the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayChange {
    /// Nothing changed.
    None,
    /// The prompt or the confirmation went away: give the focus back to the editor.
    Closed,
}

/// The view state of every query tab.
#[derive(Default)]
pub(crate) struct QueryViews {
    /// Grids of a new run, waiting for a window to make their tables.
    pending: Vec<(TabId, Vec<GridTab>)>,
    results: HashMap<TabId, Results>,
    params: Option<ParamInputs>,
    confirm_focus: Option<FocusHandle>,
    overlay_open: bool,
}

impl QueryViews {
    /// Takes the grids of a new run; they replace the tab's earlier results.
    pub(crate) fn insert_grids(&mut self, tab: TabId, grids: Vec<GridTab>) {
        self.results.remove(&tab);
        self.pending.retain(|(id, _)| *id != tab);
        self.pending.push((tab, grids));
    }

    /// Drops the state of tabs that are gone. Dropping a table drops its model.
    pub(crate) fn retain(&mut self, tabs: &TabStrip) {
        let alive = |id: &TabId| tabs.get(*id).is_some();
        self.pending.retain(|(id, _)| alive(id));
        self.results.retain(|id, _| alive(id));
        if self.params.as_ref().is_some_and(|p| !alive(&p.tab)) {
            self.params = None;
        }
    }

    /// The text the user typed for each parameter of the open prompt.
    pub(crate) fn param_texts(&self, tab: TabId, cx: &App) -> Vec<String> {
        self.params
            .as_ref()
            .filter(|p| p.tab == tab)
            .map(|p| {
                p.inputs
                    .iter()
                    .map(|i| i.read(cx).value().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The input of parameter `index` of the open prompt.
    pub(super) fn param_input(&self, tab: TabId, index: usize) -> Option<&Entity<InputState>> {
        self.params
            .as_ref()
            .filter(|p| p.tab == tab)
            .and_then(|p| p.inputs.get(index))
    }

    /// The focus handle of the confirmation's Cancel button.
    pub(super) fn confirm_focus(&self) -> Option<&FocusHandle> {
        self.confirm_focus.as_ref()
    }

    /// Picks the result the area shows.
    pub(crate) fn choose(&mut self, tab: TabId, index: usize) {
        if let Some(results) = self.results.get_mut(&tab) {
            results.chosen = Some(index);
        }
    }

    /// Fetches the next page of the paused table of a tab. A script holds at its paused statement,
    /// so at most one table is paused. Returns `false` when none is.
    pub(crate) fn fetch_more(&self, tab: TabId, cx: &mut Context<WorkspaceView>) -> bool {
        let Some(results) = self.results.get(&tab) else {
            return false;
        };
        let paused = results
            .tables
            .iter()
            .find(|t| matches!(t.read(cx).delegate().grid().state(), IngestState::Paused));
        let Some(table) = paused else {
            return false;
        };
        table.update(cx, |state, cx| {
            let fetched = state.delegate_mut().grid_mut().fetch_more();
            if fetched {
                cx.notify();
            }
            fetched
        })
    }

    fn table(&self, tab: TabId, index: usize) -> Option<&Table> {
        self.results.get(&tab)?.tables.get(index)
    }

    fn chosen(&self, tab: TabId) -> Option<usize> {
        self.results.get(&tab)?.chosen
    }

    /// Creates what the next frame needs: tables for new grids, the inputs of the parameter
    /// prompt and the focus of the confirmation.
    pub(crate) fn materialize(
        &mut self,
        ws: &Workspace,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) -> OverlayChange {
        for (id, grids) in std::mem::take(&mut self.pending) {
            let tables = grids
                .into_iter()
                .map(|grid| {
                    let table = cx.new(|cx| {
                        TableState::new(GridDelegate::new(grid), window, cx)
                            .col_movable(false)
                            .col_resizable(false)
                            .sortable(false)
                    });
                    start_polling(&table, cx);
                    table
                })
                .collect();
            self.results.insert(
                id,
                Results {
                    tables,
                    chosen: None,
                },
            );
        }
        self.sync_overlay(ws, window, cx)
    }

    fn sync_overlay(
        &mut self,
        ws: &Workspace,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) -> OverlayChange {
        let active = ws
            .tabs()
            .active()
            .filter(|t| t.kind() == TabKind::Query)
            .map(crate::tabs::Tab::id);
        let phase = active.and_then(|id| ws.query_tab(id)).map(QueryTab::phase);
        match (active, phase) {
            (Some(tab), Some(Phase::Prompting(form))) => {
                let fresh = self
                    .params
                    .as_ref()
                    .is_none_or(|p| p.tab != tab || p.inputs.len() != form.rows().len());
                if fresh {
                    let inputs: Vec<_> = form
                        .rows()
                        .iter()
                        .map(|row| {
                            let text = row.text.clone();
                            cx.new(|cx| {
                                let mut state = InputState::new(window, cx).placeholder("value");
                                state.set_value(text, window, cx);
                                state
                            })
                        })
                        .collect();
                    if let Some(first) = inputs.first() {
                        window.focus(&first.focus_handle(cx), cx);
                    }
                    self.params = Some(ParamInputs { tab, inputs });
                }
                self.overlay_open = true;
                OverlayChange::None
            }
            (_, Some(Phase::Confirming(_))) => {
                self.params = None;
                let handle = self
                    .confirm_focus
                    .get_or_insert_with(|| cx.focus_handle())
                    .clone();
                if !self.overlay_open {
                    window.focus(&handle, cx);
                }
                self.overlay_open = true;
                OverlayChange::None
            }
            _ => {
                self.params = None;
                if std::mem::take(&mut self.overlay_open) {
                    OverlayChange::Closed
                } else {
                    OverlayChange::None
                }
            }
        }
    }
}

/// The index of the result to show when the user has not picked one: the last statement that has
/// started.
fn followed(statuses: impl Iterator<Item = bool>) -> usize {
    statuses
        .enumerate()
        .filter(|(_, started)| *started)
        .map(|(i, _)| i)
        .last()
        .unwrap_or(0)
}

/// The area below the editor: the result strip, the status line and the active result.
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(super) fn results_area(
    view: &WorkspaceView,
    tab: TabId,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (border, muted, danger) = (theme.border, theme.muted_foreground, theme.danger);
    let (active_bg, hover_bg) = (theme.list_active, theme.list_hover);
    let area = div()
        .id("query-results")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col();
    let Some(query) = view.workspace().query_tab(tab) else {
        return area.into_any_element();
    };
    let entries = query.results();
    if entries.is_empty() {
        return area
            .items_center()
            .justify_center()
            .text_color(muted)
            .child("Run the statement at the cursor, the selection or the whole script with the Query commands.")
            .into_any_element();
    }
    let index =
        view.queries
            .chosen(tab)
            .unwrap_or_else(|| {
                followed(entries.iter().map(|e| {
                    !matches!(e.status, StatementStatus::Pending | StatementStatus::NotRun)
                }))
            })
            .min(entries.len() - 1);
    let entry = &entries[index];

    let mut strip = div()
        .id("query-strip")
        .flex_none()
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(border)
        .overflow_hidden();
    for (i, e) in entries.iter().enumerate() {
        let mut chip = div()
            .id(("query-result", i))
            .px_2()
            .py_0p5()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                view.queries.choose(tab, i);
                cx.notify();
            }))
            .child(e.title.clone());
        if i == index {
            chip = chip.bg(active_bg);
        }
        if matches!(e.status, StatementStatus::Failed { .. }) {
            chip = chip.text_color(danger);
        }
        strip = strip.child(chip);
    }
    strip = strip.child(div().flex_1());
    let running_text = match query.phase() {
        Phase::Running {
            cancelling: true, ..
        } => Some("Cancelling…"),
        Phase::Running { .. } => Some("Running…"),
        _ => None,
    };
    if let Some(text) = running_text {
        strip = strip.child(div().text_sm().text_color(muted).child(text));
        let cancelling = text == "Cancelling…";
        strip = strip.child(
            Button::new("query-cancel")
                .label("Cancel")
                .ghost()
                .small()
                .disabled(cancelling)
                .on_click(cx.listener(move |view, _, _, cx| view.query_cancel(tab, cx))),
        );
    }

    // The shown table stopped at its page limit: say so, and offer the next page.
    let paused_text = view
        .queries
        .table(tab, index)
        .map(|t| t.read(cx).delegate().grid())
        .filter(|g| matches!(g.state(), IngestState::Paused))
        .map(GridTab::status_text);
    if paused_text.is_some() {
        strip = strip.child(
            Button::new("query-fetch-more")
                .label("Fetch more")
                .small()
                .on_click(cx.listener(move |view, _, _, cx| {
                    if view.queries.fetch_more(tab, cx) {
                        cx.notify();
                    }
                })),
        );
    }

    let status_color = if matches!(entry.status, StatementStatus::Failed { .. }) {
        danger
    } else {
        muted
    };
    let status = div()
        .id("query-status")
        .flex_none()
        .w_full()
        .px_2()
        .py_1()
        .text_sm()
        .text_color(status_color)
        .border_b_1()
        .border_color(border)
        .child(paused_text.unwrap_or_else(|| entry.status_text()));

    let table = view
        .queries
        .table(tab, index)
        .filter(|t| t.read(cx).delegate().grid().column_count() > 0);
    let body = match table {
        Some(table) => div()
            .flex_1()
            .min_h_0()
            .child(DataTable::new(table).stripe(true).bordered(true))
            .into_any_element(),
        None => div().flex_1().into_any_element(),
    };
    area.child(strip)
        .child(status)
        .child(body)
        .into_any_element()
}
