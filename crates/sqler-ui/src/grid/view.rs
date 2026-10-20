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
    App, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Stateful, Styled as _, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _};

use super::{GridAction, GridTab, IngestState, POLL_BUDGET, ProbeReport};
use crate::tabs::{TabId, TabStrip};
use crate::workspace::{WorkspaceAction, WorkspaceView};

/// How often the pending messages of a grid are applied.
const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// The grid action a command ID stands for, if it is one.
pub(crate) fn action_for_command(id: &crate::command::CommandId) -> Option<GridAction> {
    [
        GridAction::ScrollToTop,
        GridAction::ScrollToEnd,
        GridAction::FetchMore,
    ]
    .into_iter()
    .find(|a| a.command_id() == *id)
}

/// The width of a column of this type, in pixels.
fn column_width(label: &str) -> f32 {
    match label {
        "integer" => 110.0,
        "float" => 120.0,
        "boolean" => 80.0,
        "text" => 220.0,
        _ => 160.0,
    }
}

/// The table delegate: a thin wrapper around a [`GridTab`].
pub(crate) struct GridDelegate {
    tab: GridTab,
    columns: Vec<Column>,
}

impl GridDelegate {
    pub(crate) fn new(tab: GridTab) -> Self {
        Self {
            tab,
            columns: Vec::new(),
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

    /// Rebuilds the column list from the tab's headers.
    fn rebuild_columns(&mut self) {
        self.columns = (0..self.tab.column_count())
            .filter_map(|ix| self.tab.header(ix).map(|h| (ix, h)))
            .map(|(ix, h)| {
                Column::new(format!("c{ix}"), h.name.clone())
                    .width(px(column_width(h.type_label)))
                    .resizable(false)
                    .movable(false)
            })
            .collect();
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.tab.row_count()
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
            .header(col_ix)
            .map_or(("", ""), |h| (h.name.as_str(), h.type_label));
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .whitespace_nowrap()
            .child(name.to_owned())
            .child(div().text_xs().text_color(muted).child(label.to_owned()))
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let rows = self.tab.row_count();
        self.tab.stats_mut().row_rendered(row_ix, rows);
        div().id(("row", row_ix))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(self.tab.cell_text(row_ix, col_ix))
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
        self.tab.cell_text(row_ix, col_ix)
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
            let table = cx.new(|cx| {
                TableState::new(GridDelegate::new(tab), window, cx)
                    .col_movable(false)
                    .col_resizable(false)
                    .sortable(false)
            });
            start_polling(&table, cx);
            self.tables.insert(id, table);
        }
    }

    /// Runs a grid action on the grid of a tab: scrolls to the top or the end, or fetches the next
    /// page of a paused result. Returns whether it did anything.
    pub(crate) fn run_action(
        &self,
        tab: TabId,
        action: GridAction,
        cx: &mut Context<WorkspaceView>,
    ) -> bool {
        let Some(table) = self.tables.get(&tab) else {
            return false;
        };
        if action == GridAction::FetchMore {
            return table.update(cx, |state, cx| {
                let fetched = state.delegate_mut().grid_mut().fetch_more();
                if fetched {
                    cx.notify();
                }
                fetched
            });
        }
        let Some(row) = table.read(cx).delegate().tab.jump_target(action) else {
            return false;
        };
        table.update(cx, |state, cx| state.scroll_to_row(row, cx));
        true
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
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.grids.run_action(tab, GridAction::FetchMore, cx);
                    }))
            }))
            .child(
                Button::new("grid-top")
                    .label("Top")
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.grids.run_action(tab, GridAction::ScrollToTop, cx);
                    })),
            )
            .child(
                Button::new("grid-end")
                    .label("End")
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.grids.run_action(tab, GridAction::ScrollToEnd, cx);
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(DataTable::new(table).stripe(true).bordered(true)),
            )
    }
}

/// Applies the pending ingestion messages every [`POLL_INTERVAL`] until the grid ends or goes.
pub(crate) fn start_polling(table: &Table, cx: &mut Context<WorkspaceView>) {
    let weak = table.downgrade();
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let polled = weak.update(cx, |state, cx| {
                let outcome = state.delegate_mut().tab.poll(POLL_BUDGET);
                if outcome.columns_changed {
                    state.delegate_mut().rebuild_columns();
                    state.refresh(cx);
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
