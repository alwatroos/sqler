// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The results area of a query tab, and the view state behind the query tabs (the result tables
//! and the inputs of the parameter prompt).

use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement as _, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::InputState;
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _};

use crate::grid::edit::GridKey;
use crate::grid::edit::view::edit_row;
use crate::grid::view::{Table, grid_table, new_table};
use crate::grid::{GridTab, IngestState};
use crate::query::{Phase, QueryTab, StatementStatus, StatusTone, status_tone, status_word};
use crate::tabs::{TabId, TabKind, TabStrip};
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::workspace::chrome::{ICON_MD, ICON_SM, TOOLBAR_HEIGHT, ui_scale};
use crate::workspace::{Workspace, WorkspaceView};

/// Width of the line between two segments.
const SEPARATOR_W: f32 = 1.;

/// The colour role of a segment icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tint {
    Muted,
    Success,
    Danger,
    Warning,
}

/// The icon and colour role of a segment: the status is never carried by colour alone.
fn segment_icon(tone: StatusTone) -> (AppIcon, Tint) {
    match tone {
        StatusTone::Pending => (AppIcon::Clock, Tint::Muted),
        StatusTone::Running => (AppIcon::LoaderCircle, Tint::Muted),
        StatusTone::Done => (AppIcon::Check, Tint::Success),
        StatusTone::Failed => (AppIcon::CircleAlert, Tint::Danger),
        StatusTone::Cancelled => (AppIcon::CircleStop, Tint::Warning),
        StatusTone::NotRun => (AppIcon::Ellipsis, Tint::Muted),
    }
}

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

    /// The index of the result a query tab shows: the one the user picked, else the last
    /// statement that has started. `None` while there are no results.
    pub(crate) fn shown_index(&self, ws: &Workspace, tab: TabId) -> Option<usize> {
        let entries = ws.query_tab(tab)?.results();
        if entries.is_empty() {
            return None;
        }
        let index =
            self.chosen(tab).unwrap_or_else(|| {
                followed(entries.iter().map(|e| {
                    !matches!(e.status, StatementStatus::Pending | StatementStatus::NotRun)
                }))
            });
        Some(index.min(entries.len() - 1))
    }

    /// The table of the result a query tab shows, if it has one.
    pub(crate) fn shown_table(&self, ws: &Workspace, tab: TabId) -> Option<Table> {
        self.table(tab, self.shown_index(ws, tab)?).cloned()
    }

    pub(crate) fn table(&self, tab: TabId, index: usize) -> Option<&Table> {
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
                .map(|grid| new_table(grid, window, cx))
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
    let (fg, success, warning) = (theme.foreground, theme.success, theme.warning);
    let (active_bg, hover_bg) = (theme.list_active, theme.list_hover);
    let (ring, radius) = (theme.ring, theme.radius);
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
    let index = view.queries.shown_index(view.workspace(), tab).unwrap_or(0);
    let entry = &entries[index];

    let mut strip = div()
        .id("query-strip")
        .flex_none()
        .w_full()
        .h(px(TOOLBAR_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .px(px(SPACE[3]))
        .border_b_1()
        .border_color(border);
    let sm = ui_scale(cx).sm;
    let mut group = div()
        .id("query-results-tabs")
        .role(Role::TabList)
        .aria_label("Statement results")
        .h(px(ICON_MD + SPACE[3]))
        .flex()
        .flex_row()
        .flex_shrink(1.)
        .min_w_0()
        .overflow_x_scroll()
        .border_1()
        .border_color(border)
        .rounded(radius);
    for (i, e) in entries.iter().enumerate() {
        let tone = status_tone(&e.status);
        let active = i == index;
        let (icon, tint) = segment_icon(tone);
        let tint = match tint {
            Tint::Muted => muted,
            Tint::Success => success,
            Tint::Danger => danger,
            Tint::Warning => warning,
        };
        let glyph = if tone == StatusTone::Running {
            Spinner::new()
                .icon(icon)
                .color(muted)
                .xsmall()
                .into_any_element()
        } else {
            Icon::new(icon)
                .size(px(ICON_SM))
                .text_color(tint)
                .into_any_element()
        };
        let title_color = match (tone, active) {
            (StatusTone::Failed, _) => danger,
            (_, true) => fg,
            _ => muted,
        };
        let status_text = e.status_text();
        if i > 0 {
            group = group.child(div().w(px(SEPARATOR_W)).h_full().flex_none().bg(border));
        }
        group = group.child(
            div()
                .id(("query-result", i))
                .flex_none()
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                .px(px(SPACE[3]))
                .gap(px(SPACE[2]))
                .border_1()
                .border_color(gpui::transparent_black())
                .text_size(px(sm))
                .text_color(title_color)
                .cursor_pointer()
                .when(active, |el| {
                    el.bg(active_bg).font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |el| el.hover(move |s| s.bg(hover_bg)))
                .focusable()
                .tab_stop(true)
                .focus_visible(move |s| s.border_color(ring))
                .role(Role::Tab)
                .aria_label(format!("{}, {}", e.title, status_word(tone)))
                .tooltip(move |window, cx| Tooltip::new(status_text.clone()).build(window, cx))
                .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                    view.queries.choose(tab, i);
                    cx.notify();
                }))
                .child(glyph)
                .child(e.title.clone()),
        );
    }
    strip = strip.child(group);
    let mut controls: Vec<AnyElement> = Vec::new();
    let running_text = match query.phase() {
        Phase::Running {
            cancelling: true, ..
        } => Some("Cancelling…"),
        Phase::Running { .. } => Some("Running…"),
        _ => None,
    };
    if let Some(text) = running_text {
        controls.push(
            div()
                .text_size(px(sm))
                .text_color(muted)
                .child(text)
                .into_any_element(),
        );
        let cancelling = text == "Cancelling…";
        controls.push(
            Button::new("query-cancel")
                .label("Cancel")
                .ghost()
                .small()
                .disabled(cancelling)
                .on_click(cx.listener(move |view, _, _, cx| view.query_cancel(tab, cx)))
                .into_any_element(),
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
        controls.push(
            Button::new("query-fetch-more")
                .label("Fetch more")
                .small()
                .on_click(cx.listener(move |view, _, _, cx| {
                    if view.queries.fetch_more(tab, cx) {
                        cx.notify();
                    }
                }))
                .into_any_element(),
        );
    }

    let table = view
        .queries
        .table(tab, index)
        .filter(|t| t.read(cx).delegate().grid().column_count() > 0);
    let edit = table.and_then(|t| {
        let key = GridKey {
            tab,
            result: Some(index),
        };
        edit_row(view, key, t, cx)
    });
    let body = match table {
        Some(table) => grid_table(table, cx).into_any_element(),
        None => div().flex_1().into_any_element(),
    };
    // One toolbar row: the result chips, the status of the shown statement, then the edit
    // controls (which take the rest of the row) or the run controls.
    let status_color = if matches!(entry.status, StatementStatus::Failed { .. }) {
        danger
    } else {
        muted
    };
    let status = div()
        .id("query-status")
        .flex_none()
        .px(px(SPACE[2]))
        .text_size(px(sm))
        .text_color(status_color)
        .child(paused_text.unwrap_or_else(|| entry.status_text()));
    let strip = match edit {
        Some(edit) => strip
            .child(status)
            .children(controls)
            .child(edit.flex_1().min_w_0()),
        None => strip.child(status).child(div().flex_1()).children(controls),
    };
    area.child(strip).child(body).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tone_has_an_icon() {
        let tones = [
            StatusTone::Pending,
            StatusTone::Running,
            StatusTone::Done,
            StatusTone::Failed,
            StatusTone::Cancelled,
            StatusTone::NotRun,
        ];
        for tone in tones {
            let _ = segment_icon(tone);
        }
        // No two tones share an icon, so the status never rests on colour alone.
        let mut icons: Vec<_> = tones.iter().map(|t| segment_icon(*t).0).collect();
        icons.sort_by_key(|i| format!("{i:?}"));
        icons.dedup();
        assert_eq!(icons.len(), tones.len());
        assert_eq!(
            segment_icon(StatusTone::Failed),
            (AppIcon::CircleAlert, Tint::Danger)
        );
    }
}
