// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI settings inside the workspace (ADR-0050).
//!
//! Like history and export this is state only. `dispatch_ai` returns an [`Effect`] when
//! `ai.toml`, the keychain or a provider must be touched; the view does that on the blocking pool
//! or tokio and reports back through [`Workspace::ai_saved`] and [`Workspace::ai_test_finished`].
//! Nothing here reads the keychain or calls a provider, and nothing runs at startup.

use sqler_config::{AiConfig, ConfigDir, ConnectionId, ConnectionProfile};

use super::state::{Effect, SecretOp, Workspace};
use crate::ai::state::{AiSave, TestRequest};
use crate::ai::{AiAction, AiState, HubModel, hub_model};
use crate::workbench::{SideView, WorkbenchAction};

/// What a command that needs a provider says when none is set up.
pub const NO_PROVIDER: &str = "No AI provider is set up yet. Add one in Settings > AI; Ollama runs on this computer and is the default.";

/// Why `ai.toml` cannot be changed.
const READ_ONLY: &str = "ai.toml could not be read; fix or remove it to change AI settings.";

/// What to tell the user when there is no connection to act on.
const NO_CONNECTION: &str = "Open or select a connection first.";

/// One `ai.toml` write and the keychain changes that follow it.
#[derive(Debug)]
pub(crate) struct PendingSave {
    pub dir: Option<ConfigDir>,
    pub config: AiConfig,
    pub secrets: Vec<SecretOp>,
}

/// Serialises the saves of `ai.toml`: one is in flight, later ones wait and only the latest
/// configuration is written. The keychain changes of a skipped save are kept and run after the
/// file write of the save that replaced it, in order.
#[derive(Debug, Default)]
pub(crate) struct SaveQueue {
    busy: bool,
    pending: Option<PendingSave>,
}

impl SaveQueue {
    /// Offers a save. Returns it when nothing is in flight (run it now); otherwise it waits.
    #[must_use]
    pub(crate) fn submit(&mut self, save: PendingSave) -> Option<PendingSave> {
        if !self.busy {
            self.busy = true;
            return Some(save);
        }
        self.pending = Some(match self.pending.take() {
            Some(mut older) => {
                older.secrets.extend(save.secrets);
                PendingSave {
                    secrets: older.secrets,
                    ..save
                }
            }
            None => save,
        });
        None
    }

    /// The running save is done (file and keychain). Returns the waiting save to run next, else
    /// the queue goes idle.
    #[must_use]
    pub(crate) fn finish(&mut self) -> Option<PendingSave> {
        let next = self.pending.take();
        self.busy = next.is_some();
        next
    }
}

impl Workspace {
    /// Replaces the AI settings (the binary loads them at startup).
    #[must_use]
    pub fn with_ai(mut self, ai: AiState) -> Self {
        self.ai = ai;
        self
    }

    /// The AI settings.
    #[must_use]
    pub fn ai(&self) -> &AiState {
        &self.ai
    }

    /// The connection AI commands act on: the active tab's, else the selected one.
    #[must_use]
    pub fn ai_target(&self) -> Option<ConnectionId> {
        self.tabs
            .active()
            .and_then(|tab| self.tab_conns.get(&tab.id()))
            .or_else(|| self.sidebar.selected_id())
            .filter(|id| self.sidebar.profile(id).is_some())
            .cloned()
    }

    fn ai_target_profile(&self) -> Option<&ConnectionProfile> {
        self.sidebar.profile(&self.ai_target()?)
    }

    /// What the AI side view shows.
    #[must_use]
    pub fn hub_model(&self) -> HubModel {
        self.with_sessions(hub_model(&self.ai, self.ai_target_profile()))
    }

    fn ai_notice(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    /// Turns the outcome of a change into the effect that persists it.
    fn ai_save_effect(&self, save: AiSave) -> Effect {
        if save.secrets.is_empty() && !self.ai.persistent() {
            return Effect::None;
        }
        Effect::SaveAiConfig {
            dir: self.ai.dir().cloned(),
            config: save.config,
            secrets: save.secrets,
        }
    }

    fn ai_test_effect(request: TestRequest) -> Effect {
        Effect::TestAiProvider {
            request: request.request,
            provider: request.provider,
            key: request.key,
        }
    }

    /// Runs an AI action. The returned effect is carried out by the view.
    #[allow(clippy::too_many_lines)] // one arm per action
    pub fn dispatch_ai(&mut self, action: AiAction) -> Effect {
        if action.is_session() {
            return self.dispatch_session(action);
        }
        if action.is_assist() {
            return self.dispatch_assist(action, None);
        }
        let changes_config = !matches!(
            action,
            AiAction::OpenSettings
                | AiAction::CancelProvider
                | AiAction::TestProvider
                | AiAction::ToggleConnection
                | AiAction::OpenAuditLog
                | AiAction::LoadMoreAudit
                | AiAction::CopyAuditSql
                | AiAction::SetAuditFilter(_)
                | AiAction::SelectAuditEntry(_)
        );
        if changes_config && self.ai.load_error().is_some() {
            return self.ai_notice(READ_ONLY);
        }
        match action {
            AiAction::OpenSettings => self.dispatch(super::WorkspaceAction::OpenSettings),
            AiAction::AddProvider(kind) => {
                if !self.ai.open_new(kind) {
                    return self.ai_notice(format!(
                        "{} is not available in this build.",
                        crate::ai::kind_label(kind)
                    ));
                }
                self.dispatch(super::WorkspaceAction::OpenSettings)
            }
            AiAction::EditProvider(id) => {
                if self.ai.open_edit(&id) {
                    return self.dispatch(super::WorkspaceAction::OpenSettings);
                }
                Effect::None
            }
            AiAction::RemoveProvider(id) => match self.ai.remove_provider(&id) {
                Some(save) => self.ai_save_effect(save),
                None => Effect::None,
            },
            AiAction::SetText { field, text } => {
                self.ai.set_text(field, text);
                Effect::None
            }
            AiAction::SetKind(kind) => {
                self.ai.set_kind(kind);
                Effect::None
            }
            AiAction::ClearKey => {
                self.ai.clear_key();
                Effect::None
            }
            AiAction::SaveProvider => match self.ai.save_provider() {
                Some(save) => self.ai_save_effect(save),
                None => Effect::None,
            },
            AiAction::CancelProvider => {
                self.ai.close_draft();
                Effect::None
            }
            AiAction::TestProvider => self.test_provider(),
            AiAction::UseListedModels => {
                self.ai.use_listed_models();
                Effect::None
            }
            AiAction::SetModel { use_case, choice } => match self.ai.set_model(use_case, choice) {
                Ok(save) => self.ai_save_effect(save),
                Err(message) => self.ai_notice(message),
            },
            AiAction::SetUnlimited(on) => {
                self.ai.set_unlimited(on);
                Effect::None
            }
            AiAction::SaveLimits => match self.ai.save_limits() {
                Some(save) => self.ai_save_effect(save),
                None => Effect::None,
            },
            AiAction::ToggleConnection => self.toggle_connection_ai(),
            audit @ (AiAction::OpenAuditLog
            | AiAction::LoadMoreAudit
            | AiAction::CopyAuditSql
            | AiAction::SetAuditFilter(_)
            | AiAction::SelectAuditEntry(_)) => self.dispatch_audit(audit),
            // Handled at the top; named here so a new action is a compile error.
            session @ (AiAction::OpenSession
            | AiAction::SetSessionDraft(..)
            | AiAction::SendMessage(_)
            | AiAction::CancelSession(_)
            | AiAction::ApproveStatement(_)
            | AiAction::DenyStatement(_)
            | AiAction::RerunSql(..)
            | AiAction::OpenSqlInEditor(..)) => self.dispatch_session(session),
            // Routed above; kept so a new assist action cannot fall through unhandled.
            assist => self.dispatch_assist(assist, None),
        }
    }

    /// Tests the editor's provider with the typed key; without an editor, the provider of the
    /// connection the user works on.
    fn test_provider(&mut self) -> Effect {
        if self.ai.draft().is_some() {
            return match self.ai.begin_draft_test() {
                Some(request) => Self::ai_test_effect(request),
                None => Effect::None,
            };
        }
        let provider = self
            .ai
            .config()
            .models
            .default
            .as_ref()
            .or_else(|| crate::ai::main_choice(self.ai.config()))
            .map(|c| c.provider.clone());
        let request = provider.and_then(|id| self.ai.begin_provider_test(&id));
        match request {
            Some(request) => Self::ai_test_effect(request),
            None => self.no_provider(),
        }
    }

    /// Explains how to set a provider up and shows the AI view.
    pub(super) fn no_provider(&mut self) -> Effect {
        self.layout
            .apply(WorkbenchAction::ShowSideView(SideView::Ai));
        self.ai_notice(NO_PROVIDER)
    }

    /// Flips the AI opt-in of the connection the user works on.
    fn toggle_connection_ai(&mut self) -> Effect {
        let Some(id) = self.ai_target() else {
            return self.ai_notice(NO_CONNECTION);
        };
        let Some(profile) = self.sidebar.profile(&id) else {
            return self.ai_notice(NO_CONNECTION);
        };
        let mut spec = profile.spec().clone();
        let mut ai = spec.ai;
        ai.enabled = !ai.enabled;
        if !ai.enabled {
            // Consent must not outlive AI being off: a re-enable starts from the environment default.
            ai.send_rows = None;
        }
        spec = spec.with_ai(ai);
        let turned_on = ai.enabled;
        if let Err(err) = self.sidebar.update_connection(&id, spec) {
            return self.ai_notice(err.to_string());
        }
        if turned_on && crate::ai::main_choice(self.ai.config()).is_none() {
            self.notice = Some(NO_PROVIDER.to_owned());
        }
        let stopped = if turned_on {
            Effect::None
        } else {
            // Turning AI off stops the running turns of this connection's sessions.
            self.stop_ai_sessions_of(&id)
        };
        let save = Effect::SaveConnections {
            secrets: Vec::new(),
        };
        match stopped {
            Effect::None => save,
            stopped => Effect::Batch(vec![stopped, save]),
        }
    }

    /// `ai.toml` and the keychain were written (or not): report the file's result. A failure
    /// keeps the change for this session.
    pub fn ai_saved(&mut self, result: Result<(), String>) {
        if let Err(message) = result {
            self.notice = Some(format!(
                "Could not save ai.toml (the change lasts for this session): {message}"
            ));
        }
    }

    /// A provider test ended. A result of an older test is ignored.
    pub fn ai_test_finished(&mut self, request: u64, result: Result<Vec<String>, String>) {
        let Some(report) = self.ai.finish_test(request, result) else {
            return;
        };
        // The editor shows its own result; a test from the AI view has no place for one.
        if let Some(id) = report.provider {
            self.notice = Some(match report.result {
                Ok(1) => format!("{id}: connected, 1 model."),
                Ok(n) => format!("{id}: connected, {n} models."),
                Err(message) => format!("{id}: test failed: {message}"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AiField, AiTestStatus};
    use crate::sidebar::SidebarModel;
    use crate::workspace::WorkspaceAction;
    use sqler_config::{
        AiConfig, ConnectionSpec, ModelChoice, ProviderConfig, ProviderId, ProviderKind as K,
    };
    use sqler_driver_api::sqler_core::DriverId;

    fn id(s: &str) -> ProviderId {
        ProviderId::new(s).unwrap()
    }

    fn ws_with(ai: AiState) -> Workspace {
        let mut sidebar = SidebarModel::default();
        sidebar
            .add_connection(
                ConnectionSpec::new("dev", DriverId::new("sqlite").unwrap(), "x"),
                None,
            )
            .unwrap();
        Workspace::new(sidebar).with_ai(ai)
    }

    fn ws() -> Workspace {
        ws_with(AiState::for_config(AiConfig::default()))
    }

    fn persistent_ws() -> (Workspace, std::path::PathBuf) {
        // Tests run in parallel: each gets its own directory.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sqler-ws-ai-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ai = AiState::new(
            AiConfig::default(),
            crate::ai::ALL_KINDS.to_vec(),
            Some(sqler_config::ConfigDir::new(&dir)),
        );
        (ws_with(ai), dir)
    }

    fn text(w: &mut Workspace, field: AiField, text: &str) {
        w.dispatch_ai(AiAction::SetText {
            field,
            text: text.into(),
        });
    }

    #[test]
    fn saving_a_provider_with_a_key_sets_the_secret_and_masks_it_afterwards() {
        let (mut w, dir) = persistent_ws();
        w.dispatch_ai(AiAction::AddProvider(K::Anthropic));
        assert!(
            w.settings_tab_active(),
            "the editor lives on the settings page"
        );
        text(&mut w, AiField::ProviderId, "claude");
        text(&mut w, AiField::Models, "c1");
        text(&mut w, AiField::Key, "sk-test-123");
        let effect = w.dispatch_ai(AiAction::SaveProvider);
        let Effect::SaveAiConfig {
            dir: Some(_),
            config,
            secrets,
        } = &effect
        else {
            panic!("expected SaveAiConfig, got {effect:?}");
        };
        assert_eq!(config.providers.len(), 1);
        assert!(matches!(
            secrets.as_slice(),
            [SecretOp::Set(r, s)] if r.to_string() == "claude:ai-provider-key" && s.expose() == "sk-test-123"
        ));
        assert!(!format!("{effect:?}").contains("sk-test-123"));
        let d = w.ai().draft().unwrap();
        assert!(d.key_kept() && !d.key_typed());
        assert!(!format!("{:?} {d:?}", w.ai()).contains("sk-test-123"));
        assert!(w.ai().config().provider(&id("claude")).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_session_only_save_still_runs_the_keychain_ops() {
        let mut w = ws();
        w.dispatch_ai(AiAction::AddProvider(K::OpenAi));
        text(&mut w, AiField::Key, "k");
        let Effect::SaveAiConfig { dir, secrets, .. } = w.dispatch_ai(AiAction::SaveProvider)
        else {
            panic!("expected SaveAiConfig");
        };
        assert!(dir.is_none());
        assert_eq!(secrets.len(), 1);
        // No key, no directory: nothing to do.
        w.dispatch_ai(AiAction::CancelProvider);
        w.dispatch_ai(AiAction::AddProvider(K::Ollama));
        assert!(matches!(
            w.dispatch_ai(AiAction::SaveProvider),
            Effect::None
        ));
        assert_eq!(w.ai().config().providers.len(), 2);
    }

    #[test]
    fn an_invalid_provider_changes_nothing() {
        let mut w = ws();
        w.dispatch_ai(AiAction::AddProvider(K::OpenAi));
        assert!(matches!(
            w.dispatch_ai(AiAction::SaveProvider),
            Effect::None
        ));
        assert_eq!(w.ai().config().providers.len(), 0);
        assert!(w.ai().draft().unwrap().error().is_some());
    }

    #[test]
    fn a_broken_ai_toml_is_read_only() {
        let startup = crate::ai::AiStartup {
            config: Err("line 1".into()),
            dir: None,
            factory: std::sync::Arc::new(crate::ai::NoProviders),
        };
        let mut w = ws_with(AiState::from_startup(&startup));
        for action in [
            AiAction::AddProvider(K::Ollama),
            AiAction::RemoveProvider(id("a")),
            AiAction::SaveProvider,
            AiAction::SaveLimits,
            AiAction::SetUnlimited(true),
        ] {
            assert!(matches!(w.dispatch_ai(action), Effect::None));
            assert_eq!(
                w.take_notice().as_deref(),
                Some("ai.toml could not be read; fix or remove it to change AI settings.")
            );
        }
        assert!(w.ai().draft().is_none());
        assert!(!w.settings_tab_open());
    }

    #[test]
    fn testing_without_a_provider_explains_the_setup() {
        let mut w = ws();
        assert!(!w.layout().side_visible() || w.layout().side_view() != SideView::Ai);
        assert!(matches!(
            w.dispatch_ai(AiAction::TestProvider),
            Effect::None
        ));
        assert_eq!(w.take_notice().as_deref(), Some(NO_PROVIDER));
        assert_eq!(w.layout().side_view(), SideView::Ai);
        assert!(w.layout().side_visible());
    }

    #[test]
    fn testing_the_connections_provider() {
        let mut config = AiConfig::default();
        config
            .providers
            .push(ProviderConfig::new(id("local"), K::Ollama).with_models(["m"]));
        config.models.default = Some(ModelChoice::new(id("local"), "m"));
        let mut w = ws_with(AiState::for_config(config));
        let Effect::TestAiProvider {
            request,
            provider,
            key,
        } = w.dispatch_ai(AiAction::TestProvider)
        else {
            panic!("expected TestAiProvider");
        };
        assert_eq!(provider.id.as_str(), "local");
        assert!(key.is_none());
        w.ai_test_finished(request, Ok(vec!["m".into(), "n".into()]));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("local: connected, 2 models.")
        );
        let Effect::TestAiProvider { request, .. } = w.dispatch_ai(AiAction::TestProvider) else {
            panic!("expected TestAiProvider");
        };
        w.ai_test_finished(request, Err("down".into()));
        assert_eq!(w.take_notice().as_deref(), Some("local: test failed: down"));
    }

    #[test]
    fn testing_the_editor_passes_the_typed_key_and_ignores_stale_results() {
        let mut w = ws();
        w.dispatch_ai(AiAction::AddProvider(K::OpenAi));
        text(&mut w, AiField::Key, "sk-test-123");
        let effect = w.dispatch_ai(AiAction::TestProvider);
        let Effect::TestAiProvider { request, key, .. } = &effect else {
            panic!("expected TestAiProvider");
        };
        assert_eq!(
            key.as_ref().map(sqler_config::Secret::expose),
            Some("sk-test-123")
        );
        assert!(!format!("{effect:?}").contains("sk-test-123"));
        let request = *request;
        assert_eq!(*w.ai().test(), AiTestStatus::Running(request));
        w.ai_test_finished(request + 1, Ok(vec!["x".into()]));
        assert_eq!(*w.ai().test(), AiTestStatus::Running(request));
        w.ai_test_finished(request, Ok(vec!["x".into()]));
        assert_eq!(*w.ai().test(), AiTestStatus::Ok(vec!["x".into()]));
        assert_eq!(w.take_notice(), None, "the editor shows its own result");
    }

    #[test]
    fn toggling_saves_the_connections() {
        let mut w = ws();
        w.dispatch(WorkspaceAction::SelectConnection(0));
        let conn = w.ai_target().unwrap();
        assert!(!w.sidebar().profile(&conn).unwrap().spec().ai.enabled);
        let effect = w.dispatch_ai(AiAction::ToggleConnection);
        assert!(matches!(&effect, Effect::SaveConnections { secrets } if secrets.is_empty()));
        assert!(w.sidebar().profile(&conn).unwrap().spec().ai.enabled);
        assert_eq!(w.take_notice().as_deref(), Some(NO_PROVIDER));
        w.dispatch_ai(AiAction::ToggleConnection);
        assert!(!w.sidebar().profile(&conn).unwrap().spec().ai.enabled);
        assert_eq!(w.take_notice(), None);
    }

    #[test]
    fn toggling_off_then_on_forgets_the_row_consent() {
        use sqler_driver_api::sqler_core::Environment;
        for env in [Environment::Prod, Environment::Dev] {
            let mut sidebar = SidebarModel::default();
            let id = sidebar
                .add_connection(
                    ConnectionSpec::new("c", DriverId::new("sqlite").unwrap(), "x")
                        .with_environment(env)
                        .with_ai(
                            sqler_config::ConnectionAi::new(true)
                                .with_send_rows(env != Environment::Dev),
                        ),
                    None,
                )
                .unwrap();
            let mut w = Workspace::new(sidebar).with_ai(AiState::for_config(AiConfig::default()));
            w.dispatch(WorkspaceAction::SelectConnection(0));
            w.dispatch_ai(AiAction::ToggleConnection);
            let ai = w.sidebar().profile(&id).unwrap().spec().ai;
            assert!(!ai.enabled && ai.send_rows.is_none());
            w.dispatch_ai(AiAction::ToggleConnection);
            let ai = w.sidebar().profile(&id).unwrap().spec().ai;
            assert!(ai.enabled && ai.send_rows.is_none());
            assert_eq!(ai.sends_rows(Some(env)), env != Environment::Prod);
            // And the file written for it holds no leftover choice.
            w.dispatch_ai(AiAction::ToggleConnection);
            let dir = std::env::temp_dir()
                .join(format!("sqler-toggle-ai-{}-{env:?}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            sqler_config::ConfigDir::new(&dir)
                .save_connection_store(w.sidebar().store())
                .unwrap();
            let text = std::fs::read_to_string(dir.join("connections.toml")).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            assert!(!text.contains("send_rows"), "{text}");
        }
    }

    #[test]
    fn toggling_with_a_provider_has_no_notice() {
        let mut config = AiConfig::default();
        config
            .providers
            .push(ProviderConfig::new(id("local"), K::Ollama).with_models(["m"]));
        config.models.default = Some(ModelChoice::new(id("local"), "m"));
        let mut w = ws_with(AiState::for_config(config));
        w.dispatch(WorkspaceAction::SelectConnection(0));
        w.dispatch_ai(AiAction::ToggleConnection);
        assert_eq!(w.take_notice(), None);
    }

    #[test]
    fn toggling_without_a_connection_says_so() {
        let mut w = Workspace::new(SidebarModel::default());
        assert!(matches!(
            w.dispatch_ai(AiAction::ToggleConnection),
            Effect::None
        ));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Open or select a connection first.")
        );
    }

    #[test]
    fn open_settings_reuses_the_workspace_action() {
        let mut w = ws();
        w.dispatch_ai(AiAction::OpenSettings);
        assert!(w.settings_tab_active());
    }

    #[test]
    fn model_choices_and_limits_save_the_file() {
        let (mut w, dir) = persistent_ws();
        w.dispatch_ai(AiAction::AddProvider(K::Ollama));
        text(&mut w, AiField::Models, "m1");
        w.dispatch_ai(AiAction::SaveProvider);
        let effect = w.dispatch_ai(AiAction::SetModel {
            use_case: None,
            choice: Some(ModelChoice::new(id("ollama"), "m1")),
        });
        assert!(matches!(effect, Effect::SaveAiConfig { .. }));
        let bad = w.dispatch_ai(AiAction::SetModel {
            use_case: None,
            choice: Some(ModelChoice::new(id("ollama"), "nope")),
        });
        assert!(matches!(bad, Effect::None));
        assert!(w.take_notice().is_some());
        text(&mut w, AiField::MaxRows, "0");
        assert!(matches!(w.dispatch_ai(AiAction::SaveLimits), Effect::None));
        assert!(w.ai().limits().error().is_some());
        text(&mut w, AiField::MaxRows, "10");
        assert!(matches!(
            w.dispatch_ai(AiAction::SaveLimits),
            Effect::SaveAiConfig { .. }
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn save(max_rows: u32, secrets: usize) -> PendingSave {
        let mut config = AiConfig::default();
        config.limits.max_rows = max_rows;
        PendingSave {
            dir: None,
            config,
            secrets: (0..secrets)
                .map(|i| {
                    SecretOp::Delete(
                        sqler_config::SecretRef::new(
                            &format!("k{i}"),
                            sqler_config::SecretKind::ProviderApiKey,
                        )
                        .expect("a valid ref"),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn two_quick_saves_write_one_after_the_other_and_the_latest_wins() {
        let mut q = SaveQueue::default();
        let first = q.submit(save(1, 1)).expect("idle: runs now");
        assert_eq!(first.config.limits.max_rows, 1);
        assert!(q.submit(save(2, 1)).is_none());
        assert!(q.submit(save(3, 1)).is_none());
        let next = q.finish().expect("the latest waits");
        assert_eq!(next.config.limits.max_rows, 3);
        assert_eq!(
            next.secrets.len(),
            2,
            "the skipped save keeps its keychain ops"
        );
        assert!(q.finish().is_none());
        assert!(q.submit(save(4, 0)).is_some(), "idle again");
    }

    #[test]
    fn a_failed_save_is_a_notice() {
        let mut w = ws();
        w.ai_saved(Ok(()));
        assert_eq!(w.take_notice(), None);
        w.ai_saved(Err("disk full".into()));
        assert!(w.take_notice().unwrap().contains("disk full"));
    }

    #[test]
    fn the_target_is_the_selected_connection_without_a_tab() {
        let mut w = ws();
        w.sidebar.clear_selection();
        assert_eq!(w.ai_target(), None);
        w.dispatch(WorkspaceAction::SelectConnection(0));
        assert!(w.ai_target().is_some());
        let m = w.hub_model();
        assert_eq!(m.connection.unwrap().name, "dev");
    }
}
