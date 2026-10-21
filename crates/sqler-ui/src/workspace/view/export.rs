// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the export (ADR-0032): the dialog's inputs, the save dialog, and the three
//! tasks of a running job. The writer runs on tokio's blocking pool; a fresh run reads rows on a
//! short-lived session; loaded rows are copied out of the grid model here, on the UI thread, in
//! slices short enough not to cost a frame.

use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Context, Entity, Window};
use gpui_component::WindowExt as _;
use gpui_component::input::InputState;
use tokio::sync::mpsc;

use super::WorkspaceView;
use crate::export::{
    Delimiter, ExportCommand, ExportFailure, ExportJob, ExportScope, ExportSource, ExportTarget,
    FEED_CAPACITY, Feed, GridFacts, LoadedCursor, LoadedRows, fresh_rows, write_feed,
};
use crate::grid::view::Table;
use crate::grid::{DisplayMap, GridModel, IngestState, RowOrder};
use crate::tabs::TabId;
use crate::workspace::Effect;

/// Rows in one batch the copier hands to the writer.
const COPY_BATCH_ROWS: usize = 2_048;
/// Batches the copier may hand over in one tick.
const COPY_BATCHES_PER_TICK: usize = 4;
/// How long the copier may work in one tick.
const COPY_TICK_BUDGET: Duration = Duration::from_millis(4);
/// The pause between two ticks.
const COPY_TICK_PAUSE: Duration = Duration::from_millis(1);

/// Shown when the selection no longer matches the rows or columns of the grid.
const SELECTION_UNMAPPED: &str =
    "The selection no longer matches the grid. Select the cells again, then export.";

/// The text inputs of the export dialog on screen.
pub(crate) struct ExportInputs {
    /// The dialog these inputs belong to.
    generation: u64,
    /// `NULL` text (`CSV` and Markdown).
    pub(crate) null_text: Entity<InputState>,
    /// `SQL`: the table.
    pub(crate) table: Entity<InputState>,
    /// `SQL`: the schema.
    pub(crate) schema: Entity<InputState>,
    /// `SQL`: rows per `INSERT`.
    pub(crate) rows_per_insert: Entity<InputState>,
}

/// What the dialog needs to know about the table on screen. A selection that holds a position the
/// grid cannot map to the model is an error, never a smaller selection.
fn grid_facts(table: &Table, cx: &App) -> Result<GridFacts, &'static str> {
    let state = table.read(cx);
    let delegate = state.delegate();
    let grid = delegate.grid();
    let rows = grid.row_count();
    let controls = delegate.controls();
    let layout = controls.layout();
    // The grid's own row order, so a sort and a filter apply to a selection export.
    let order = grid.rows();
    let map = DisplayMap {
        rows: &order,
        columns: layout,
    };
    // Selection exports use the order on screen.
    let selection = match controls.selection().rect(&map) {
        None => None,
        Some(rect) => {
            let rows: Option<Vec<usize>> = rect.rows.map(|r| order.model_row(r)).collect();
            let columns: Option<Vec<usize>> = rect.cols.map(|c| layout.model_column(c)).collect();
            match (rows, columns) {
                (Some(rows), Some(columns)) => (!rows.is_empty() && !columns.is_empty())
                    .then_some(LoadedRows::Picked { rows, columns }),
                _ => return Err(SELECTION_UNMAPPED),
            }
        }
    };
    Ok(GridFacts {
        columns: grid.column_count(),
        rows,
        complete: matches!(grid.state(), IngestState::Done { .. }),
        selection,
    })
}

/// Sends up to a tick's worth of batches. `true` when the copy is over: everything was handed
/// over, or the writer is gone. A batch the channel has no room for is kept in `pending`.
fn copy_slice(
    cursor: &mut LoadedCursor,
    pending: &mut Option<Feed>,
    tx: &mpsc::Sender<Feed>,
    model: &GridModel,
) -> bool {
    let started = Instant::now();
    let mut sent = 0;
    while sent < COPY_BATCHES_PER_TICK && started.elapsed() < COPY_TICK_BUDGET {
        let feed = match pending.take() {
            Some(feed) => feed,
            None => match cursor.next_feed(model, COPY_BATCH_ROWS) {
                Some(feed) => feed,
                None => return true,
            },
        };
        match tx.try_send(feed) {
            Ok(()) => sent += 1,
            Err(mpsc::error::TrySendError::Full(feed)) => {
                *pending = Some(feed);
                return false;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return true,
        }
    }
    false
}

impl WorkspaceView {
    /// The dialog's inputs, while a dialog is on screen.
    pub(crate) fn export_inputs(&self) -> Option<&ExportInputs> {
        self.export_inputs.as_ref()
    }

    /// The table on screen and the target it stands for: the active grid tab, else the shown
    /// result of the active query tab.
    fn export_subject(&self) -> Option<(ExportTarget, Table)> {
        if let Some(tab) = self.ws_active_grid() {
            let table = self.grids.table(tab)?.clone();
            return Some((ExportTarget { tab, result: None }, table));
        }
        let tab = self.active_query_tab()?;
        let index = self.queries.shown_index(&self.ws, tab)?;
        let table = self.queries.table(tab, index)?.clone();
        Some((
            ExportTarget {
                tab,
                result: Some(index),
            },
            table,
        ))
    }

    /// The table a running export copies its rows from.
    fn export_table(&self, target: ExportTarget) -> Option<Table> {
        match target.result {
            None => self.grids.table(target.tab).cloned(),
            Some(index) => self.queries.table(target.tab, index).cloned(),
        }
    }

    /// Runs `grid:export` or `export:cancel`.
    pub(crate) fn run_export_command(
        &mut self,
        command: ExportCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let effect = match command {
            ExportCommand::Open => {
                let Some((target, table)) = self.export_subject() else {
                    window.push_notification("Open a results grid first.", cx);
                    return;
                };
                let facts = match grid_facts(&table, cx) {
                    Ok(facts) => facts,
                    Err(message) => {
                        window.push_notification(message, cx);
                        return;
                    }
                };
                self.ws.open_export(target, &facts)
            }
            ExportCommand::Cancel => {
                let Some(tab) = self.ws.tabs().active().map(crate::tabs::Tab::id) else {
                    return;
                };
                self.ws.cancel_export(tab)
            }
        };
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Makes the dialog inputs match the dialog on screen.
    pub(crate) fn sync_export_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.ws.export_dialog() else {
            self.export_inputs = None;
            return;
        };
        if self
            .export_inputs
            .as_ref()
            .is_some_and(|i| i.generation == dialog.generation)
        {
            return;
        }
        let input = |text: &str, hint: &'static str, window: &mut Window, cx: &mut App| {
            let text = text.to_owned();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(hint);
                state.set_value(text, window, cx);
                state
            })
        };
        let form = &dialog.form;
        let generation = dialog.generation;
        self.export_inputs = Some(ExportInputs {
            generation,
            null_text: input(&form.null_text, "empty", window, cx),
            table: input(&form.table, "table name", window, cx),
            schema: input(&form.schema, "schema", window, cx),
            rows_per_insert: input(&form.rows_per_insert, "100", window, cx),
        });
    }

    /// Copies what the user typed into the dialog's form.
    fn pull_export_inputs(&mut self, cx: &App) {
        let (Some(inputs), Some(dialog)) = (&self.export_inputs, self.ws.export_dialog_mut())
        else {
            return;
        };
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        dialog.form.null_text = text(&inputs.null_text);
        dialog.form.table = text(&inputs.table);
        dialog.form.schema = text(&inputs.schema);
        dialog.form.rows_per_insert = text(&inputs.rows_per_insert);
    }

    /// A format button.
    pub(crate) fn export_set_format(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pull_export_inputs(cx);
        let Some(dialog) = self.ws.export_dialog_mut() else {
            return;
        };
        dialog.set_format(id);
        // A `NULL` text still at the old default moved to the new one.
        let null_text = dialog.form.null_text.clone();
        if let Some(input) = self.export_inputs.as_ref().map(|i| i.null_text.clone()) {
            input.update(cx, |state, cx| state.set_value(null_text, window, cx));
        }
        cx.notify();
    }

    /// A scope choice.
    pub(crate) fn export_set_scope(&mut self, scope: ExportScope, cx: &mut Context<Self>) {
        if let Some(dialog) = self.ws.export_dialog_mut() {
            dialog.set_scope(scope);
        }
        cx.notify();
    }

    /// A delimiter button.
    pub(crate) fn export_set_delimiter(&mut self, delimiter: Delimiter, cx: &mut Context<Self>) {
        if let Some(dialog) = self.ws.export_dialog_mut() {
            dialog.set_delimiter(delimiter);
        }
        cx.notify();
    }

    /// The header toggle.
    pub(crate) fn export_set_header(&mut self, header: bool, cx: &mut Context<Self>) {
        if let Some(dialog) = self.ws.export_dialog_mut() {
            dialog.set_header(header);
        }
        cx.notify();
    }

    /// The Export button: checks the form and opens the save dialog.
    pub(crate) fn export_submit(&mut self, cx: &mut Context<Self>) {
        self.pull_export_inputs(cx);
        let Some(form) = self.ws.export_dialog().map(|d| d.form.clone()) else {
            return;
        };
        let effect = self.ws.submit_export(form);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Closes the dialog; nothing is exported.
    pub(crate) fn export_dismiss(&mut self, cx: &mut Context<Self>) {
        self.ws.dismiss_export();
        self.settle(cx);
    }

    /// The Cancel button of a running export.
    pub(crate) fn export_cancel(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let effect = self.ws.cancel_export(tab);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Carries out `PickExportPath` and `RunExport`.
    pub(crate) fn apply_export(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::PickExportPath {
                directory,
                suggested_name,
                dialog,
            } => {
                let picked = cx.prompt_for_new_path(&directory, Some(&suggested_name));
                cx.spawn(async move |this, cx| {
                    let path = match picked.await {
                        Ok(Ok(path)) => path,
                        Ok(Err(err)) => {
                            let _ = this.update(cx, |view, cx| {
                                view.notices
                                    .push(format!("The save dialog could not open: {err}"));
                                cx.notify();
                            });
                            None
                        }
                        Err(_) => None,
                    };
                    this.update(cx, |view, cx| {
                        let effect = view.ws.export_path_chosen(dialog, path);
                        view.apply(effect, cx);
                        view.settle(cx);
                    })
                    .ok();
                })
                .detach();
            }
            Effect::RunExport(job) => self.start_export(job, cx),
            _ => {}
        }
    }

    /// Starts the writer, the producer of the job's source, and the task that follows them.
    fn start_export(&mut self, job: ExportJob, cx: &mut Context<Self>) {
        let ExportJob {
            tab,
            id,
            target,
            source,
            spec,
            stop,
        } = job;
        let (tx, rx) = mpsc::channel(FEED_CAPACITY);
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
        let writer_stop = stop.clone();
        let done = self.bridge.blocking(move || {
            write_feed(
                rx,
                spec,
                &writer_stop,
                Box::new(move |progress| {
                    // The view may have stopped listening; the writer does not care.
                    let _ = progress_tx.send(progress);
                }),
            )
        });
        match source {
            ExportSource::Fresh { request, query } => {
                drop(self.bridge.spawn(fresh_rows(
                    self.bridge.connector(),
                    request,
                    query,
                    tx,
                    stop,
                )));
            }
            ExportSource::Loaded(rows) => match self.export_table(target) {
                Some(table) => Self::copy_loaded(table, rows, tx, stop, cx),
                // The table is gone: dropping the sender ends the writer with a failure.
                None => drop(tx),
            },
        }
        cx.spawn(async move |this, cx| {
            // The channel closes when the writer is done with its progress callback.
            while let Some(mut progress) = progress_rx.recv().await {
                while let Ok(newer) = progress_rx.try_recv() {
                    progress = newer;
                }
                let alive = this
                    .update(cx, |view, cx| {
                        view.ws.export_progress(tab, id, progress);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
            let outcome = done.await.unwrap_or(Err(ExportFailure::Lost));
            this.update(cx, |view, cx| {
                let effect = view.ws.export_finished(tab, id, outcome);
                view.apply(effect, cx);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Copies the rows of `table` into the writer's channel in short slices.
    fn copy_loaded(
        table: Table,
        rows: LoadedRows,
        tx: mpsc::Sender<Feed>,
        stop: crate::export::ExportStop,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |_, cx| {
            let mut cursor = LoadedCursor::new(rows);
            let mut pending = None;
            // Dropping the sender (on any exit) tells the writer the source ended.
            while !stop.is_cancelled() {
                let finished = table.read_with(cx, |state, _| {
                    let model = state.delegate().grid().model();
                    copy_slice(&mut cursor, &mut pending, &tx, model)
                });
                if finished {
                    break;
                }
                cx.background_executor().timer(COPY_TICK_PAUSE).await;
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{GridLimits, SyntheticSource, grid_channel_with, spawn_ingest};

    /// A model fed by a synthetic source, polled until it is done.
    fn model(rows: usize, columns: usize) -> GridModel {
        let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
        drop(spawn_ingest(SyntheticSource::new(rows, columns, 100), sink));
        while !matches!(model.state(), IngestState::Done { .. }) {
            model.poll(64);
            std::thread::yield_now();
        }
        model
    }

    #[test]
    fn a_full_channel_keeps_the_batch_for_the_next_tick() {
        let model = model(10_000, 2);
        let mut cursor = LoadedCursor::new(LoadedRows::All {
            rows: model.row_count(),
        });
        let (tx, mut rx) = mpsc::channel(2);
        let mut pending = None;
        // Columns + one rows batch fill the channel; the third message waits.
        assert!(!copy_slice(&mut cursor, &mut pending, &tx, &model));
        assert!(pending.is_some());
        assert!(matches!(rx.try_recv(), Ok(Feed::Columns(_))));
        assert!(rx.try_recv().is_ok());
        // Room again: the kept batch goes first, nothing is lost or reordered.
        let mut rows = 0;
        loop {
            let finished = copy_slice(&mut cursor, &mut pending, &tx, &model);
            while let Ok(feed) = rx.try_recv() {
                if let Feed::Rows(batch) = feed {
                    rows += batch.len();
                }
            }
            if finished {
                break;
            }
        }
        // The first batch (2,048) was taken above; the kept one is counted here.
        assert_eq!(rows + COPY_BATCH_ROWS, 10_000);
    }

    #[test]
    fn a_closed_channel_ends_the_copy() {
        let model = model(100, 2);
        let mut cursor = LoadedCursor::new(LoadedRows::All {
            rows: model.row_count(),
        });
        let (tx, rx) = mpsc::channel(2);
        drop(rx);
        let mut pending = None;
        assert!(copy_slice(&mut cursor, &mut pending, &tx, &model));
    }
}
