// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The results-grid views: a `gpui-component` virtual table over a [`GridTab`], a toolbar, and an
//! opt-in probe (`SQLER_GRID_PROBE=1`) that measures the real window.
//!
//! The delegate only asks the headless [`GridTab`] for the cells of the rows the table draws, so
//! the work per frame depends on the window height and not on the row count.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Context, Div, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, NoAction, ParentElement as _, Pixels,
    SharedString, Stateful, Styled as _, Subscription, WeakEntity, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::select::{Select, SelectState};
use gpui_component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, IndexPath, Sizable as _};

use super::layout::{MIN_COLUMN_PX, widest_text};
use super::{
    Collation, ColumnHeader, Condition, FilterOp, FilterSpec, GridAction, GridControls, GridTab,
    IngestState, OrderProgress, POLL_BUDGET, ProbeReport, QuickFilter,
};
use crate::command::CommandId;
use crate::tabs::{TabId, TabStrip};
use crate::workspace::{WorkspaceAction, WorkspaceView};

/// How often the pending messages of a grid are applied.
const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// The work of one pass of the sort and filter job on the UI thread.
const ORDER_SLICE: Duration = Duration::from_millis(6);

/// The pause between two passes, so that input and painting get their turn.
const ORDER_YIELD: Duration = Duration::from_millis(1);

/// How long after the last keystroke the quick filter is applied.
const QUICK_FILTER_DELAY: Duration = Duration::from_millis(150);

/// The operators of the condition editor, in the order of its select.
const OPS: [(FilterOp, &str); 7] = [
    (FilterOp::Equals, "= Equals"),
    (FilterOp::NotEquals, "≠ Not equals"),
    (FilterOp::Contains, "Contains"),
    (FilterOp::Greater, "> Greater"),
    (FilterOp::Less, "< Less"),
    (FilterOp::IsNull, "Is NULL"),
    (FilterOp::NotNull, "Is not NULL"),
];

/// The entities of the filter bar of one grid; their subscriptions live and die with it.
struct FilterBar {
    quick: Entity<InputState>,
    scope: Entity<SelectState<Vec<SharedString>>>,
    column: Entity<SelectState<Vec<SharedString>>>,
    op: Entity<SelectState<Vec<SharedString>>>,
    operand: Entity<InputState>,
    /// Counts the edits of the quick filter, so that only the last one of a burst applies.
    edits: u64,
    _subscriptions: Vec<Subscription>,
}

/// The grid action a command ID stands for, if it is one.
pub(crate) fn action_for_command(id: &CommandId) -> Option<GridAction> {
    GridAction::from_command_id(id)
}

/// The key context of the data table.
const TABLE_CONTEXT: &str = "DataTable";

/// Keys of the table's own bindings that the grid takes over: its key-down listener gets them.
const TABLE_KEYS: [&str; 11] = [
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "tab",
    "shift-tab",
    "escape",
];

/// Unbinds the key bindings of the data table, so the keys reach the grid's key-down listener.
pub(crate) fn init_keys(cx: &mut App) {
    cx.bind_keys(
        TABLE_KEYS
            .into_iter()
            .map(|k| KeyBinding::new(k, NoAction, Some(TABLE_CONTEXT))),
    );
}

/// The table delegate: a thin wrapper around a [`GridTab`].
pub(crate) struct GridDelegate {
    tab: GridTab,
    controls: GridControls,
    columns: Vec<Column>,
    /// The workspace to repaint while the sort and filter job runs.
    workspace: WeakEntity<WorkspaceView>,
    /// Whether a pump task is running for the sort and filter job.
    pumping: bool,
    bar: Option<FilterBar>,
}

impl GridDelegate {
    pub(crate) fn new(tab: GridTab, workspace: WeakEntity<WorkspaceView>) -> Self {
        Self {
            tab,
            controls: GridControls::default(),
            columns: Vec::new(),
            workspace,
            pumping: false,
            bar: None,
        }
    }

    /// The grid the table shows.
    pub(crate) fn grid(&self) -> &GridTab {
        &self.tab
    }

    /// The grid the table shows, for actions that change it.
    pub(crate) fn grid_mut(&mut self) -> &mut GridTab {
        &mut self.tab
    }

    /// Whether the motion would change the active cell.
    pub(crate) fn motion_moves(&self, to: super::selection::Motion) -> bool {
        self.controls
            .motion_moves(to, &self.tab.rows(), self.tab.visible())
    }

    /// Rebuilds the column list, in display order, from the tab's headers and the layout.
    fn rebuild_columns(&mut self) {
        let headers: Vec<ColumnHeader> = (0..self.tab.column_count())
            .filter_map(|ix| self.tab.header(ix).cloned())
            .collect();
        self.controls.sync_columns(&headers);
        let layout = self.controls.layout();
        let pinned = layout.pinned_count();
        self.columns = (0..layout.len())
            .filter_map(|display| {
                let model = layout.model_column(display)?;
                let header = headers.get(model)?;
                let column = Column::new(format!("c{model}"), header.name.clone())
                    .width(px(layout.width(model)))
                    .min_width(px(MIN_COLUMN_PX));
                Some(if display < pinned {
                    column.fixed_left()
                } else {
                    column
                })
            })
            .collect();
    }

    /// The model column drawn at a display position.
    fn model_col(&self, display: usize) -> usize {
        self.controls
            .layout()
            .model_column(display)
            .unwrap_or(usize::MAX)
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.tab.display_count()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns.get(col_ix).cloned().unwrap_or_default()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let (name, label) = self
            .tab
            .header(self.model_col(col_ix))
            .map_or(("", ""), |h| (h.name.as_str(), h.type_label));
        let model_col = self.model_col(col_ix);
        let indicator = self.tab.order().sort().indicator(model_col);
        let sorted = indicator.is_some();
        let fg = cx.theme().foreground;
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .whitespace_nowrap()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |state, e: &MouseDownEvent, window, cx| {
                    let d = state.delegate_mut();
                    let rows = d.tab.rows();
                    let hit = d.controls.click_header(col_ix, &rows, e.modifiers.shift);
                    if hit {
                        window.focus(&state.focus_handle(cx), cx);
                        cx.notify();
                    }
                }),
            )
            .child(name.to_owned())
            .child(div().text_xs().text_color(muted).child(label.to_owned()))
            .child(div().flex_1())
            .child(
                // The sort indicator: a click on it sorts; the rest of the header selects the
                // column.
                div()
                    .px_1()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(if sorted { fg } else { muted })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |state, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            let d = state.delegate_mut();
                            let mut spec = d.tab.order().sort().clone();
                            spec.click(model_col, e.modifiers.shift);
                            d.tab.order_mut().set_sort(spec);
                            ensure_pump(state, cx);
                        }),
                    )
                    .child(indicator.unwrap_or_else(|| "↕".to_owned())),
            )
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let rows = self.tab.display_count();
        self.tab.stats_mut().row_rendered(row_ix, rows);
        div().id(("row", row_ix))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let model_row = self.tab.model_row(row_ix);
        let selected = self.controls.is_selected(row_ix, col_ix, &self.tab.rows());
        let active = self.controls.selection().active().is_some_and(|c| {
            Some(c.row) == model_row && self.controls.layout().display_column(c.col) == Some(col_ix)
        });
        let tokens = &cx.theme().tokens;
        let (fill, border) = (tokens.table_active, tokens.table_active_border);
        let mut cell = div()
            .size_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |state, e: &MouseDownEvent, window, cx| {
                    let d = state.delegate_mut();
                    let rows = d.tab.rows();
                    let hit = d
                        .controls
                        .click_cell(row_ix, col_ix, &rows, e.modifiers.shift);
                    if hit {
                        window.focus(&state.focus_handle(cx), cx);
                        cx.notify();
                    }
                }),
            );
        if selected {
            cell = cell.bg(fill);
        }
        if active {
            cell = cell.border_1().border_color(border);
        }
        cell.child(model_row.map_or_else(String::new, |row| {
            self.tab.cell_text(row, self.model_col(col_ix))
        }))
    }

    fn visible_rows_changed(
        &mut self,
        visible_range: std::ops::Range<usize>,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.tab.set_visible(visible_range);
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.tab.model_row(row_ix).map_or_else(String::new, |row| {
            self.tab.cell_text(row, self.model_col(col_ix))
        })
    }

    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        if self.controls.move_display(col_ix, to_ix) {
            self.rebuild_columns();
        }
    }

    fn fit_column(
        &mut self,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<Pixels> {
        let model = self.controls.layout().model_column(col_ix)?;
        let header = self.tab.header(model)?;
        let header_text = format!("{} {}", header.name, header.type_label);
        let cells: Vec<String> = self
            .tab
            .visible()
            .filter_map(|row| self.tab.model_row(row))
            .map(|row| self.tab.cell_text(row, model))
            .collect();
        let widest = widest_text(
            std::iter::once(header_text.as_str()).chain(cells.iter().map(String::as_str)),
        )?;
        let font_size = cx.theme().font_size;
        let run = window.text_style().to_run(widest.len());
        let line = window.text_system().shape_line(
            SharedString::from(widest.to_owned()),
            font_size,
            &[run],
            None,
        );
        Some(line.width)
    }
}

/// A table entity over a grid.
pub(crate) type Table = Entity<TableState<GridDelegate>>;

/// The grid tabs of the workspace: tabs waiting for a window and the table entity of each.
#[derive(Default)]
pub(crate) struct GridViews {
    pending: Vec<(TabId, GridTab)>,
    tables: HashMap<TabId, Table>,
}

impl GridViews {
    /// Takes over a new grid tab; the table entity is created on the next render.
    pub(crate) fn insert(&mut self, tab: TabId, grid: GridTab) {
        self.pending.push((tab, grid));
    }

    /// Drops the grids of tabs that are gone. Dropping a table drops its model.
    pub(crate) fn retain(&mut self, tabs: &TabStrip) {
        self.pending.retain(|(id, _)| tabs.get(*id).is_some());
        self.tables.retain(|id, _| tabs.get(*id).is_some());
    }

    /// The table entity of a tab, once materialized.
    pub(crate) fn table(&self, tab: TabId) -> Option<&Table> {
        self.tables.get(&tab)
    }

    /// Creates the table entities for pending grids and starts their poll tasks.
    pub(crate) fn materialize(&mut self, window: &mut Window, cx: &mut Context<WorkspaceView>) {
        for (id, tab) in std::mem::take(&mut self.pending) {
            self.tables.insert(id, new_table(tab, window, cx));
        }
    }

    /// Runs a grid action on the grid of a tab. Returns whether it did anything.
    pub(crate) fn run_action(
        &self,
        tab: TabId,
        action: GridAction,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) -> bool {
        let Some(table) = self.tables.get(&tab) else {
            return false;
        };
        table.update(cx, |state, cx| apply_action(state, action, window, cx))
    }

    /// The body of a grid tab: toolbar above the table.
    pub(crate) fn body(&self, tab: TabId, cx: &mut Context<WorkspaceView>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let Some(table) = self.tables.get(&tab) else {
            return div()
                .id("grid-body")
                .flex_1()
                .w_full()
                .text_color(muted)
                .child("Preparing the results…");
        };
        let (status, range, paused) = {
            let grid = &table.read(cx).delegate().tab;
            (
                grid.status_text(),
                grid.range_text(),
                matches!(grid.state(), IngestState::Paused),
            )
        };
        let toolbar = div()
            .id("grid-toolbar")
            .w_full()
            .h(px(36.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_2()
            .border_b_1()
            .border_color(border)
            .child(div().text_sm().child(status))
            .children(range.map(|r| div().text_sm().text_color(muted).child(r)))
            .child(div().flex_1())
            .children(paused.then(|| {
                Button::new("grid-fetch-more")
                    .label("Fetch more")
                    .small()
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.grids
                            .run_action(tab, GridAction::FetchMore, window, cx);
                    }))
            }))
            .child(
                Button::new("grid-top")
                    .label("Top")
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.grids
                            .run_action(tab, GridAction::ScrollToTop, window, cx);
                    })),
            )
            .child(
                Button::new("grid-end")
                    .label("End")
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.grids
                            .run_action(tab, GridAction::ScrollToEnd, window, cx);
                    })),
            );
        div()
            .id("grid-body")
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(grid_table(table, cx))
    }
}

/// Builds the table entity of a grid: columns can be resized and moved, the table's own
/// selection is off (the grid draws its own), and the ingestion is polled.
pub(crate) fn new_table(
    grid: GridTab,
    window: &mut Window,
    cx: &mut Context<WorkspaceView>,
) -> Table {
    let workspace = cx.entity().downgrade();
    let table = cx.new(|cx| {
        TableState::new(GridDelegate::new(grid, workspace), window, cx)
            .col_movable(true)
            .col_resizable(true)
            .sortable(false)
            .row_selectable(false)
            .col_selectable(false)
            .cell_selectable(false)
    });
    cx.subscribe(&table, |_, table, event: &TableEvent, cx| {
        if let TableEvent::ColumnWidthsChanged(widths) = event {
            let widths: Vec<f32> = widths.iter().map(|w| f32::from(*w)).collect();
            table.update(cx, |state, _| {
                let delegate = state.delegate_mut();
                delegate.controls.set_display_widths(&widths);
                delegate.rebuild_columns();
            });
        }
    })
    .detach();
    let bar = new_filter_bar(&table, window, cx);
    table.update(cx, |state, cx| {
        state.delegate_mut().bar = Some(bar);
        sync_bar(state, window, cx);
    });
    start_polling(&table, window, cx);
    table
}

/// Applies a grid action to a table: selection, layout, scrolling and fetching. Returns whether
/// it did anything.
pub(crate) fn apply_action(
    state: &mut TableState<GridDelegate>,
    action: GridAction,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) -> bool {
    if action == GridAction::FetchMore {
        let fetched = state.delegate_mut().grid_mut().fetch_more();
        if fetched {
            cx.notify();
        }
        return fetched;
    }
    if matches!(
        action,
        GridAction::SortAscending
            | GridAction::SortDescending
            | GridAction::ClearSort
            | GridAction::ClearFilter
            | GridAction::ToggleMatchCase
    ) {
        let d = state.delegate_mut();
        let active_col = d.controls.selection().active().map(|c| c.col);
        let changed = d.tab.apply_order_action(action, active_col);
        if changed {
            if action == GridAction::ClearFilter
                && let Some(bar) = &d.bar
            {
                let (quick, operand) = (bar.quick.clone(), bar.operand.clone());
                for input in [quick, operand] {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
            order_changed(state, cx);
        }
        return changed;
    }
    let delegate = state.delegate_mut();
    let visible = delegate.tab.visible();
    let rows = delegate.tab.rows();
    let applied = delegate.controls.apply(action, &rows, visible);
    if applied.relayout {
        state.delegate_mut().rebuild_columns();
        state.refresh(cx);
    }
    if let Some(row) = applied.scroll_row {
        // `row` is a display row; scroll the least that shows it, and not at all when it is
        // already fully visible.
        state
            .vertical_scroll_handle
            .scroll_to_item(row, gpui::ScrollStrategy::Nearest);
        cx.notify();
    }
    if let Some(col) = applied.scroll_col {
        state.scroll_to_col(col, cx);
    }
    if let Some(col) = applied.autofit {
        state.fit_col(col, window, cx);
    }
    cx.notify();
    applied != super::Applied::default()
}

/// The table under its filter bar. Its keys are routed by the workspace
/// (`WorkspaceView::grid_key`), before the global chord router, because that router would
/// otherwise take Ctrl+Space for completion.
pub(crate) fn grid_table(table: &Table, cx: &mut Context<WorkspaceView>) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .children(filter_bar(table, cx))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .child(DataTable::new(table).stripe(true).bordered(true)),
        )
}

/// Applies the pending ingestion messages every [`POLL_INTERVAL`] until the grid ends or goes.
pub(crate) fn start_polling(table: &Table, window: &mut Window, cx: &mut Context<WorkspaceView>) {
    let weak = table.downgrade();
    cx.spawn_in(window, async move |this, cx| {
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let polled = weak.update_in(cx, |state, window, cx| {
                let outcome = state.delegate_mut().tab.poll(POLL_BUDGET);
                if outcome.columns_changed {
                    state.delegate_mut().rebuild_columns();
                    sync_bar(state, window, cx);
                    state.refresh(cx);
                }
                // The poll tick is also the timer of the 500 ms merge trigger: rows that wait while
                // the stream is quiet are merged once due. It ends with this task (result done,
                // tab closed), so nothing outlives the grid.
                if state.delegate().tab.order().is_active()
                    && (outcome.terminal
                        || state
                            .delegate()
                            .tab
                            .next_extend_check(Instant::now())
                            .is_some_and(|wait| wait.is_zero()))
                {
                    ensure_pump(state, cx);
                }
                if outcome.applied > 0 || outcome.terminal {
                    cx.notify();
                }
                (outcome.applied > 0 || outcome.terminal, outcome.terminal)
            });
            let Ok((changed, terminal)) = polled else {
                break;
            };
            if changed && this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
            if terminal {
                break;
            }
        }
    })
    .detach();
}

/// The sort or filter changed: show the rows at once (an inactive order is the identity) and
/// run the job.
fn order_changed(state: &mut TableState<GridDelegate>, cx: &mut Context<TableState<GridDelegate>>) {
    state.refresh(cx);
    ensure_pump(state, cx);
    cx.notify();
}

/// Starts the task that runs the sort and filter job in slices of [`ORDER_SLICE`] with a pause
/// of [`ORDER_YIELD`] between them, unless it is running. It ends when the job is done.
fn ensure_pump(state: &mut TableState<GridDelegate>, cx: &mut Context<TableState<GridDelegate>>) {
    if state.delegate().pumping {
        return;
    }
    state.delegate_mut().pumping = true;
    cx.spawn(async move |this, cx| {
        loop {
            let step = this.update(cx, |state, cx| {
                let d = state.delegate_mut();
                let progress = d.tab.step_order(Some(ORDER_SLICE));
                if progress == OrderProgress::Idle {
                    d.pumping = false;
                }
                let workspace = d.workspace.clone();
                if progress == OrderProgress::Published {
                    state.refresh(cx);
                }
                cx.notify();
                (progress, workspace)
            });
            let Ok((progress, workspace)) = step else {
                break;
            };
            // The status text and the filter bar live in the workspace's render.
            workspace.update(cx, |_, cx| cx.notify()).ok();
            if progress == OrderProgress::Idle {
                break;
            }
            cx.background_executor().timer(ORDER_YIELD).await;
        }
    })
    .detach();
}

// ---- filter bar ------------------------------------------------------------------------------

/// A select over a list of titles, with the first one chosen.
fn list_select(
    items: Vec<SharedString>,
    window: &mut Window,
    cx: &mut Context<WorkspaceView>,
) -> Entity<SelectState<Vec<SharedString>>> {
    cx.new(|cx| SelectState::new(items, Some(IndexPath::default().row(0)), window, cx))
}

/// The row of a select's choice.
fn chosen(select: &Entity<SelectState<Vec<SharedString>>>, cx: &App) -> usize {
    select.read(cx).selected_index(cx).map_or(0, |ix| ix.row)
}

/// Creates the inputs of a grid's filter bar and subscribes to them.
fn new_filter_bar(
    table: &Table,
    window: &mut Window,
    cx: &mut Context<WorkspaceView>,
) -> FilterBar {
    let quick = cx.new(|cx| InputState::new(window, cx).placeholder("Filter rows…"));
    let operand = cx.new(|cx| InputState::new(window, cx).placeholder("Value"));
    let scope = list_select(vec!["All columns".into()], window, cx);
    let column = list_select(Vec::new(), window, cx);
    let ops = OPS
        .iter()
        .map(|(_, title)| SharedString::from(*title))
        .collect();
    let op = list_select(ops, window, cx);
    let weak = table.downgrade();
    let mut subscriptions = Vec::new();
    let on_quick = weak.clone();
    subscriptions.push(cx.subscribe(&quick, move |_, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            on_quick.update(cx, schedule_quick_filter).ok();
        }
    }));
    let on_scope = weak;
    subscriptions.push(cx.subscribe(
        &scope,
        move |_, _, _: &gpui_component::select::SelectEvent<Vec<SharedString>>, cx| {
            on_scope
                .update(cx, |state, cx| edit_filter(state, cx, |_| {}))
                .ok();
        },
    ));
    // A change of the operator shows or hides the operand.
    subscriptions.push(cx.subscribe(
        &op,
        |_, _, _: &gpui_component::select::SelectEvent<Vec<SharedString>>, cx| {
            cx.notify();
        },
    ));
    FilterBar {
        quick,
        scope,
        column,
        op,
        operand,
        edits: 0,
        _subscriptions: subscriptions,
    }
}

/// Fills the column selects from the grid's headers and empties the quick filter: called when
/// the column set arrives or changes (the order was reset with it).
fn sync_bar(
    state: &mut TableState<GridDelegate>,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) {
    let d = state.delegate();
    let Some(bar) = &d.bar else {
        return;
    };
    let names: Vec<SharedString> = (0..d.tab.column_count())
        .filter_map(|ix| d.tab.header(ix))
        .map(|h| SharedString::from(h.name.clone()))
        .collect();
    let (quick, scope, column) = (bar.quick.clone(), bar.scope.clone(), bar.column.clone());
    let mut all = vec![SharedString::from("All columns")];
    all.extend(names.iter().cloned());
    let first = Some(IndexPath::default().row(0));
    scope.update(cx, |select, cx| {
        select.set_items(all, window, cx);
        select.set_selected_index(first, window, cx);
    });
    column.update(cx, |select, cx| {
        let any = !names.is_empty();
        select.set_items(names, window, cx);
        if any {
            select.set_selected_index(first, window, cx);
        }
    });
    quick.update(cx, |input, cx| input.set_value("", window, cx));
}

/// Applies the quick filter once typing paused for [`QUICK_FILTER_DELAY`].
fn schedule_quick_filter(
    state: &mut TableState<GridDelegate>,
    cx: &mut Context<TableState<GridDelegate>>,
) {
    let Some(bar) = state.delegate_mut().bar.as_mut() else {
        return;
    };
    bar.edits += 1;
    let edit = bar.edits;
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(QUICK_FILTER_DELAY).await;
        this.update(cx, |state, cx| {
            if state
                .delegate()
                .bar
                .as_ref()
                .is_some_and(|bar| bar.edits == edit)
            {
                edit_filter(state, cx, |_| {});
            }
        })
        .ok();
    })
    .detach();
}

/// Rebuilds the filter from the quick filter inputs and the conditions (changed by `edit`),
/// and runs the job.
fn edit_filter(
    state: &mut TableState<GridDelegate>,
    cx: &mut Context<TableState<GridDelegate>>,
    edit: impl FnOnce(&mut Vec<Condition>),
) {
    let d = state.delegate_mut();
    let Some(bar) = &d.bar else {
        return;
    };
    let text = bar.quick.read(cx).value().to_string();
    let scope = chosen(&bar.scope, cx);
    let mut conditions = d.tab.order().filter().conditions.clone();
    edit(&mut conditions);
    let quick = (!text.is_empty()).then_some(QuickFilter {
        text,
        column: scope.checked_sub(1),
    });
    d.tab
        .order_mut()
        .set_filter(FilterSpec { quick, conditions });
    order_changed(state, cx);
}

/// Adds the condition the editor shows.
fn add_condition(
    state: &mut TableState<GridDelegate>,
    window: &mut Window,
    cx: &mut Context<TableState<GridDelegate>>,
) {
    let Some(bar) = &state.delegate().bar else {
        return;
    };
    let column = chosen(&bar.column, cx);
    let Some((op, _)) = OPS.get(chosen(&bar.op, cx)).copied() else {
        return;
    };
    let operand = bar.operand.read(cx).value().to_string();
    if state.delegate().tab.column_count() == 0 || (op.needs_operand() && operand.trim().is_empty())
    {
        return;
    }
    let operand_input = bar.operand.clone();
    operand_input.update(cx, |input, cx| input.set_value("", window, cx));
    let operand = if op.needs_operand() {
        operand
    } else {
        String::new()
    };
    edit_filter(state, cx, move |conditions| {
        conditions.push(Condition {
            column,
            op,
            operand,
        });
    });
}

/// One chip per filter condition, with a button that removes it.
fn condition_chips(
    table: &Table,
    filter: &FilterSpec,
    names: &[String],
    cx: &mut Context<WorkspaceView>,
) -> Vec<Div> {
    let border = cx.theme().border;
    filter
        .conditions
        .iter()
        .enumerate()
        .map(|(ix, c)| {
            let title = OPS
                .iter()
                .find(|(op, _)| *op == c.op)
                .map_or("", |(_, title)| title);
            let text = if c.op.needs_operand() {
                format!("{} {} {}", names[ix], title, c.operand)
            } else {
                format!("{} {title}", names[ix])
            };
            let remove = table.clone();
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .px_1()
                .border_1()
                .border_color(border)
                .rounded_sm()
                .text_sm()
                .child(text)
                .child(
                    Button::new(("grid-chip-x", ix as u64))
                        .label("×")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            remove.update(cx, |state, cx| {
                                edit_filter(state, cx, |conditions| {
                                    if ix < conditions.len() {
                                        conditions.remove(ix);
                                    }
                                });
                            });
                        })),
                )
        })
        .collect()
}

/// The filter bar above a grid: quick filter, scope, match case, condition editor and chips,
/// and the status of the sort and filter. `None` before the bar exists.
fn filter_bar(table: &Table, cx: &mut Context<WorkspaceView>) -> Option<Div> {
    let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
    let id = table.entity_id().as_u64();
    let (quick, scope, column, op, operand, filter, status, case, chip_names) = {
        let d = table.read(cx).delegate();
        let bar = d.bar.as_ref()?;
        let filter = d.tab.order().filter().clone();
        let names: Vec<String> = filter
            .conditions
            .iter()
            .map(|c| {
                d.tab
                    .header(c.column)
                    .map_or_else(String::new, |h| h.name.clone())
            })
            .collect();
        (
            bar.quick.clone(),
            bar.scope.clone(),
            bar.column.clone(),
            bar.op.clone(),
            bar.operand.clone(),
            filter,
            d.tab.order_text(),
            d.tab.order().collation() == Collation::CodePoint,
            names,
        )
    };
    let needs_operand = OPS
        .get(chosen(&op, cx))
        .is_none_or(|(op, _)| op.needs_operand());
    let act = |table: &Table, action: GridAction| {
        let table = table.clone();
        move |window: &mut Window, cx: &mut App| {
            table.update(cx, |state, cx| apply_action(state, action, window, cx));
        }
    };
    let (on_case, on_clear) = (
        act(table, GridAction::ToggleMatchCase),
        act(table, GridAction::ClearFilter),
    );
    let add_table = table.clone();
    let chips = condition_chips(table, &filter, &chip_names, cx);
    Some(
        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(border)
            .child(div().w(px(200.)).child(Input::new(&quick).small()))
            .child(div().w(px(140.)).child(Select::new(&scope).small()))
            .child(
                Checkbox::new(("grid-match-case", id))
                    .label("Match case")
                    .checked(case)
                    .on_click(cx.listener(move |_, _: &bool, window, cx| on_case(window, cx))),
            )
            .child(div().w(px(1.)).h(px(18.)).bg(border))
            .child(div().w(px(140.)).child(Select::new(&column).small()))
            .child(div().w(px(130.)).child(Select::new(&op).small()))
            .children(needs_operand.then(|| div().w(px(140.)).child(Input::new(&operand).small())))
            .child(
                Button::new(("grid-add-condition", id))
                    .label("Add")
                    .small()
                    .on_click(cx.listener(move |_, _, window, cx| {
                        add_table.update(cx, |state, cx| add_condition(state, window, cx));
                    })),
            )
            .child(
                // The parent id scopes the chip buttons to this table.
                div()
                    .id(("grid-chips", id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .children(chips),
            )
            .child(
                Button::new(("grid-clear-filter", id))
                    .label("Clear filter")
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |_, _, window, cx| on_clear(window, cx))),
            )
            .children(status.map(|text| div().text_sm().text_color(muted).child(text))),
    )
}

// ---- probe -----------------------------------------------------------------------------------

/// Seconds after which the probe gives up and exits with code 2.
const WATCHDOG_SECS: u64 = 120;
/// Rows the probe scrolls to, one per frame.
const PROBE_STEPS: usize = 600;

/// The opt-in measurement run, started by `SQLER_GRID_PROBE=1`.
pub(crate) struct GridProbe;

/// What the frame hook shares with the probe task.
#[derive(Default)]
struct Shared {
    measuring: Cell<bool>,
    last_frame: Cell<Option<Instant>>,
    frames: Cell<u64>,
    table: RefCell<Option<Table>>,
}

impl GridProbe {
    /// Starts the probe if the environment asks for it.
    pub(crate) fn start_if_requested(window: &mut Window, cx: &mut Context<WorkspaceView>) {
        if std::env::var("SQLER_GRID_PROBE").as_deref() != Ok("1") {
            return;
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(WATCHDOG_SECS));
            println!("SQLER grid_result=fail reason=watchdog");
            std::process::exit(2);
        });
        let shared = Rc::new(Shared::default());
        frame_hook(window, &shared);
        cx.spawn_in(window, async move |this, cx| {
            // Let the first frame present.
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let started = Instant::now();
            let opened = this.update_in(cx, |view, _, cx| {
                view.dispatch(WorkspaceAction::SelectConnection(0), cx);
                view.dispatch(WorkspaceAction::OpenSyntheticGrid, cx);
                view.ws_active_grid()
            });
            let Ok(Some(tab)) = opened else {
                finish(0, 0.0, Some("no-connection"), &Measured::default());
            };
            // Wait for the table and the end of the ingestion.
            let ingest_ms = loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let state = this.update_in(cx, |view, _, cx| {
                    view.grids.table(tab).map(|t| {
                        let grid = &t.read(cx).delegate().tab;
                        (grid.state().clone(), grid.row_count(), t.clone())
                    })
                });
                match state {
                    Ok(Some((IngestState::Done { .. }, _, table))) => {
                        *shared.table.borrow_mut() = Some(table);
                        break started.elapsed().as_secs_f64() * 1000.0;
                    }
                    Ok(Some((IngestState::Failed(_) | IngestState::Cancelled, _, _))) | Err(_) => {
                        finish(0, 0.0, Some("ingest-failed"), &Measured::default());
                    }
                    Ok(_) => {}
                }
            };
            let Some(table) = shared.table.borrow().clone() else {
                finish(0, ingest_ms, Some("ingest-failed"), &Measured::default());
            };
            let rows = table.read_with(cx, |t, _| t.delegate().tab.row_count());
            // Rows drawn while the result was loading are not part of the scroll measurement.
            table.update(cx, |state, _| {
                *state.delegate_mut().tab.stats_mut() = super::RenderStats::default();
            });
            shared.measuring.set(true);
            for step in 0..PROBE_STEPS {
                let row = step * rows.saturating_sub(1) / (PROBE_STEPS - 1);
                scroll_and_wait(&table, row, &shared, cx).await;
            }
            scroll_and_wait(&table, rows.saturating_sub(1), &shared, cx).await;
            for _ in 0..(5000 / 16) {
                let saw = table.read_with(cx, |t, _| t.delegate().tab.stats().saw_last());
                if saw {
                    break;
                }
                cx.background_executor().timer(POLL_INTERVAL).await;
            }
            let measured = measure(&table, cx);
            finish(rows, ingest_ms, None, &measured)
        })
        .detach();
    }
}

/// Scrolls to a row and waits for the next presented frame (at most 100 ms).
async fn scroll_and_wait(
    table: &Table,
    row: usize,
    shared: &Rc<Shared>,
    cx: &mut gpui::AsyncWindowContext,
) {
    let before = shared.frames.get();
    table.update(cx, |state, cx| state.scroll_to_row(row, cx));
    for _ in 0..25 {
        cx.background_executor()
            .timer(Duration::from_millis(4))
            .await;
        if shared.frames.get() > before {
            break;
        }
    }
}

/// Records the interval of every presented frame once measuring has started, and re-arms itself.
fn frame_hook(window: &mut Window, shared: &Rc<Shared>) {
    let shared = Rc::clone(shared);
    window.on_next_frame(move |window, cx| {
        let now = Instant::now();
        if shared.measuring.get() {
            let interval = shared
                .last_frame
                .get()
                .map(|t| now.duration_since(t).as_secs_f64() * 1000.0);
            shared.last_frame.set(Some(now));
            shared.frames.set(shared.frames.get() + 1);
            if let Some(table) = shared.table.borrow().as_ref() {
                table.update(cx, |state, _| {
                    state.delegate_mut().tab.stats_mut().end_frame(interval);
                });
            }
        }
        frame_hook(window, &shared);
    });
}

/// What the probe measured while scrolling.
#[derive(Default)]
struct Measured {
    frames: u64,
    p50: f64,
    p95: f64,
    max: f64,
    max_rows: usize,
    last_row: bool,
}

/// Reads the frame and row statistics of the probe's table.
fn measure(table: &Table, cx: &mut gpui::AsyncWindowContext) -> Measured {
    table.read_with(cx, |state, _| {
        let stats = state.delegate().tab.stats();
        Measured {
            frames: stats.frames(),
            p50: stats.frame_ms(50).unwrap_or(0.0),
            p95: stats.frame_ms(95).unwrap_or(0.0),
            max: stats.max_frame_ms().unwrap_or(0.0),
            max_rows: stats.max_rows_per_frame(),
            last_row: stats.saw_last(),
        }
    })
}

/// Prints the report and exits: 0 pass, 1 fail.
fn finish(rows: usize, ingest_ms: f64, failure: Option<&'static str>, m: &Measured) -> ! {
    let report = ProbeReport {
        os: std::env::consts::OS.to_owned(),
        rows,
        ingest_ms,
        frames: m.frames,
        frame_ms_p50: m.p50,
        frame_ms_p95: m.p95,
        frame_ms_max: m.max,
        max_rows_rendered_per_frame: m.max_rows,
        rendered_last_row: m.last_row,
        rss_mb: rss_mb(),
        failure,
    };
    for line in report.lines() {
        println!("{line}");
    }
    std::process::exit(i32::from(report.verdict().is_some()));
}

/// Resident memory in MB: `VmRSS` on Linux, `ps` on macOS, else unknown.
fn rss_mb() -> Option<f64> {
    let kb: f64 = if cfg!(target_os = "linux") {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()?
    } else if cfg!(target_os = "macos") {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        String::from_utf8(out.stdout).ok()?.trim().parse().ok()?
    } else {
        return None;
    };
    Some(kb / 1024.0)
}
