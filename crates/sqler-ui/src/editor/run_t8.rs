// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The real [`RunPort`]: it hands the editor buffer to query execution (ADR-0024).

use crate::query::{self, BufferSnapshot};
use crate::tabs::TabId;
use crate::workspace::{Effect, Workspace};

use super::{EditorBuffer, RunMode, RunPort};

/// The [`RunPort`] over `Workspace::run_query` and `Workspace::cancel_query`.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryRunPort;

/// The query-side run mode that matches the editor's.
#[must_use]
pub fn query_mode(mode: RunMode) -> query::RunMode {
    match mode {
        RunMode::Selection => query::RunMode::Selection,
        RunMode::Statement => query::RunMode::Statement,
        RunMode::All => query::RunMode::All,
    }
}

/// The editor-side run mode for a query-side one.
#[must_use]
pub fn run_mode(mode: query::RunMode) -> RunMode {
    match mode {
        query::RunMode::Selection => RunMode::Selection,
        query::RunMode::Statement => RunMode::Statement,
        // `query::RunMode` is non-exhaustive: `All` and anything added later run the whole script.
        _ => RunMode::All,
    }
}

/// The buffer in the form query execution reads: same text, selection and cursor.
#[must_use]
pub fn run_snapshot(buffer: EditorBuffer) -> BufferSnapshot {
    BufferSnapshot {
        text: buffer.text,
        selection: buffer.selection,
        cursor: buffer.cursor,
    }
}

impl RunPort for QueryRunPort {
    fn run(
        &mut self,
        ws: &mut Workspace,
        tab: TabId,
        mode: RunMode,
        buffer: EditorBuffer,
    ) -> Result<Effect, String> {
        Ok(ws.run_query(tab, query_mode(mode), run_snapshot(buffer)))
    }

    fn cancel(&mut self, ws: &mut Workspace, tab: TabId) -> Result<Effect, String> {
        Ok(ws.cancel_query(tab))
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

    use super::*;
    use crate::query::StatementStatus;
    use crate::sidebar::SidebarModel;
    use crate::workspace::WorkspaceAction;
    use crate::workspace::testing::FakeSession;

    /// A workspace with one open editor tab on a fake session.
    fn open() -> (Workspace, TabId) {
        let driver = DriverId::new("sqlite").unwrap();
        let mut w = Workspace::new(SidebarModel::from_connections(vec![ConnectionConfig::new(
            "dev", driver, ":memory:",
        )]));
        let Effect::Connect { request, .. } = w.dispatch(WorkspaceAction::OpenConnection(0)) else {
            panic!("expected Connect");
        };
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        let tab = w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        (w, tab)
    }

    /// The SQL of the statements a run command started.
    fn started_sql(effect: &Effect) -> Vec<String> {
        match effect {
            Effect::RunQuery { job, .. } => job.statements.iter().map(|s| s.sql.clone()).collect(),
            other => panic!("expected RunQuery, got {other:?}"),
        }
    }

    const SCRIPT: &str = "select 1;\nselect 2;\nselect 3;";

    #[test]
    fn modes_and_snapshot_map_one_to_one() {
        for (editor, query) in [
            (RunMode::Selection, query::RunMode::Selection),
            (RunMode::Statement, query::RunMode::Statement),
            (RunMode::All, query::RunMode::All),
        ] {
            assert_eq!(query_mode(editor), query);
            assert_eq!(run_mode(query), editor);
        }
        let buffer = EditorBuffer {
            text: "select 1".to_owned(),
            selection: 2..4,
            cursor: 4,
        };
        let snapshot = run_snapshot(buffer);
        assert_eq!(snapshot.text, "select 1");
        assert_eq!(snapshot.selection, 2..4);
        assert_eq!(snapshot.cursor, 4);
    }

    #[test]
    fn run_selection_reads_the_editor_selection() {
        let (mut w, _) = open();
        let start = SCRIPT.find("select 2").unwrap();
        let buffer = EditorBuffer {
            text: SCRIPT.to_owned(),
            selection: start..start + "select 2".len(),
            cursor: start,
        };
        let effect = w.run_editor(&mut QueryRunPort, RunMode::Selection, Some(buffer));
        assert_eq!(started_sql(&effect), ["select 2"]);
    }

    #[test]
    fn run_statement_with_a_selection_runs_exactly_the_selection() {
        let (mut w, _) = open();
        // A partial-line selection of the second statement, cursor elsewhere.
        let start = SCRIPT.find("select 2").unwrap();
        let buffer = EditorBuffer {
            text: SCRIPT.to_owned(),
            selection: start..start + "select".len(),
            cursor: start + "select".len(),
        };
        let effect = w.run_editor(&mut QueryRunPort, RunMode::Statement, Some(buffer));
        assert_eq!(started_sql(&effect), ["select"]);
        // A multi-statement selection runs each statement; the editor text is never edited.
        let buffer = EditorBuffer {
            text: SCRIPT.to_owned(),
            selection: 0..SCRIPT.len() - "select 3;".len(),
            cursor: 0,
        };
        let (mut w, _) = open();
        let effect = w.run_editor(&mut QueryRunPort, RunMode::Statement, Some(buffer));
        assert!(matches!(effect, Effect::RunQuery { .. }), "{effect:?}");
        assert_eq!(started_sql(&effect), ["select 1", "select 2"]);
    }

    #[test]
    fn run_statement_does_not_split_at_blank_lines() {
        let (mut w, _) = open();
        let text = "update t\n\nset x=1";
        let effect = w.run_editor(
            &mut QueryRunPort,
            RunMode::Statement,
            Some(EditorBuffer::new(text, text.len())),
        );
        assert_eq!(started_sql(&effect), [text]);
    }

    #[test]
    fn run_statement_with_a_selection_runs_the_whole_selection() {
        let (mut w, _) = open();
        let text = "insert into t (a)\n\nselect 1";
        let mut buffer = EditorBuffer::new(text, text.len());
        buffer.selection = 0..text.len();
        let effect = w.run_editor(&mut QueryRunPort, RunMode::Statement, Some(buffer));
        assert_eq!(started_sql(&effect), [text]);
    }

    #[test]
    fn run_statement_reads_the_editor_cursor() {
        let (mut w, _) = open();
        let cursor = SCRIPT.find("select 3").unwrap() + 3;
        let effect = w.run_editor(
            &mut QueryRunPort,
            RunMode::Statement,
            Some(EditorBuffer::new(SCRIPT, cursor)),
        );
        assert_eq!(started_sql(&effect), ["select 3"]);
    }

    #[test]
    fn run_all_runs_every_statement_in_order() {
        let (mut w, _) = open();
        let effect = w.run_editor(
            &mut QueryRunPort,
            RunMode::All,
            Some(EditorBuffer::new(SCRIPT, 0)),
        );
        assert_eq!(started_sql(&effect), ["select 1", "select 2", "select 3"]);
    }

    #[test]
    fn results_of_a_run_from_the_editor_land_in_the_query_tab() {
        let (mut w, tab) = open();
        let effect = w.run_editor(
            &mut QueryRunPort,
            RunMode::All,
            Some(EditorBuffer::new("select 1; select 2", 0)),
        );
        let Effect::RunQuery {
            job,
            grids,
            mut events,
        } = effect
        else {
            panic!("expected RunQuery");
        };
        assert_eq!(grids.len(), 2);
        let run = job.run;
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let done = rt.block_on(job.run());
        // The fake session fails every statement: the first shows the error, the second is not run.
        while let Ok(event) = events.try_recv() {
            w.query_event(tab, run, event);
        }
        assert!(matches!(w.query_finished(done), Effect::None));
        let results = w.query_tab(tab).unwrap().results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].sql, "select 1");
        assert!(matches!(results[0].status, StatementStatus::Failed { .. }));
        assert_eq!(results[1].status, StatementStatus::NotRun);
        assert!(w.has_session(tab));
    }

    #[test]
    fn cancel_goes_to_query_execution() {
        let (mut w, tab) = open();
        // Nothing is running: the query side says so.
        assert!(matches!(
            w.cancel_editor_run(&mut QueryRunPort),
            Effect::None
        ));
        assert_eq!(w.take_notice().as_deref(), Some("No query is running."));
        let effect = w.run_editor(
            &mut QueryRunPort,
            RunMode::All,
            Some(EditorBuffer::new("select 1", 0)),
        );
        assert!(matches!(effect, Effect::RunQuery { .. }));
        assert!(matches!(
            w.cancel_editor_run(&mut QueryRunPort),
            Effect::CancelQuery(_) | Effect::None
        ));
        assert!(w.query_tab(tab).unwrap().is_running());
    }

    #[test]
    fn no_editor_buffer_runs_nothing() {
        let (mut w, _) = open();
        let effect = w.run_editor(&mut QueryRunPort, RunMode::All, None);
        assert!(matches!(effect, Effect::None));
    }
}
