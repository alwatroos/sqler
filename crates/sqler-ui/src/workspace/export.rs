// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Exporting a result inside the workspace: the dialog, the statements it may run again and the
//! export in progress of every tab.
//!
//! Like the query lifecycle this is state only. [`Workspace::submit_export`] and
//! [`Workspace::export_path_chosen`] return the [`Effect`]s the view carries out (the save dialog,
//! then the job); the view reports progress and the end back. The rule of ADR-0032: a statement
//! that `sqler-sql` does not classify `Read` is never run again, on any connection, so only the
//! rows already loaded can be exported then.

use std::collections::HashMap;
use std::path::PathBuf;

use sqler_sql::{Dialect, StatementClass, classify_each};

use super::state::{Effect, Workspace};
use crate::connections::ConnectRequest;
use crate::export::{
    ExportDialog, ExportForm, ExportJob, ExportOrigin, ExportOutcome, ExportScope, ExportSource,
    ExportStatus, ExportStop, ExportTarget, FreshRun, GridFacts, LoadedRows, WriteSpec,
    display_name, file_stem, fresh_gate, rows_text, start_directory, validate,
};
use crate::query::BoundStatement;
use crate::tabs::TabId;
use sqler_driver_api::sqler_core::Query;
use sqler_export::{ExportOptions, Progress};

/// A submitted dialog that waits for the save dialog.
struct PendingExport {
    target: ExportTarget,
    format_id: String,
    options: ExportOptions,
    source: ExportSource,
    /// The dialog showed that a fresh run was refused.
    loaded_only: bool,
    /// The generation of the tab's result when the dialog opened.
    result: u64,
    /// The generation of the dialog that asked for the path.
    dialog: u64,
}

/// The notice when the result under a dialog was replaced.
const RESULT_CHANGED: &str = "The result changed while the dialog was open; export cancelled.";

/// An export in progress.
struct RunningExport {
    status: ExportStatus,
    stop: ExportStop,
    loaded_only: bool,
}

/// The export state of the workspace.
#[derive(Default)]
pub(super) struct ExportCenter {
    dialog: Option<ExportDialog>,
    pending: Option<PendingExport>,
    running: HashMap<TabId, RunningExport>,
    /// The statement behind a grid tab (`None`) or a result of a query tab (`Some(i)`).
    origins: HashMap<(TabId, Option<usize>), ExportOrigin>,
    next_job: u64,
    next_dialog: u64,
    /// The generation of the result each tab shows. A new run or a reopened grid replaces it.
    results: HashMap<TabId, u64>,
    next_result: u64,
    /// The generation of the result the open dialog describes.
    dialog_result: u64,
    last_dir: Option<PathBuf>,
}

impl Workspace {
    fn export_notice(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    fn export_dialect(&self, tab: TabId) -> Option<Dialect> {
        Dialect::from_driver_id(self.tabs.get(tab)?.connection().driver.as_str())
    }

    /// Marks the results of a tab as replaced (a new run, a reopened grid). An open export dialog
    /// or a waiting save dialog of the tab then no longer describes what is on screen.
    pub fn result_replaced(&mut self, tab: TabId) {
        self.exports.next_result += 1;
        self.exports.results.insert(tab, self.exports.next_result);
    }

    fn result_generation(&self, tab: TabId) -> u64 {
        self.exports.results.get(&tab).copied().unwrap_or(0)
    }

    /// Remembers the statements of a run, so their results can be exported with a fresh run.
    /// Replaces the results of the tab's earlier run.
    pub fn remember_run(&mut self, tab: TabId, statements: &[BoundStatement]) {
        self.result_replaced(tab);
        let origins = &mut self.exports.origins;
        origins.retain(|(t, result), _| !(*t == tab && result.is_some()));
        for (i, statement) in statements.iter().enumerate() {
            origins.insert(
                (tab, Some(i)),
                ExportOrigin {
                    sql: statement.sql.clone(),
                    class: statement.class,
                    query: statement.query.clone(),
                },
            );
        }
    }

    /// Remembers the `SELECT` behind a grid tab, classified with the dialect of its connection.
    pub fn remember_open_data(&mut self, tab: TabId, sql: &str) {
        self.result_replaced(tab);
        let class = match self.export_dialect(tab).map(|d| classify_each(d, sql)) {
            Some(segments) if segments.len() == 1 => segments[0].1,
            _ => StatementClass::Unknown,
        };
        self.exports.origins.insert(
            (tab, None),
            ExportOrigin {
                sql: sql.to_owned(),
                class,
                query: Query::new(sql),
            },
        );
    }

    /// The request for a short-lived session of the tab's connection.
    fn export_request(&self, tab: TabId) -> Result<ConnectRequest, String> {
        let unavailable = || "The connection of this tab is not available.".to_owned();
        let conn = self.tab_conns.get(&tab).ok_or_else(unavailable)?;
        let profile = self.sidebar.profile(conn).ok_or_else(unavailable)?;
        ConnectRequest::from_profile(profile).map_err(|_| unavailable())
    }

    /// Whether the result can be read again: the statement gate, then the connection.
    fn fresh_check(&self, target: ExportTarget) -> Result<(ConnectRequest, Query), String> {
        let origin = self.exports.origins.get(&(target.tab, target.result));
        fresh_gate(self.export_dialect(target.tab), origin)?;
        let request = self
            .export_request(target.tab)
            .map_err(|msg| format!("{msg} Only the loaded rows can be exported."))?;
        let query = origin.map_or_else(|| Query::new(""), |o| o.query.clone());
        Ok((request, query))
    }

    /// Opens the export dialog for a result.
    ///
    /// Refuses with a notice when the result has no columns or the tab already exports.
    pub fn open_export(&mut self, target: ExportTarget, facts: &GridFacts) -> Effect {
        if self.tabs.get(target.tab).is_none() {
            return Effect::None;
        }
        if facts.columns == 0 {
            return self.export_notice("This result has nothing to export.");
        }
        if self.exports.running.contains_key(&target.tab) {
            return self.export_notice("An export is already running in this tab.");
        }
        let fresh = if facts.complete {
            FreshRun::NotNeeded
        } else {
            match self.fresh_check(target) {
                Ok(_) => FreshRun::Allowed,
                Err(message) => FreshRun::Refused(message),
            }
        };
        let stem = match target.result {
            Some(i) => format!("result-{}", i + 1),
            None => file_stem(
                &self
                    .tabs
                    .get(target.tab)
                    .map(crate::tabs::Tab::title)
                    .unwrap_or_default(),
            ),
        };
        self.exports.next_dialog += 1;
        self.exports.dialog_result = self.result_generation(target.tab);
        self.exports.pending = None;
        self.exports.dialog = Some(ExportDialog::new(
            target,
            facts,
            fresh,
            stem,
            self.exports.next_dialog,
        ));
        Effect::None
    }

    /// The export dialog on screen, if any.
    #[must_use]
    pub fn export_dialog(&self) -> Option<&ExportDialog> {
        self.exports.dialog.as_ref()
    }

    /// The export dialog, to change its choices.
    pub fn export_dialog_mut(&mut self) -> Option<&mut ExportDialog> {
        self.exports.dialog.as_mut()
    }

    /// Closes the export dialog; nothing is exported.
    pub fn dismiss_export(&mut self) {
        self.exports.dialog = None;
        self.exports.pending = None;
    }

    /// Checks the dialog. Returns the request for the save dialog, or keeps the dialog open with
    /// the reason in its error line.
    pub fn submit_export(&mut self, form: ExportForm) -> Effect {
        let Some(target) = self.exports.dialog.as_ref().map(|d| d.target) else {
            return Effect::None;
        };
        if self.exports.dialog_result != self.result_generation(target.tab) {
            self.dismiss_export();
            return self.export_notice(RESULT_CHANGED);
        }
        let dialect = self.export_dialect(target.tab);
        // Checked again here: the statement and the connection may have changed.
        let fresh = self.fresh_check(target);
        let Some(dialog) = self.exports.dialog.as_mut() else {
            return Effect::None;
        };
        dialog.form = form;
        dialog.error = None;
        let (format_id, options) = match validate(&dialog.form, dialect) {
            Ok(checked) => checked,
            Err(message) => {
                dialog.error = Some(message);
                return Effect::None;
            }
        };
        let loaded_only = matches!(dialog.fresh, FreshRun::Refused(_));
        let source = match dialog.form.scope {
            ExportScope::Loaded => ExportSource::Loaded(LoadedRows::All {
                rows: dialog.loaded_rows,
            }),
            ExportScope::Selection => {
                let Some(rows) = dialog.selection.clone() else {
                    dialog.error = Some("There is no selection to export.".to_owned());
                    return Effect::None;
                };
                ExportSource::Loaded(rows)
            }
            ExportScope::Fresh => match (&dialog.fresh, fresh) {
                (FreshRun::Allowed, Ok((request, query))) => ExportSource::Fresh { request, query },
                (FreshRun::Allowed, Err(message)) => {
                    dialog.fresh = FreshRun::Refused(message.clone());
                    dialog.form.scope = ExportScope::Loaded;
                    dialog.error = Some(message);
                    return Effect::None;
                }
                (FreshRun::Refused(message), _) => {
                    dialog.error = Some(message.clone());
                    return Effect::None;
                }
                (FreshRun::NotNeeded, _) => {
                    dialog.error = Some("All rows are loaded already.".to_owned());
                    return Effect::None;
                }
            },
        };
        let suggested_name = format!("{}.{}", dialog.stem, dialog.extension());
        self.exports.pending = Some(PendingExport {
            target,
            format_id,
            options,
            source,
            loaded_only,
            result: self.exports.dialog_result,
            dialog: self.exports.next_dialog,
        });
        Effect::PickExportPath {
            directory: start_directory(self.exports.last_dir.as_ref()),
            suggested_name,
            dialog: self.exports.next_dialog,
        }
    }

    /// The save dialog answered for the export dialog `dialog` (from `PickExportPath`). An answer
    /// to an older prompt is ignored. `None` (dismissed) keeps the export dialog; a path closes it
    /// and starts the job.
    pub fn export_path_chosen(&mut self, dialog: u64, path: Option<PathBuf>) -> Effect {
        if self
            .exports
            .pending
            .as_ref()
            .is_none_or(|p| p.dialog != dialog)
        {
            return Effect::None;
        }
        let Some(path) = path else {
            self.exports.pending = None;
            return Effect::None;
        };
        let Some(pending) = self.exports.pending.take() else {
            return Effect::None;
        };
        let tab = pending.target.tab;
        if self.tabs.get(tab).is_none() {
            self.exports.dialog = None;
            return Effect::None;
        }
        if self.exports.running.contains_key(&tab) {
            return self.export_notice("An export is already running in this tab.");
        }
        self.exports.dialog = None;
        // The job starts in this call, so this is also the check at job start.
        if pending.result != self.result_generation(tab) {
            return self.export_notice(RESULT_CHANGED);
        }
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.exports.last_dir = Some(dir.to_owned());
        }
        let id = self.exports.next_job;
        self.exports.next_job += 1;
        let stop = ExportStop::new();
        self.exports.running.insert(
            tab,
            RunningExport {
                status: ExportStatus {
                    id,
                    file_name: display_name(&path),
                    rows: 0,
                    bytes: 0,
                    cancelling: false,
                },
                stop: stop.clone(),
                loaded_only: pending.loaded_only,
            },
        );
        Effect::RunExport(ExportJob {
            tab,
            id,
            target: pending.target,
            source: pending.source,
            spec: WriteSpec {
                format_id: pending.format_id,
                options: pending.options,
                path,
                job: id,
            },
            stop,
        })
    }

    /// Records how far a job has come; reports of an older job are ignored.
    pub fn export_progress(&mut self, tab: TabId, id: u64, progress: Progress) {
        if let Some(running) = self.exports.running.get_mut(&tab)
            && running.status.id == id
        {
            running.status.rows = progress.rows;
            running.status.bytes = progress.bytes;
        }
    }

    /// The job ended: shows the result as a notice. A job of a tab that is gone is ignored.
    pub fn export_finished(&mut self, tab: TabId, id: u64, outcome: ExportOutcome) -> Effect {
        if self.exports.running.get(&tab).map(|r| r.status.id) != Some(id) {
            return Effect::None;
        }
        let Some(running) = self.exports.running.remove(&tab) else {
            return Effect::None;
        };
        let message = match outcome {
            Ok(summary) => {
                let note = if running.loaded_only {
                    " (loaded rows only)"
                } else {
                    ""
                };
                format!(
                    "Exported {} to {}{note}",
                    rows_text(summary.rows),
                    display_name(&summary.path)
                )
            }
            Err(crate::export::ExportFailure::Cancelled) => {
                "Export cancelled; no file was written.".to_owned()
            }
            Err(failure) => format!("Export failed: {failure}"),
        };
        self.export_notice(message)
    }

    /// Cancels the export of a tab; the job reports `Cancelled` when it has stopped.
    pub fn cancel_export(&mut self, tab: TabId) -> Effect {
        match self.exports.running.get_mut(&tab) {
            Some(running) => {
                running.stop.cancel();
                running.status.cancelling = true;
                Effect::None
            }
            None => self.export_notice("No export is running."),
        }
    }

    /// The export in progress in a tab.
    #[must_use]
    pub fn export_status(&self, tab: TabId) -> Option<&ExportStatus> {
        self.exports.running.get(&tab).map(|r| &r.status)
    }

    /// Stops every export (the app is quitting).
    pub fn cancel_all_exports(&mut self) {
        for (_, running) in self.exports.running.drain() {
            running.stop.cancel();
        }
        self.dismiss_export();
    }

    /// Stops the export of a tab that is closing and forgets its statements.
    pub(super) fn cancel_tab_export(&mut self, tab: TabId) {
        if let Some(running) = self.exports.running.remove(&tab) {
            running.stop.cancel();
        }
        self.exports.origins.retain(|(t, _), _| *t != tab);
        self.exports.results.remove(&tab);
        if self
            .exports
            .dialog
            .as_ref()
            .is_some_and(|d| d.target.tab == tab)
        {
            self.dismiss_export();
        }
    }

    /// The folder of the last export, for tests.
    #[cfg(test)]
    fn last_export_dir(&self) -> Option<&std::path::Path> {
        self.exports.last_dir.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Environment};
    use sqler_sql::StatementClass;

    use super::*;
    use crate::export::{ExportFailure, ExportSummary};
    use crate::query::{BufferSnapshot, RunMode};
    use crate::sidebar::SidebarModel;
    use crate::workspace::WorkspaceAction;
    use crate::workspace::testing::FakeSession;

    fn ws() -> Workspace {
        let driver = DriverId::new("sqlite").unwrap();
        Workspace::new(SidebarModel::from_connections(vec![
            ConnectionConfig::new("dev", driver.clone(), ":memory:")
                .with_environment(Environment::Dev),
            ConnectionConfig::new("prod", driver, ":memory:").with_environment(Environment::Prod),
        ]))
    }

    fn open(w: &mut Workspace, index: usize) -> TabId {
        let Effect::Connect { request, .. } = w.dispatch(WorkspaceAction::OpenConnection(index))
        else {
            panic!("expected Connect");
        };
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        w.tabs().active().map(crate::tabs::Tab::id).unwrap()
    }

    /// Runs the statements and gives the session back, so the tab is idle.
    fn run(w: &mut Workspace, tab: TabId, text: &str) {
        let Effect::RunQuery { job, .. } =
            w.run_query(tab, RunMode::All, BufferSnapshot::at_cursor(text, 0))
        else {
            panic!("expected RunQuery");
        };
        w.remember_run(tab, &job.statements);
    }

    fn facts(complete: bool) -> GridFacts {
        GridFacts {
            columns: 3,
            rows: 50,
            complete,
            selection: None,
        }
    }

    fn target(tab: TabId, result: usize) -> ExportTarget {
        ExportTarget {
            tab,
            result: Some(result),
        }
    }

    fn form(w: &Workspace) -> ExportForm {
        w.export_dialog().unwrap().form.clone()
    }

    fn start_job(w: &mut Workspace, tab: TabId, scope: ExportScope) -> ExportJob {
        w.open_export(target(tab, 0), &facts(false));
        w.export_dialog_mut().unwrap().set_scope(scope);
        let f = form(w);
        assert!(matches!(w.submit_export(f), Effect::PickExportPath { .. }));
        let Effect::RunExport(job) = w.export_path_chosen(
            w.export_dialog().unwrap().generation,
            Some(PathBuf::from("/tmp/x/out.csv")),
        ) else {
            panic!("expected RunExport");
        };
        job
    }

    #[test]
    fn remember_run_replaces_the_results_of_the_tab() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        w.remember_open_data(tab, "select * from t limit 10");
        run(&mut w, tab, "select 1; select 2");
        assert!(w.exports.origins.contains_key(&(tab, Some(0))));
        assert!(w.exports.origins.contains_key(&(tab, Some(1))));
        // A shorter run drops result 1; the grid origin of the tab stays.
        w.remember_run(
            tab,
            &[BoundStatement {
                index: 0,
                sql: "select 3".to_owned(),
                class: StatementClass::Read,
                query: Query::new("select 3"),
                values: Vec::new(),
            }],
        );
        assert_eq!(w.exports.origins[&(tab, Some(0))].sql, "select 3");
        assert!(!w.exports.origins.contains_key(&(tab, Some(1))));
        assert!(w.exports.origins.contains_key(&(tab, None)));
    }

    fn picked_facts() -> GridFacts {
        GridFacts {
            selection: Some(LoadedRows::Picked {
                rows: vec![1, 2],
                columns: vec![0],
            }),
            ..facts(false)
        }
    }

    const CHANGED: &str = "The result changed while the dialog was open; export cancelled.";

    #[test]
    fn a_result_replaced_while_the_save_dialog_is_open_starts_no_job() {
        for scope in [
            ExportScope::Loaded,
            ExportScope::Selection,
            ExportScope::Fresh,
        ] {
            let mut w = ws();
            let tab = open(&mut w, 0);
            run(&mut w, tab, "select 1");
            w.open_export(target(tab, 0), &picked_facts());
            w.export_dialog_mut().unwrap().set_scope(scope);
            let f = form(&w);
            assert!(matches!(w.submit_export(f), Effect::PickExportPath { .. }));
            // The tab runs again before the save dialog answers.
            w.remember_open_data(tab, "select 2");
            let effect = w.export_path_chosen(
                w.export_dialog().unwrap().generation,
                Some(PathBuf::from("/tmp/x/out.csv")),
            );
            assert!(effect.is_none_effect(), "{scope:?}");
            assert_eq!(w.take_notice().as_deref(), Some(CHANGED), "{scope:?}");
            assert!(w.export_status(tab).is_none(), "{scope:?}");
            assert!(w.export_dialog().is_none(), "{scope:?}");
        }
    }

    #[test]
    fn a_result_replaced_while_the_dialog_is_open_refuses_the_submit() {
        for scope in [
            ExportScope::Loaded,
            ExportScope::Selection,
            ExportScope::Fresh,
        ] {
            let mut w = ws();
            let tab = open(&mut w, 0);
            run(&mut w, tab, "select 1");
            w.open_export(target(tab, 0), &picked_facts());
            w.export_dialog_mut().unwrap().set_scope(scope);
            // A grid tab that is opened again counts the same.
            w.result_replaced(tab);
            let f = form(&w);
            assert!(w.submit_export(f).is_none_effect(), "{scope:?}");
            assert_eq!(w.take_notice().as_deref(), Some(CHANGED), "{scope:?}");
            assert!(w.export_dialog().is_none(), "{scope:?}");
        }
    }

    #[test]
    fn an_unchanged_result_still_exports_after_other_tabs_ran() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let other = open(&mut w, 1);
        run(&mut w, tab, "select 1");
        w.open_export(target(tab, 0), &facts(false));
        let f = form(&w);
        assert!(matches!(w.submit_export(f), Effect::PickExportPath { .. }));
        w.remember_open_data(other, "select 2");
        assert!(matches!(
            w.export_path_chosen(
                w.export_dialog().unwrap().generation,
                Some(PathBuf::from("/tmp/x/out.csv"))
            ),
            Effect::RunExport(_)
        ));
    }

    #[test]
    fn open_data_origins_are_classified_with_the_dialect() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        w.remember_open_data(tab, "SELECT *\nFROM \"t\"\nLIMIT 1000;\n");
        assert_eq!(w.exports.origins[&(tab, None)].class, StatementClass::Read);
        w.remember_open_data(tab, "delete from t");
        assert_eq!(w.exports.origins[&(tab, None)].class, StatementClass::Write);
        w.remember_open_data(tab, "select 1; select 2");
        assert_eq!(
            w.exports.origins[&(tab, None)].class,
            StatementClass::Unknown
        );
    }

    #[test]
    fn open_export_refuses_an_empty_result_and_a_second_export() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let mut f = facts(true);
        f.columns = 0;
        assert!(matches!(w.open_export(target(tab, 0), &f), Effect::None));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("This result has nothing to export.")
        );
        assert!(w.export_dialog().is_none());

        run(&mut w, tab, "select 1");
        let _job = start_job(&mut w, tab, ExportScope::Fresh);
        assert!(matches!(
            w.open_export(target(tab, 0), &facts(true)),
            Effect::None
        ));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("An export is already running in this tab.")
        );
    }

    #[test]
    fn a_read_result_defaults_to_a_fresh_run_and_a_complete_one_to_loaded() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 1");
        w.open_export(target(tab, 0), &facts(false));
        let d = w.export_dialog().unwrap();
        assert_eq!(d.fresh, FreshRun::Allowed);
        assert_eq!(d.form.scope, ExportScope::Fresh);
        assert_eq!(d.stem, "result-1");
        assert_eq!(d.loaded_rows, 50);
        w.open_export(target(tab, 0), &facts(true));
        let d = w.export_dialog().unwrap();
        assert_eq!(d.fresh, FreshRun::NotNeeded);
        assert_eq!(d.form.scope, ExportScope::Loaded);
    }

    #[test]
    fn a_non_read_origin_is_refused_and_never_runs() {
        let mut w = ws();
        let tab = open(&mut w, 1);
        // A write that returns rows, on a prod connection.
        w.remember_run(
            tab,
            &[BoundStatement {
                index: 0,
                sql: "delete from t returning *".to_owned(),
                class: StatementClass::Write,
                query: Query::new("delete from t returning *"),
                values: Vec::new(),
            }],
        );
        w.open_export(target(tab, 0), &facts(false));
        let d = w.export_dialog().unwrap();
        let FreshRun::Refused(message) = &d.fresh else {
            panic!("expected Refused, got {:?}", d.fresh);
        };
        assert!(message.contains("class: write"), "{message}");
        assert_eq!(d.form.scope, ExportScope::Loaded);
        // Forcing the scope is rejected and no job is produced.
        w.export_dialog_mut().unwrap().set_scope(ExportScope::Fresh);
        let f = form(&w);
        assert!(matches!(w.submit_export(f), Effect::None));
        let d = w.export_dialog().unwrap();
        assert!(d.error.as_deref().unwrap().contains("class: write"));
        assert!(
            w.export_path_chosen(
                w.export_dialog().unwrap().generation,
                Some(PathBuf::from("/tmp/o.csv"))
            )
            .is_none_effect()
        );
        assert!(w.export_status(tab).is_none());

        // Loaded rows are exported, with the note.
        w.export_dialog_mut()
            .unwrap()
            .set_scope(ExportScope::Loaded);
        let f = form(&w);
        assert!(matches!(w.submit_export(f), Effect::PickExportPath { .. }));
        let Effect::RunExport(job) = w.export_path_chosen(
            w.export_dialog().unwrap().generation,
            Some(PathBuf::from("/tmp/o.csv")),
        ) else {
            panic!("expected RunExport");
        };
        assert!(matches!(
            job.source,
            ExportSource::Loaded(LoadedRows::All { .. })
        ));
        let summary = ExportSummary {
            rows: 1234,
            bytes: 5,
            path: PathBuf::from("/tmp/o.csv"),
        };
        w.export_finished(tab, job.id, Ok(summary));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Exported 1,234 rows to o.csv (loaded rows only)")
        );
    }

    #[test]
    fn a_text_without_an_origin_or_dialect_is_refused() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        // No run was remembered.
        w.open_export(target(tab, 0), &facts(false));
        let FreshRun::Refused(message) = &w.export_dialog().unwrap().fresh else {
            panic!("expected Refused");
        };
        assert!(message.contains("no statement to run again"), "{message}");
        // A synthetic grid has no connection.
        w.dispatch(WorkspaceAction::OpenSyntheticGrid);
        let grid = w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        w.remember_open_data(grid, "select 1");
        w.open_export(
            ExportTarget {
                tab: grid,
                result: None,
            },
            &facts(false),
        );
        assert!(matches!(
            w.export_dialog().unwrap().fresh,
            FreshRun::Refused(_)
        ));
    }

    #[test]
    fn an_answer_for_an_older_dialog_is_ignored() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 1");
        w.open_export(target(tab, 0), &facts(false));
        w.export_dialog_mut()
            .unwrap()
            .set_scope(ExportScope::Loaded);
        let f = form(&w);
        let Effect::PickExportPath { dialog, .. } = w.submit_export(f) else {
            panic!("expected PickExportPath");
        };
        let stale = dialog - 1;
        let path = Some(PathBuf::from("/tmp/x/out.csv"));
        assert!(matches!(
            w.export_path_chosen(stale, path.clone()),
            Effect::None
        ));
        // The pending export is still there for the right answer.
        assert!(matches!(
            w.export_path_chosen(dialog, path),
            Effect::RunExport(_)
        ));
    }

    #[test]
    fn submit_validates_and_suggests_a_name() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 1");
        w.open_export(target(tab, 0), &facts(false));
        w.export_dialog_mut().unwrap().set_format("sql");
        let f = form(&w);
        assert!(matches!(w.submit_export(f), Effect::None));
        assert!(
            w.export_dialog()
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains("table name")
        );
        let mut f = form(&w);
        f.table = "t".to_owned();
        let Effect::PickExportPath { suggested_name, .. } = w.submit_export(f) else {
            panic!("expected PickExportPath");
        };
        assert_eq!(suggested_name, "result-1.sql");
        // A dismissed save dialog keeps the dialog.
        assert!(
            w.export_path_chosen(w.export_dialog().unwrap().generation, None)
                .is_none_effect()
        );
        assert!(w.export_dialog().is_some());
        // And nothing is pending any more.
        assert!(
            w.export_path_chosen(
                w.export_dialog().unwrap().generation,
                Some(PathBuf::from("/tmp/a.sql"))
            )
            .is_none_effect()
        );
    }

    #[test]
    fn selection_scope_needs_a_selection() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        w.open_export(target(tab, 0), &facts(true));
        w.export_dialog_mut()
            .unwrap()
            .set_scope(ExportScope::Selection);
        let f = form(&w);
        assert!(matches!(w.submit_export(f), Effect::None));
        assert!(w.export_dialog().unwrap().error.is_some());

        let mut f = facts(true);
        f.selection = Some(LoadedRows::Picked {
            rows: vec![1],
            columns: vec![0],
        });
        w.open_export(target(tab, 0), &f);
        w.export_dialog_mut()
            .unwrap()
            .set_scope(ExportScope::Selection);
        let f = form(&w);
        assert!(matches!(w.submit_export(f), Effect::PickExportPath { .. }));
        let Effect::RunExport(job) = w.export_path_chosen(
            w.export_dialog().unwrap().generation,
            Some(PathBuf::from("/tmp/s.csv")),
        ) else {
            panic!("expected RunExport");
        };
        assert!(matches!(
            job.source,
            ExportSource::Loaded(LoadedRows::Picked { .. })
        ));
    }

    #[test]
    fn a_fresh_job_carries_the_origin_query() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 7");
        let job = start_job(&mut w, tab, ExportScope::Fresh);
        let ExportSource::Fresh { request, query } = &job.source else {
            panic!("expected a fresh source");
        };
        assert_eq!(query.sql, "select 7");
        assert_eq!(request.spec.name, "dev");
        assert_eq!(job.spec.format_id, "csv");
        assert_eq!(job.spec.path, PathBuf::from("/tmp/x/out.csv"));
        let status = w.export_status(tab).unwrap();
        assert_eq!(status.file_name, "out.csv");
        assert_eq!(status.status_text(), "Exporting to out.csv… 0 rows");
        assert_eq!(w.last_export_dir(), Some(Path::new("/tmp/x")));
        // The dialog is closed; a Debug of the job has no SQL or path.
        assert!(w.export_dialog().is_none());
        let text = format!("{job:?}");
        assert!(
            !text.contains("select 7") && !text.contains("/tmp/x"),
            "{text}"
        );
    }

    #[test]
    fn the_next_dialog_starts_in_the_last_folder() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 1");
        let job = start_job(&mut w, tab, ExportScope::Loaded);
        w.export_finished(
            tab,
            job.id,
            Ok(ExportSummary {
                rows: 1,
                bytes: 1,
                path: job.spec.path.clone(),
            }),
        );
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Exported 1 row to out.csv")
        );
        w.open_export(target(tab, 0), &facts(true));
        let f = form(&w);
        let Effect::PickExportPath { directory, .. } = w.submit_export(f) else {
            panic!("expected PickExportPath");
        };
        assert_eq!(directory, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn progress_cancel_and_the_end() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 1");
        let job = start_job(&mut w, tab, ExportScope::Fresh);
        w.export_progress(
            tab,
            job.id,
            Progress {
                rows: 1234,
                bytes: 9,
            },
        );
        assert_eq!(
            w.export_status(tab).unwrap().status_text(),
            "Exporting to out.csv… 1,234 rows"
        );
        // A report of another job is ignored.
        w.export_progress(tab, job.id + 1, Progress { rows: 5, bytes: 1 });
        assert_eq!(w.export_status(tab).unwrap().rows, 1234);
        assert!(matches!(w.cancel_export(tab), Effect::None));
        assert!(job.stop.is_cancelled());
        assert_eq!(
            w.export_status(tab).unwrap().status_text(),
            "Cancelling export…"
        );
        // An outcome of another job changes nothing.
        w.export_finished(tab, job.id + 1, Err(ExportFailure::Lost));
        assert!(w.export_status(tab).is_some());
        w.export_finished(tab, job.id, Err(ExportFailure::Cancelled));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Export cancelled; no file was written.")
        );
        assert!(w.export_status(tab).is_none());
        // Cancel with nothing running.
        w.cancel_export(tab);
        assert_eq!(w.take_notice().as_deref(), Some("No export is running."));
    }

    #[test]
    fn a_failure_is_shown() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 1");
        let job = start_job(&mut w, tab, ExportScope::Fresh);
        w.export_finished(
            tab,
            job.id,
            Err(ExportFailure::Create(
                "cannot create the file: Permission denied (os error 13)".into(),
            )),
        );
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Export failed: cannot create the file: Permission denied (os error 13)")
        );
    }

    #[test]
    fn closing_the_tab_and_quitting_raise_the_stop() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 1");
        let job = start_job(&mut w, tab, ExportScope::Fresh);
        w.dispatch(WorkspaceAction::CloseTab(tab));
        assert!(job.stop.is_cancelled());
        assert!(w.export_status(tab).is_none());
        assert!(!w.exports.origins.keys().any(|(t, _)| *t == tab));
        // The late outcome is ignored.
        w.export_finished(tab, job.id, Err(ExportFailure::Cancelled));
        assert!(w.take_notice().is_none());

        let mut w = ws();
        let tab = open(&mut w, 0);
        run(&mut w, tab, "select 1");
        let job = start_job(&mut w, tab, ExportScope::Fresh);
        w.dispatch(WorkspaceAction::Quit);
        assert!(job.stop.is_cancelled());
        assert!(w.export_status(tab).is_none());
    }

    #[test]
    fn closing_the_tab_closes_its_dialog() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        w.open_export(target(tab, 0), &facts(true));
        assert!(w.export_dialog().is_some());
        w.dispatch(WorkspaceAction::CloseTab(tab));
        assert!(w.export_dialog().is_none());
        // A late answer of the save dialog does nothing.
        assert!(
            w.export_path_chosen(1, Some(PathBuf::from("/tmp/a.csv")))
                .is_none_effect()
        );
    }

    #[test]
    fn effects_print_no_sql_or_path() {
        let effect = Effect::PickExportPath {
            directory: PathBuf::from("/secret/dir"),
            suggested_name: "private-name.csv".to_owned(),
            dialog: 1,
        };
        let text = format!("{effect:?}");
        assert!(
            !text.contains("secret") && !text.contains("private"),
            "{text}"
        );
    }

    trait IsNone {
        fn is_none_effect(&self) -> bool;
    }

    impl IsNone for Effect {
        fn is_none_effect(&self) -> bool {
            matches!(self, Effect::None)
        }
    }
}
