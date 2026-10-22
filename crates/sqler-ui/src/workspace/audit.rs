// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI audit log viewer inside the workspace (ADR-0037, ADR-0050).
//!
//! State only, like history. `open_audit` and the audit actions return [`Effect::ReadAudit`];
//! the view reads the log on the blocking pool and reports through [`Workspace::audit_loaded`].
//! The viewer never touches `ai.toml`, so a broken AI configuration does not block it.

use super::state::{Effect, Workspace};
use crate::ai::{AiAction, AuditBatch, AuditPanel};
use crate::workbench::PanelView;

impl Workspace {
    /// The audit viewer state.
    #[must_use]
    pub fn audit(&self) -> &AuditPanel {
        &self.audit
    }

    /// The saved connections as (id, name), in sidebar order. The connection filter offers them.
    #[must_use]
    pub fn audit_connections(&self) -> Vec<(String, String)> {
        self.sidebar
            .entries()
            .iter()
            .map(|e| (e.id().to_string(), e.config().name.clone()))
            .collect()
    }

    /// Shows the audit view and reads its first page. When the connection the user works on
    /// changed since the view was last opened, the connection filter follows it; otherwise the
    /// user's choice (also "all connections") stays.
    pub fn open_audit(&mut self) -> Effect {
        let current = self.ai_target().map(|id| id.to_string());
        if current.as_deref() != self.audit.bound() {
            self.audit.bind(current);
        }
        self.layout.reveal_panel(PanelView::Audit);
        Effect::ReadAudit(self.audit.begin(false))
    }

    /// Takes the answer of an [`Effect::ReadAudit`]. A stale answer is dropped.
    pub fn audit_loaded(&mut self, request: u64, result: Result<AuditBatch, String>) {
        self.audit.finish(request, result);
    }

    /// Runs an audit action.
    pub(super) fn dispatch_audit(&mut self, action: AiAction) -> Effect {
        match action {
            AiAction::OpenAuditLog => self.open_audit(),
            AiAction::SetAuditFilter(change) => {
                if self.audit.set_filter(change) {
                    Effect::ReadAudit(self.audit.begin(false))
                } else {
                    Effect::None
                }
            }
            AiAction::LoadMoreAudit => {
                if self.audit.has_more() {
                    Effect::ReadAudit(self.audit.begin(true))
                } else {
                    Effect::None
                }
            }
            AiAction::CopyAuditSql => self.audit.selected_sql().map_or(Effect::None, Effect::Copy),
            AiAction::SelectAuditEntry(ix) => {
                self.audit.select(ix);
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// Fills the list with these entries (screenshots and tests) and drops a read in flight.
    #[cfg(any(test, feature = "screenshots"))]
    pub fn audit_fixture(&mut self, entries: Vec<sqler_ai_tools::AuditEntry>) {
        self.audit.set_fixture(entries);
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::ConnectionConfig;
    use sqler_driver_api::sqler_core::DriverId;
    use sqler_driver_api::sqler_core::Environment;

    use super::*;
    use crate::ai::audit::fixture;
    use crate::ai::{AiState, AuditFilterChange, AuditLoad};
    use crate::sidebar::SidebarModel;
    use crate::tabs::Tab;
    use crate::workbench::WorkbenchAction;
    use crate::workspace::WorkspaceAction;
    use crate::workspace::testing::FakeSession;

    const NOW: u64 = 1_800_000_000_000;

    fn ws() -> Workspace {
        let driver = DriverId::new("sqlite").unwrap();
        Workspace::new(SidebarModel::from_connections(vec![
            ConnectionConfig::new("dev", driver.clone(), ":memory:")
                .with_environment(Environment::Dev),
            ConnectionConfig::new("prod", driver, ":memory:").with_environment(Environment::Prod),
        ]))
    }

    fn id_of(w: &Workspace, ix: usize) -> String {
        w.sidebar().entries()[ix].id().to_string()
    }

    fn open_tab(w: &mut Workspace, ix: usize) {
        let effect = w.dispatch(WorkspaceAction::OpenConnection(ix));
        let Effect::Connect { request, .. } = effect else {
            panic!("expected Connect, got {effect:?}");
        };
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        assert!(w.tabs().active().map(Tab::id).is_some());
    }

    fn read_of(e: Effect) -> crate::ai::AuditRead {
        match e {
            Effect::ReadAudit(r) => r,
            other => panic!("expected ReadAudit, got {other:?}"),
        }
    }

    fn batch(n: usize, more: bool) -> AuditBatch {
        let entries = fixture::entries(NOW)
            .into_iter()
            .take(n)
            .collect::<Vec<_>>();
        let next = more.then(|| sqler_ai_tools::AuditCursor::new(1, "x"));
        AuditBatch {
            entries,
            next,
            ..AuditBatch::default()
        }
    }

    #[test]
    fn open_with_an_active_tab_binds_the_connection() {
        let mut w = ws();
        open_tab(&mut w, 1);
        let read = read_of(w.open_audit());
        assert_eq!(read.query.connection, Some(id_of(&w, 1)));
        assert_eq!(w.audit().filters().connection, Some(id_of(&w, 1)));
        assert_eq!(w.layout().panel_view(), PanelView::Audit);
        assert!(w.layout().panel_visible());
        assert_eq!(*w.audit().load(), AuditLoad::Loading);
    }

    #[test]
    fn open_without_a_tab_means_all_connections() {
        let mut w = ws();
        let read = read_of(w.open_audit());
        assert_eq!(read.query.connection, None);
        assert_eq!(w.audit().bound(), None);
    }

    #[test]
    fn open_follows_the_sidebar_selection_without_a_tab() {
        let mut w = ws();
        let id = w.sidebar().entries()[0].id().clone();
        w.sidebar.select_connection(&id);
        let read = read_of(w.open_audit());
        assert_eq!(read.query.connection, Some(id.to_string()));
    }

    #[test]
    fn reopening_on_the_same_connection_keeps_all() {
        let mut w = ws();
        open_tab(&mut w, 0);
        w.open_audit();
        w.dispatch_ai(AiAction::SetAuditFilter(AuditFilterChange::Connection(
            None,
        )));
        let read = read_of(w.open_audit());
        assert_eq!(read.query.connection, None);
        // Another connection becomes the target: the filter follows it again.
        open_tab(&mut w, 1);
        let read = read_of(w.open_audit());
        assert_eq!(read.query.connection, Some(id_of(&w, 1)));
    }

    #[test]
    fn show_audit_from_the_workbench_reads_the_log() {
        let mut w = ws();
        let e = w.dispatch_workbench(WorkbenchAction::ShowPanelView(PanelView::Audit));
        read_of(e);
        assert_eq!(w.layout().panel_view(), PanelView::Audit);
        assert!(matches!(
            w.dispatch_workbench(WorkbenchAction::ShowPanelView(PanelView::Messages)),
            Effect::None
        ));
    }

    #[test]
    fn stale_answers_are_ignored_and_more_appends() {
        let mut w = ws();
        w.audit.set_page_size(3);
        let first = read_of(w.open_audit());
        let second = read_of(w.open_audit());
        w.audit_loaded(first.request, Ok(batch(8, false)));
        assert_eq!(w.audit().entries().len(), 0);
        w.audit_loaded(second.request, Ok(batch(3, true)));
        assert_eq!(w.audit().entries().len(), 3);
        assert_eq!(*w.audit().load(), AuditLoad::Ready);
        let more = read_of(w.dispatch_ai(AiAction::LoadMoreAudit));
        assert!(more.query.cursor.is_some());
        w.audit_loaded(more.request, Ok(batch(2, false)));
        assert_eq!(w.audit().entries().len(), 5);
        assert!(matches!(
            w.dispatch_ai(AiAction::LoadMoreAudit),
            Effect::None
        ));
    }

    #[test]
    fn load_more_is_refused_while_a_first_page_is_pending() {
        let mut w = ws();
        let r = read_of(w.open_audit());
        w.audit_loaded(r.request, Ok(batch(3, true)));
        assert!(w.audit().has_more());
        let change = AuditFilterChange::Tool(Some("run_query".into()));
        read_of(w.dispatch_ai(AiAction::SetAuditFilter(change)));
        assert!(!w.audit().has_more());
        assert!(matches!(
            w.dispatch_ai(AiAction::LoadMoreAudit),
            Effect::None
        ));
        let id = crate::command::CommandId::new("ai:load-more-audit").unwrap();
        assert_eq!(crate::ai::action_for(&id, &w), None);
    }

    #[test]
    fn a_filter_change_rereads_from_the_first_page() {
        let mut w = ws();
        let r = read_of(w.open_audit());
        w.audit_loaded(r.request, Ok(batch(3, true)));
        let change = AuditFilterChange::Tool(Some("run_query".into()));
        let r = read_of(w.dispatch_ai(AiAction::SetAuditFilter(change.clone())));
        assert!(r.query.cursor.is_none());
        assert_eq!(r.query.tool.as_deref(), Some("run_query"));
        assert!(matches!(
            w.dispatch_ai(AiAction::SetAuditFilter(change)),
            Effect::None
        ));
    }

    #[test]
    fn copy_returns_the_sql_of_the_selection() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch_ai(AiAction::CopyAuditSql),
            Effect::None
        ));
        w.audit_fixture(fixture::entries(NOW));
        w.dispatch_ai(AiAction::SelectAuditEntry(5));
        match w.dispatch_ai(AiAction::CopyAuditSql) {
            Effect::Copy(sql) => assert_eq!(sql, "update acct set ssn = ?;"),
            other => panic!("expected Copy, got {other:?}"),
        }
        assert!(!format!("{:?}", w.dispatch_ai(AiAction::CopyAuditSql)).contains("ssn"));
    }

    #[test]
    fn the_fixture_drops_a_read_in_flight() {
        let mut w = ws();
        let r = read_of(w.open_audit());
        w.audit_fixture(fixture::entries(NOW));
        w.audit_loaded(r.request, Ok(batch(1, false)));
        assert_eq!(w.audit().entries().len(), 8);
    }

    #[test]
    fn a_broken_ai_toml_does_not_block_the_viewer() {
        let ai = AiState::from_startup(&crate::ai::AiStartup {
            config: Err("bad toml".into()),
            dir: None,
            factory: std::sync::Arc::new(crate::ai::NoProviders),
        });
        let mut w = ws().with_ai(ai);
        assert!(w.ai().load_error().is_some());
        read_of(w.dispatch_ai(AiAction::OpenAuditLog));
        assert!(w.take_notice().is_none());
    }

    #[test]
    fn the_connection_list_follows_the_sidebar() {
        let w = ws();
        let list = w.audit_connections();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].1, "prod");
        assert_eq!(list[0].0, id_of(&w, 0));
    }
}
