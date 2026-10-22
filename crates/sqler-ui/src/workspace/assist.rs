// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The editor AI actions inside the workspace (ADR-0052): generate SQL, explain a query, fix a
//! failed statement.
//!
//! State only. [`Workspace::dispatch_assist`] checks the gate and returns [`Effect::RunAssist`];
//! the view runs `run_assist` on tokio and reports with [`Workspace::assist_finished`]. The only
//! way text reaches the editor is [`Effect::EditorEdit`]; nothing here returns `RunQuery`, so
//! running generated SQL stays on the existing run path and its production gate.

use sqler_ai_tools::ConnectionInfo;
use sqler_config::ProviderConfig;
use sqler_sql::Dialect;

use super::state::{Effect, Workspace};
use crate::ai::editor::{
    AI_CONFIG_BROKEN, AI_OFF, AssistKind, AssistOutcome, AssistRequest, AssistState, BUSY,
    FIX_STALE, NO_EDITOR, NO_FAILURE, NO_MODEL, NO_STATEMENT, NOT_BUILT, PromptBar, TabAssist,
    explain_target, fix_edit, insert_edit,
};
use crate::ai::{AiAction, AssistJob};
use crate::editor::EditorBuffer;
use crate::query::{AiToolbar, BufferSnapshot, StatementStatus};
use crate::tabs::{TabId, TabKind};
use crate::workbench::PanelView;

impl Workspace {
    /// The assistant of a tab: its prompt bar and the state of the last request.
    #[must_use]
    pub fn assist(&self, tab: TabId) -> Option<&TabAssist> {
        self.assist.get(tab)
    }

    /// The tab whose request `id` is running.
    #[must_use]
    pub fn assist_request_tab(&self, id: u64) -> Option<TabId> {
        self.assist.tab_of(id)
    }

    /// The active tab when it is a query tab: the one AI commands act on.
    #[must_use]
    pub(crate) fn assist_tab(&self) -> Option<TabId> {
        let tab = self.tabs.active()?;
        self.is_query_tab(tab.id()).then(|| tab.id())
    }

    fn is_query_tab(&self, tab: TabId) -> bool {
        self.tabs.get(tab).map(crate::tabs::Tab::kind) == Some(TabKind::Query)
            && self.queries.contains_key(&tab)
    }

    /// Whether a request runs on the tab.
    pub(crate) fn assist_running(&self, tab: TabId) -> bool {
        matches!(
            self.assist.get(tab).map(|a| &a.state),
            Some(AssistState::Running { .. })
        )
    }

    /// Whether the tab holds a fix that can be accepted.
    pub(crate) fn assist_has_fix(&self, tab: TabId) -> bool {
        matches!(
            self.assist.get(tab).map(|a| &a.state),
            Some(AssistState::Done(AssistOutcome::Fix {
                applicable: true,
                ..
            }))
        )
    }

    /// Why each AI item of the query toolbar is disabled.
    #[must_use]
    pub fn ai_toolbar(&self, tab: TabId) -> AiToolbar {
        if !self.is_query_tab(tab) {
            return AiToolbar::unavailable();
        }
        let reason = |kind| self.assist_gate(tab, kind).err();
        AiToolbar {
            generate: reason(AssistKind::Generate),
            explain: reason(AssistKind::Explain),
            fix: reason(AssistKind::Fix),
        }
    }

    /// The gate of a request: the provider and model to use, or why it is blocked.
    fn assist_gate(
        &self,
        tab: TabId,
        kind: AssistKind,
    ) -> Result<(ProviderConfig, String), &'static str> {
        if !self.is_query_tab(tab) {
            return Err(NO_EDITOR);
        }
        let profile = self
            .tab_conns
            .get(&tab)
            .and_then(|id| self.sidebar.profile(id))
            .ok_or(NO_EDITOR)?;
        if !profile.spec().ai.enabled {
            return Err(AI_OFF);
        }
        if self.ai.load_error().is_some() {
            return Err(AI_CONFIG_BROKEN);
        }
        self.ai_audit()?;
        let config = self.ai.config();
        let choice = config
            .models
            .for_use_case(kind.use_case())
            .ok_or(NO_MODEL)?;
        let provider = config.provider(&choice.provider).ok_or(NO_MODEL)?;
        if !self.ai.kinds().contains(&provider.kind) {
            return Err(NOT_BUILT);
        }
        if self.assist_running(tab) {
            return Err(BUSY);
        }
        if kind == AssistKind::Fix && self.first_failed(tab).is_none() {
            return Err(NO_FAILURE);
        }
        Ok((provider.clone(), choice.model.clone()))
    }

    /// The position of the first failed statement of the last run.
    fn first_failed(&self, tab: TabId) -> Option<usize> {
        self.queries
            .get(&tab)?
            .results()
            .iter()
            .position(|r| matches!(r.status, StatementStatus::Failed { .. }))
    }

    /// The statement at `position` of the last run, when it failed: its SQL and error text.
    fn failed_statement(&self, tab: TabId, position: usize) -> Option<(String, String)> {
        let entry = self.queries.get(&tab)?.results().get(position)?;
        match &entry.status {
            StatementStatus::Failed { message, .. } => Some((entry.sql.clone(), message.clone())),
            _ => None,
        }
    }

    fn assist_notice(&mut self, message: &str) -> Effect {
        self.notice = Some(message.to_owned());
        Effect::None
    }

    /// Reports why a request is blocked. A missing model also shows the AI view.
    fn assist_blocked(&mut self, reason: &'static str) -> Effect {
        if reason == NO_MODEL {
            return self.no_provider();
        }
        self.assist_notice(reason)
    }

    /// Starts a request: marks the tab as running and builds the job.
    fn start_assist(
        &mut self,
        tab: TabId,
        request: AssistRequest,
        provider: ProviderConfig,
        model: String,
    ) -> Effect {
        let Some(profile) = self
            .tab_conns
            .get(&tab)
            .and_then(|id| self.sidebar.profile(id))
        else {
            return self.assist_notice(NO_EDITOR);
        };
        let spec = profile.spec();
        let info = ConnectionInfo::new(
            profile.id().as_str(),
            spec.name.clone(),
            spec.driver.clone(),
            spec.environment,
        )
        .with_send_rows(profile.ai_sends_rows());
        let audit = match self.ai_audit() {
            Ok(audit) => audit.clone(),
            Err(message) => return self.assist_notice(message),
        };
        let kind = request.kind();
        let id = self.assist.next_id();
        let job = AssistJob {
            id,
            tab,
            request,
            provider,
            model,
            info,
            catalog: self.editor_catalog(tab).map(|(_, catalog)| catalog),
            config: self.ai.config().clone(),
            audit,
        };
        self.assist.entry(tab).state = AssistState::Running { id, kind };
        Effect::RunAssist(Box::new(job))
    }

    fn reveal_assistant(&mut self) {
        self.layout.reveal_panel(PanelView::Assistant);
    }

    /// Runs an editor AI action on the active query tab. `buffer` is the editor text for the
    /// actions that need it ([`AiAction::needs_buffer`]).
    #[allow(clippy::too_many_lines)] // one arm per action
    pub fn dispatch_assist(&mut self, action: AiAction, buffer: Option<EditorBuffer>) -> Effect {
        let Some(tab) = self.assist_tab() else {
            return self.assist_notice(NO_EDITOR);
        };
        match action {
            AiAction::GenerateSql => {
                if let Err(reason) = self.assist_gate(tab, AssistKind::Generate)
                    && reason != BUSY
                {
                    return self.assist_blocked(reason);
                }
                let a = self.assist.entry(tab);
                if a.prompt.is_none() {
                    a.prompt = Some(PromptBar::default());
                    if matches!(
                        a.state,
                        AssistState::Done(AssistOutcome::Sql { .. })
                            | AssistState::Failed {
                                kind: AssistKind::Generate,
                                ..
                            }
                            | AssistState::Cancelled(AssistKind::Generate)
                    ) {
                        a.state = AssistState::Idle;
                    }
                }
                Effect::None
            }
            AiAction::SetPrompt(text) => {
                if let Some(bar) = self.assist.entry(tab).prompt.as_mut() {
                    bar.text = text;
                }
                Effect::None
            }
            AiAction::SubmitPrompt => {
                let question = self
                    .assist
                    .get(tab)
                    .and_then(|a| a.prompt.as_ref())
                    .map(|bar| bar.text.trim().to_owned())
                    .filter(|q| !q.is_empty());
                let Some(question) = question else {
                    return Effect::None;
                };
                match self.assist_gate(tab, AssistKind::Generate) {
                    Ok((provider, model)) => self.start_assist(
                        tab,
                        AssistRequest::Generate { question },
                        provider,
                        model,
                    ),
                    Err(reason) => self.assist_blocked(reason),
                }
            }
            AiAction::ClosePrompt => {
                let a = self.assist.entry(tab);
                a.prompt = None;
                match a.state {
                    AssistState::Running {
                        kind: AssistKind::Generate,
                        ..
                    } => self.cancel_assist(tab),
                    _ => Effect::None,
                }
            }
            AiAction::ExplainQuery => {
                let (provider, model) = match self.assist_gate(tab, AssistKind::Explain) {
                    Ok(ok) => ok,
                    Err(reason) => return self.assist_blocked(reason),
                };
                let Some(buffer) = buffer else {
                    return self.assist_notice(NO_EDITOR);
                };
                let dialect = self
                    .tabs
                    .get(tab)
                    .and_then(|t| Dialect::from_driver_id(t.connection().driver.as_str()));
                let snapshot = BufferSnapshot {
                    text: buffer.text,
                    selection: buffer.selection,
                    cursor: buffer.cursor,
                };
                let Some(sql) = explain_target(dialect, &snapshot) else {
                    return self.assist_notice(NO_STATEMENT);
                };
                self.reveal_assistant();
                self.start_assist(tab, AssistRequest::Explain { sql }, provider, model)
            }
            AiAction::FixQuery => match self.first_failed(tab) {
                Some(position) => self.fix_statement(tab, position),
                None => match self.assist_gate(tab, AssistKind::Fix) {
                    Err(reason) => self.assist_blocked(reason),
                    Ok(_) => self.assist_notice(NO_FAILURE),
                },
            },
            AiAction::FixStatement(position) => self.fix_statement(tab, position),
            AiAction::CancelAssist => self.cancel_assist(tab),
            AiAction::AcceptFix => self.accept_fix(tab, buffer),
            AiAction::RejectFix => {
                if matches!(
                    self.assist.get(tab).map(|a| &a.state),
                    Some(AssistState::Done(AssistOutcome::Fix { .. }))
                ) {
                    self.assist.entry(tab).state = AssistState::Idle;
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }

    fn fix_statement(&mut self, tab: TabId, position: usize) -> Effect {
        let (provider, model) = match self.assist_gate(tab, AssistKind::Fix) {
            Ok(ok) => ok,
            Err(reason) => return self.assist_blocked(reason),
        };
        let Some((sql, error)) = self.failed_statement(tab, position) else {
            return self.assist_notice(NO_FAILURE);
        };
        self.reveal_assistant();
        self.start_assist(
            tab,
            AssistRequest::Fix {
                sql,
                error,
                statement: position,
            },
            provider,
            model,
        )
    }

    fn cancel_assist(&mut self, tab: TabId) -> Effect {
        let a = self.assist.entry(tab);
        if let AssistState::Running { id, kind } = a.state {
            a.state = AssistState::Cancelled(kind);
            return Effect::CancelAssist(id);
        }
        Effect::None
    }

    fn accept_fix(&mut self, tab: TabId, buffer: Option<EditorBuffer>) -> Effect {
        let fix = match self.assist.get(tab).map(|a| &a.state) {
            Some(AssistState::Done(AssistOutcome::Fix {
                original,
                sql,
                applicable: true,
                ..
            })) => Some((original.clone(), sql.clone())),
            _ => None,
        };
        let Some((original, sql)) = fix else {
            return Effect::None;
        };
        let Some(buffer) = buffer else {
            return self.assist_notice(NO_EDITOR);
        };
        let dialect = self
            .tabs
            .get(tab)
            .and_then(|t| Dialect::from_driver_id(t.connection().driver.as_str()));
        match fix_edit(dialect, &buffer.text, &original, &sql) {
            Ok(edit) => {
                self.assist.entry(tab).state = AssistState::FixApplied;
                Effect::EditorEdit { tab, edit }
            }
            Err(message) => {
                debug_assert_eq!(message, FIX_STALE);
                self.assist_notice(message)
            }
        }
    }

    /// A request ended. An id that is unknown, stale, cancelled or whose tab is gone is ignored.
    /// `buffer` is the editor text of the request's tab (see [`Workspace::assist_request_tab`]).
    ///
    /// Generated SQL is inserted into the editor unless it holds a redacted value. It is never
    /// run: the only effect this returns is an `EditorEdit`.
    #[allow(clippy::needless_pass_by_value)] // the view hands the buffer over
    pub fn assist_finished(
        &mut self,
        id: u64,
        result: Result<AssistOutcome, String>,
        buffer: Option<EditorBuffer>,
    ) -> Effect {
        let Some(tab) = self.assist.tab_of(id) else {
            return Effect::None;
        };
        if self.tabs.get(tab).is_none() {
            return Effect::None;
        }
        let kind = match self.assist.get(tab).map(|a| &a.state) {
            Some(AssistState::Running { kind, .. }) => *kind,
            _ => return Effect::None,
        };
        match result {
            Err(message) => {
                self.assist.entry(tab).state = AssistState::Failed { kind, message };
                Effect::None
            }
            Ok(AssistOutcome::Sql {
                sql,
                rationale,
                tables,
                applicable,
                ..
            }) => {
                let edit = match &buffer {
                    Some(buffer) if applicable => Some(insert_edit(buffer, &sql)),
                    _ => None,
                };
                let inserted = edit.is_some();
                self.assist.entry(tab).state = AssistState::Done(AssistOutcome::Sql {
                    sql,
                    rationale,
                    tables,
                    applicable,
                    inserted,
                });
                match edit {
                    Some(edit) => Effect::EditorEdit { tab, edit },
                    None => Effect::None,
                }
            }
            Ok(outcome) => {
                self.assist.entry(tab).state = AssistState::Done(outcome);
                Effect::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqler_ai_api::AiProvider;
    use sqler_config::{
        AiConfig, ConnectionAi, ConnectionSpec, ModelChoice, ProviderConfig, ProviderId,
        ProviderKind,
    };
    use sqler_driver_api::sqler_core::{DriverId, Environment};

    use super::*;
    use crate::ai::AiState;
    use crate::ai::editor::job_tests::{OneProvider, scripted};
    use crate::ai::editor::{CANCELLED, REDACTED_SQL};
    use crate::query::{Phase, RunMode};
    use crate::sidebar::SidebarModel;
    use crate::workspace::WorkspaceAction;
    use crate::workspace::testing::FakeSession;

    fn config() -> AiConfig {
        let mut c = AiConfig::default();
        let id = ProviderId::new("local").unwrap();
        c.providers
            .push(ProviderConfig::new(id.clone(), ProviderKind::Ollama));
        c.models.default = Some(ModelChoice::new(id, "m"));
        c
    }

    fn workspace(ai_on: bool, config: AiConfig, kinds: Vec<ProviderKind>) -> Workspace {
        let mut sidebar = SidebarModel::default();
        sidebar
            .add_connection(
                ConnectionSpec::new("dev", DriverId::new("sqlite").unwrap(), ":memory:")
                    .with_environment(Environment::Prod)
                    .with_ai(ConnectionAi::new(ai_on)),
                None,
            )
            .unwrap();
        Workspace::new(sidebar)
            .with_ai(AiState::new(config, kinds, None))
            .with_ai_audit(Some(crate::ai::AiAudit::in_memory()))
    }

    fn open(w: &mut Workspace) -> TabId {
        let Effect::Connect { request, .. } = w.dispatch(WorkspaceAction::OpenConnection(0)) else {
            panic!("expected Connect");
        };
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        w.tabs().active().map(crate::tabs::Tab::id).unwrap()
    }

    fn ready() -> (Workspace, TabId) {
        let mut w = workspace(true, config(), vec![ProviderKind::Ollama]);
        let tab = open(&mut w);
        (w, tab)
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// Runs the job the way the view does, with the scripted provider.
    fn run_job(
        effect: Effect,
        provider: Arc<dyn AiProvider>,
    ) -> (u64, Result<AssistOutcome, String>) {
        let Effect::RunAssist(job) = effect else {
            panic!("expected RunAssist, got {effect:?}");
        };
        let id = job.id;
        let (_keep, rx) = tokio::sync::oneshot::channel();
        let result = rt().block_on(crate::ai::run_assist(
            *job,
            Arc::new(OneProvider(provider)),
            rx,
        ));
        (id, result)
    }

    /// Every effect, nested batches included, is not a run.
    fn never_runs(effect: &Effect) {
        match effect {
            Effect::RunQuery { .. } => panic!("an AI action started a query"),
            Effect::Batch(all) => all.iter().for_each(never_runs),
            _ => {}
        }
    }

    /// Fails `select nme from customers` through the real run path.
    fn fail_a_statement(w: &mut Workspace, tab: TabId) {
        let effect = w.run_query(
            tab,
            RunMode::All,
            BufferSnapshot::at_cursor("select nme from customers", 0),
        );
        let Effect::RunQuery {
            job, mut events, ..
        } = effect
        else {
            panic!("expected RunQuery");
        };
        let run = job.run;
        let done = rt().block_on(job.run());
        while let Ok(event) = events.try_recv() {
            w.query_event(tab, run, event);
        }
        w.query_finished(done);
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
    }

    #[test]
    fn generate_inserts_at_the_cursor_and_never_runs() {
        let (mut w, tab) = ready();
        assert!(matches!(
            w.dispatch_assist(AiAction::GenerateSql, None),
            Effect::None
        ));
        assert!(w.assist(tab).unwrap().prompt.is_some());
        w.dispatch_assist(AiAction::SetPrompt("customers".into()), None);
        let effect = w.dispatch_assist(AiAction::SubmitPrompt, None);
        never_runs(&effect);
        assert!(matches!(
            w.assist(tab).unwrap().state,
            AssistState::Running {
                kind: AssistKind::Generate,
                ..
            }
        ));
        let p = scripted(&["```sql\nselect * from customers\n```\nAll of them."]);
        let (id, result) = run_job(effect, p);
        assert_eq!(w.assist_request_tab(id), Some(tab));
        let buf = EditorBuffer::new("-- q\n", 5);
        let effect = w.assist_finished(id, result, Some(buf));
        never_runs(&effect);
        let Effect::EditorEdit { tab: t, edit } = effect else {
            panic!("expected EditorEdit");
        };
        assert_eq!(t, tab);
        assert_eq!(edit.text, "select * from customers");
        assert!(matches!(
            w.assist(tab).unwrap().state,
            AssistState::Done(AssistOutcome::Sql { inserted: true, .. })
        ));
        assert_eq!(*w.query_tab(tab).unwrap().phase(), Phase::Idle);
    }

    #[test]
    fn explain_reveals_the_assistant() {
        let (mut w, tab) = ready();
        let buf = EditorBuffer::new("select 1;", 3);
        let effect = w.dispatch_assist(AiAction::ExplainQuery, Some(buf));
        never_runs(&effect);
        assert_eq!(w.layout().panel_view(), PanelView::Assistant);
        assert!(w.layout().panel_visible());
        let p = scripted(&["It selects one."]);
        let (id, result) = run_job(effect, p);
        let effect = w.assist_finished(id, result, None);
        assert!(matches!(effect, Effect::None));
        assert!(matches!(
            &w.assist(tab).unwrap().state,
            AssistState::Done(AssistOutcome::Explanation { sql, text })
                if sql == "select 1" && text == "It selects one."
        ));
    }

    #[test]
    fn explain_without_a_statement_says_so() {
        let (mut w, _tab) = ready();
        let effect = w.dispatch_assist(AiAction::ExplainQuery, Some(EditorBuffer::new("  ", 0)));
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(NO_STATEMENT));
    }

    #[test]
    fn fix_then_accept_replaces_the_statement() {
        let (mut w, tab) = ready();
        fail_a_statement(&mut w, tab);
        let effect = w.dispatch_assist(AiAction::FixQuery, None);
        never_runs(&effect);
        assert_eq!(w.layout().panel_view(), PanelView::Assistant);
        let p = scripted(&["```sql\nselect name from customers\n```\nThe column is name."]);
        let (id, result) = run_job(effect, p);
        assert!(matches!(w.assist_finished(id, result, None), Effect::None));
        assert!(w.assist_has_fix(tab));
        let text = "-- x\nselect nme from customers\n";
        let effect = w.dispatch_assist(AiAction::AcceptFix, Some(EditorBuffer::new(text, 0)));
        never_runs(&effect);
        let Effect::EditorEdit { edit, .. } = effect else {
            panic!("expected EditorEdit");
        };
        assert_eq!(edit.text, "select name from customers");
        assert_eq!(&text[edit.range], "select nme from customers");
        assert_eq!(w.assist(tab).unwrap().state, AssistState::FixApplied);
    }

    #[test]
    fn a_stale_accept_sets_the_notice_and_keeps_the_fix() {
        let (mut w, tab) = ready();
        fail_a_statement(&mut w, tab);
        let effect = w.dispatch_assist(AiAction::FixStatement(0), None);
        let p = scripted(&["```sql\nselect name from customers\n```\nName."]);
        let (id, result) = run_job(effect, p);
        w.assist_finished(id, result, None);
        let effect = w.dispatch_assist(
            AiAction::AcceptFix,
            Some(EditorBuffer::new("select other", 0)),
        );
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(FIX_STALE));
        assert!(w.assist_has_fix(tab));
        assert!(matches!(
            w.dispatch_assist(AiAction::RejectFix, None),
            Effect::None
        ));
        assert_eq!(w.assist(tab).unwrap().state, AssistState::Idle);
    }

    #[test]
    fn a_redacted_fix_cannot_be_accepted() {
        let (mut w, tab) = ready();
        fail_a_statement(&mut w, tab);
        let effect = w.dispatch_assist(AiAction::FixQuery, None);
        let p = scripted(&["```sql\nselect * from customers where token = '<redacted>'\n```\nx"]);
        let (id, result) = run_job(effect, p);
        w.assist_finished(id, result, None);
        assert!(!w.assist_has_fix(tab));
        let effect = w.dispatch_assist(
            AiAction::AcceptFix,
            Some(EditorBuffer::new("select nme from customers", 0)),
        );
        assert!(matches!(effect, Effect::None));
    }

    #[test]
    fn a_redacted_answer_is_not_inserted() {
        let (mut w, tab) = ready();
        w.dispatch_assist(AiAction::GenerateSql, None);
        w.dispatch_assist(AiAction::SetPrompt("q".into()), None);
        let effect = w.dispatch_assist(AiAction::SubmitPrompt, None);
        let outcome = AssistOutcome::Sql {
            sql: "select '<redacted>'".into(),
            rationale: String::new(),
            tables: Vec::new(),
            applicable: false,
            inserted: false,
        };
        let Effect::RunAssist(job) = effect else {
            panic!("expected RunAssist");
        };
        let effect = w.assist_finished(job.id, Ok(outcome), Some(EditorBuffer::new("", 0)));
        assert!(matches!(effect, Effect::None));
        assert!(matches!(
            w.assist(tab).unwrap().state,
            AssistState::Done(AssistOutcome::Sql {
                inserted: false,
                ..
            })
        ));
        assert_ne!(REDACTED_SQL.len(), 0);
    }

    #[test]
    fn without_a_buffer_nothing_is_inserted() {
        let (mut w, tab) = ready();
        w.dispatch_assist(AiAction::GenerateSql, None);
        w.dispatch_assist(AiAction::SetPrompt("q".into()), None);
        let Effect::RunAssist(job) = w.dispatch_assist(AiAction::SubmitPrompt, None) else {
            panic!("expected RunAssist");
        };
        let outcome = AssistOutcome::Sql {
            sql: "select 1".into(),
            rationale: String::new(),
            tables: Vec::new(),
            applicable: true,
            inserted: false,
        };
        assert!(matches!(
            w.assist_finished(job.id, Ok(outcome), None),
            Effect::None
        ));
        assert!(matches!(
            w.assist(tab).unwrap().state,
            AssistState::Done(AssistOutcome::Sql {
                inserted: false,
                ..
            })
        ));
    }

    fn effects(effect: Effect) -> Vec<Effect> {
        match effect {
            Effect::Batch(list) => list.into_iter().flat_map(effects).collect(),
            other => vec![other],
        }
    }

    #[test]
    fn without_an_audit_sink_every_ai_surface_is_off_with_one_reason() {
        use crate::ai::NO_AUDIT;
        let (mut w, tab) = ready();
        // A prompt is typed while the sink still exists; it is gone when the user submits.
        w.dispatch_assist(AiAction::GenerateSql, None);
        w.dispatch_assist(AiAction::SetPrompt("q".into()), None);
        w.ai_audit = None;
        let toolbar = w.ai_toolbar(tab);
        assert_eq!(toolbar.generate, Some(NO_AUDIT));
        assert_eq!(toolbar.explain, Some(NO_AUDIT));
        assert_eq!(toolbar.fix, Some(NO_AUDIT));
        let hub = w.hub_model();
        assert_eq!(hub.new_session_blocked, Some(NO_AUDIT));
        assert_eq!(hub.new_agent_blocked, Some(NO_AUDIT));
        let run = effects(w.dispatch_assist(AiAction::SubmitPrompt, None));
        assert!(run.iter().all(|e| !matches!(e, Effect::RunAssist(_))));
        assert_eq!(w.take_notice().as_deref(), Some(NO_AUDIT));
        let open = effects(w.dispatch_ai(AiAction::OpenSession));
        assert!(
            open.iter()
                .all(|e| !matches!(e, Effect::StartAiSession { .. }))
        );
        assert_eq!(w.take_notice().as_deref(), Some(NO_AUDIT));
        assert!(matches!(w.dispatch_ai(AiAction::OpenAgent), Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(NO_AUDIT));
    }

    #[test]
    fn with_an_audit_sink_the_session_and_the_assist_job_carry_it() {
        let (mut w, _tab) = ready();
        let mine = crate::ai::AiAudit::in_memory();
        w.ai_audit = Some(mine.clone());
        w.dispatch_assist(AiAction::GenerateSql, None);
        w.dispatch_assist(AiAction::SetPrompt("q".into()), None);
        let run = effects(w.dispatch_assist(AiAction::SubmitPrompt, None));
        let job = run
            .iter()
            .find_map(|e| match e {
                Effect::RunAssist(job) => Some(job),
                _ => None,
            })
            .expect("a RunAssist");
        assert!(job.audit.same(&mine));
        let open = effects(w.dispatch_ai(AiAction::OpenSession));
        let start = open
            .iter()
            .find_map(|e| match e {
                Effect::StartAiSession { start, .. } => Some(start),
                _ => None,
            })
            .expect("a StartAiSession");
        assert!(start.audit.same(&mine));
    }

    #[test]
    fn cancel_ignores_the_late_result() {
        let (mut w, tab) = ready();
        w.dispatch_assist(AiAction::GenerateSql, None);
        w.dispatch_assist(AiAction::SetPrompt("q".into()), None);
        let Effect::RunAssist(job) = w.dispatch_assist(AiAction::SubmitPrompt, None) else {
            panic!("expected RunAssist");
        };
        let effect = w.dispatch_assist(AiAction::CancelAssist, None);
        assert!(matches!(effect, Effect::CancelAssist(id) if id == job.id));
        assert_eq!(
            w.assist(tab).unwrap().state,
            AssistState::Cancelled(AssistKind::Generate)
        );
        assert_eq!(w.assist_request_tab(job.id), None);
        let late = AssistOutcome::Sql {
            sql: "select 1".into(),
            rationale: String::new(),
            tables: Vec::new(),
            applicable: true,
            inserted: false,
        };
        assert!(matches!(
            w.assist_finished(job.id, Ok(late), Some(EditorBuffer::new("", 0))),
            Effect::None
        ));
        assert_eq!(
            w.assist(tab).unwrap().state,
            AssistState::Cancelled(AssistKind::Generate)
        );
        // The runner reports a cancelled job as an error with this text; it is ignored as well.
        assert!(matches!(
            w.assist_finished(job.id, Err(CANCELLED.into()), None),
            Effect::None
        ));
    }

    #[test]
    fn closing_the_bar_cancels_a_running_generate() {
        let (mut w, tab) = ready();
        w.dispatch_assist(AiAction::GenerateSql, None);
        w.dispatch_assist(AiAction::SetPrompt("q".into()), None);
        let Effect::RunAssist(job) = w.dispatch_assist(AiAction::SubmitPrompt, None) else {
            panic!("expected RunAssist");
        };
        let effect = w.dispatch_assist(AiAction::ClosePrompt, None);
        assert!(matches!(effect, Effect::CancelAssist(id) if id == job.id));
        assert!(w.assist(tab).unwrap().prompt.is_none());
    }

    #[test]
    fn a_failure_is_shown_and_unknown_ids_are_ignored() {
        let (mut w, tab) = ready();
        w.dispatch_assist(AiAction::GenerateSql, None);
        w.dispatch_assist(AiAction::SetPrompt("q".into()), None);
        let Effect::RunAssist(job) = w.dispatch_assist(AiAction::SubmitPrompt, None) else {
            panic!("expected RunAssist");
        };
        assert!(matches!(
            w.assist_finished(job.id + 100, Err("x".into()), None),
            Effect::None
        ));
        assert!(w.assist_running(tab));
        w.assist_finished(job.id, Err("provider error: down".into()), None);
        assert_eq!(
            w.assist(tab).unwrap().state,
            AssistState::Failed {
                kind: AssistKind::Generate,
                message: "provider error: down".into()
            }
        );
    }

    fn blocked(w: &mut Workspace, action: AiAction, reason: &str) {
        let effect = w.dispatch_assist(action, Some(EditorBuffer::new("select 1", 0)));
        never_runs(&effect);
        assert!(!matches!(effect, Effect::RunAssist(_)));
        let notice = w.take_notice().unwrap_or_default();
        assert!(notice.contains(reason), "{notice:?} vs {reason:?}");
    }

    #[test]
    fn the_gate_table() {
        // AI off for the connection.
        let mut w = workspace(false, config(), vec![ProviderKind::Ollama]);
        let tab = open(&mut w);
        blocked(&mut w, AiAction::ExplainQuery, AI_OFF);
        assert_eq!(w.ai_toolbar(tab).generate, Some(AI_OFF));
        assert_eq!(w.ai_toolbar(tab).explain, Some(AI_OFF));
        assert_eq!(w.ai_toolbar(tab).fix, Some(AI_OFF));

        // ai.toml broken.
        let broken = AiState::from_startup(&crate::ai::AiStartup {
            config: Err("bad".into()),
            dir: None,
            factory: Arc::new(crate::ai::NoProviders),
        });
        let mut w = workspace(true, config(), vec![ProviderKind::Ollama]).with_ai(broken);
        let tab = open(&mut w);
        blocked(&mut w, AiAction::ExplainQuery, AI_CONFIG_BROKEN);
        assert_eq!(w.ai_toolbar(tab).explain, Some(AI_CONFIG_BROKEN));

        // No model: the AI view is shown.
        let mut w = workspace(true, AiConfig::default(), vec![ProviderKind::Ollama]);
        let tab = open(&mut w);
        let effect = w.dispatch_assist(
            AiAction::ExplainQuery,
            Some(EditorBuffer::new("select 1", 0)),
        );
        assert!(matches!(effect, Effect::None));
        assert_eq!(
            w.take_notice().as_deref(),
            Some(super::super::ai::NO_PROVIDER)
        );
        assert_eq!(w.ai_toolbar(tab).generate, Some(NO_MODEL));

        // The kind is not built in.
        let mut w = workspace(true, config(), Vec::new());
        let tab = open(&mut w);
        blocked(&mut w, AiAction::ExplainQuery, NOT_BUILT);
        assert_eq!(w.ai_toolbar(tab).generate, Some(NOT_BUILT));

        // Busy, and no failure to fix.
        let (mut w, tab) = ready();
        assert_eq!(w.ai_toolbar(tab).fix, Some(NO_FAILURE));
        assert_eq!(w.ai_toolbar(tab).explain, None);
        blocked(&mut w, AiAction::FixQuery, NO_FAILURE);
        blocked(&mut w, AiAction::FixStatement(0), NO_FAILURE);
        let effect = w.dispatch_assist(
            AiAction::ExplainQuery,
            Some(EditorBuffer::new("select 1", 0)),
        );
        assert!(matches!(effect, Effect::RunAssist(_)));
        assert_eq!(w.ai_toolbar(tab).generate, Some(BUSY));
        blocked(&mut w, AiAction::ExplainQuery, BUSY);

        // No tab at all.
        let mut w = workspace(true, config(), vec![ProviderKind::Ollama]);
        blocked(&mut w, AiAction::GenerateSql, NO_EDITOR);
    }

    #[test]
    fn a_closed_tab_drops_the_result() {
        let (mut w, tab) = ready();
        let effect = w.dispatch_assist(
            AiAction::ExplainQuery,
            Some(EditorBuffer::new("select 1", 0)),
        );
        let Effect::RunAssist(job) = effect else {
            panic!("expected RunAssist");
        };
        w.dispatch(WorkspaceAction::CloseTab(tab));
        let outcome = AssistOutcome::Explanation {
            sql: "select 1".into(),
            text: "t".into(),
        };
        assert!(matches!(
            w.assist_finished(job.id, Ok(outcome), None),
            Effect::None
        ));
    }

    #[test]
    fn the_prompt_does_not_leak_into_debug_output() {
        use std::fmt::Write as _;
        let (mut w, _tab) = ready();
        w.dispatch_assist(AiAction::GenerateSql, None);
        let action = AiAction::SetPrompt("CANARY-PROMPT".into());
        let mut text = format!("{action:?}");
        w.dispatch_assist(action, None);
        let effect = w.dispatch_assist(AiAction::SubmitPrompt, None);
        let _ = write!(text, "{effect:?}");
        if let Effect::RunAssist(job) = &effect {
            let _ = write!(text, "{job:?}");
        }
        assert!(matches!(effect, Effect::RunAssist(_)));
        assert!(!text.contains("CANARY"), "{text}");
    }
}
