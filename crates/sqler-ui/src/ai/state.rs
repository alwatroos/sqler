// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `AiState`: the loaded `ai.toml`, the providers compiled in and the drafts of the AI settings.
//! It decides; `Workspace` turns the outcomes into effects and notices.

use sqler_config::{
    AiConfig, ConfigDir, ModelChoice, ProviderConfig, ProviderId, ProviderKind, Secret, UseCase,
};

use super::provider::AiStartup;
use super::settings::{AiField, AiTestStatus, DraftError, KeyPlan, LimitsDraft, ProviderDraft};
use crate::workspace::SecretOp;

/// A change to write: the whole configuration and the keychain operations that go with it.
#[derive(Debug)]
pub(crate) struct AiSave {
    /// The configuration to write to `ai.toml`.
    pub config: AiConfig,
    /// Keychain changes to make after the file is written.
    pub secrets: Vec<SecretOp>,
}

/// A "Test connection" to start.
pub(crate) struct TestRequest {
    /// Tells this run from an older one.
    pub request: u64,
    /// The provider to test.
    pub provider: ProviderConfig,
    /// A typed key that is not stored yet.
    pub key: Option<Secret>,
}

/// How a finished test ended, for the notice of a test that is not shown in the editor.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TestReport {
    /// The provider that was tested; `None` for the editor's draft (it shows its own result).
    pub provider: Option<ProviderId>,
    /// The number of models, or why it failed.
    pub result: Result<usize, String>,
}

/// The AI settings of a workspace.
#[derive(Clone)]
pub struct AiState {
    config: AiConfig,
    load_error: Option<String>,
    dir: Option<ConfigDir>,
    kinds: Vec<ProviderKind>,
    draft: Option<ProviderDraft>,
    limits: LimitsDraft,
    test: AiTestStatus,
    test_provider: Option<ProviderId>,
    next_test: u64,
    inputs_generation: u64,
}

impl Default for AiState {
    /// No provider, nothing compiled in, nowhere to save.
    fn default() -> Self {
        Self::new(AiConfig::default(), Vec::new(), None)
    }
}

// Ids, counts and flags only: no endpoint, no key, no model text.
impl std::fmt::Debug for AiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiState")
            .field("providers", &self.config.providers.len())
            .field("load_error", &self.load_error.is_some())
            .field("persistent", &self.dir.is_some())
            .field("kinds", &self.kinds)
            .field("draft", &self.draft)
            .field("test", &self.test)
            .finish_non_exhaustive()
    }
}

impl AiState {
    /// A state for `config` with these kinds compiled in, saved to `dir`.
    #[must_use]
    pub fn new(config: AiConfig, kinds: Vec<ProviderKind>, dir: Option<ConfigDir>) -> Self {
        let limits = LimitsDraft::from_config(&config);
        Self {
            config,
            load_error: None,
            dir,
            kinds,
            draft: None,
            limits,
            test: AiTestStatus::Idle,
            test_provider: None,
            next_test: 0,
            inputs_generation: 0,
        }
    }

    /// A state for `config` with every provider kind available and nowhere to save.
    #[must_use]
    pub fn for_config(config: AiConfig) -> Self {
        Self::new(config, super::ALL_KINDS.to_vec(), None)
    }

    /// The state the binary's startup data describes. A configuration that could not be read
    /// leaves the default in place, read-only, with the reason.
    #[must_use]
    pub fn from_startup(startup: &AiStartup) -> Self {
        let kinds = startup.factory.kinds();
        match &startup.config {
            Ok(config) => Self::new(config.clone(), kinds, startup.dir.clone()),
            Err(message) => {
                let mut state = Self::new(AiConfig::default(), kinds, None);
                state.load_error = Some(message.clone());
                state
            }
        }
    }

    /// The saved configuration.
    #[must_use]
    pub fn config(&self) -> &AiConfig {
        &self.config
    }

    /// Why `ai.toml` could not be read; every change is refused then.
    #[must_use]
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Whether changes are written to `ai.toml` (else they last for the session).
    #[must_use]
    pub fn persistent(&self) -> bool {
        self.dir.is_some()
    }

    /// The provider kinds compiled into this build, in enum order.
    #[must_use]
    pub fn kinds(&self) -> &[ProviderKind] {
        &self.kinds
    }

    /// The provider editor, while it is open.
    #[must_use]
    pub fn draft(&self) -> Option<&ProviderDraft> {
        self.draft.as_ref()
    }

    /// The redaction and limits form.
    #[must_use]
    pub fn limits(&self) -> &LimitsDraft {
        &self.limits
    }

    /// The state of the last test.
    #[must_use]
    pub fn test(&self) -> &AiTestStatus {
        &self.test
    }

    /// Changes whenever the editor opens or closes or a draft is reset, so the view knows to
    /// rebuild its text inputs.
    #[must_use]
    pub fn inputs_generation(&self) -> u64 {
        self.inputs_generation
    }

    /// Where `ai.toml` is saved.
    pub(crate) fn dir(&self) -> Option<&ConfigDir> {
        self.dir.as_ref()
    }

    fn bump(&mut self) {
        self.inputs_generation += 1;
    }

    fn forget_draft_test(&mut self) {
        if self.test_provider.is_none() {
            self.test = AiTestStatus::Idle;
        }
    }

    /// Opens the editor for a new provider; `false` when the kind is not compiled in.
    pub(crate) fn open_new(&mut self, kind: ProviderKind) -> bool {
        if !self.kinds.contains(&kind) {
            return false;
        }
        self.draft = Some(ProviderDraft::new_provider(kind, &self.config));
        self.test = AiTestStatus::Idle;
        self.test_provider = None;
        self.bump();
        true
    }

    /// Opens the editor for a saved provider; `false` when there is none with this id.
    pub(crate) fn open_edit(&mut self, id: &ProviderId) -> bool {
        let Some(provider) = self.config.provider(id) else {
            return false;
        };
        self.draft = Some(ProviderDraft::edit(provider));
        self.test = AiTestStatus::Idle;
        self.test_provider = None;
        self.bump();
        true
    }

    /// Closes the editor without saving.
    pub(crate) fn close_draft(&mut self) {
        if self.draft.take().is_some() {
            self.forget_draft_test();
            self.bump();
        }
    }

    /// Routes a typed text to the editor or the limits form.
    pub(crate) fn set_text(&mut self, field: AiField, text: String) {
        match field {
            AiField::ProviderId | AiField::Endpoint | AiField::Models | AiField::Key => {
                if let Some(draft) = &mut self.draft {
                    draft.set_text(field, text);
                }
            }
            AiField::Redaction | AiField::MaxRows | AiField::TimeoutSecs => {
                self.limits.set_text(field, text);
            }
        }
    }

    /// Changes the kind of the new provider in the editor.
    pub(crate) fn set_kind(&mut self, kind: ProviderKind) {
        if !self.kinds.contains(&kind) {
            return;
        }
        if let Some(draft) = &mut self.draft {
            let before = draft.id().to_owned();
            draft.set_kind(kind, &self.config);
            if draft.id() != before {
                self.bump();
            }
        }
    }

    /// Marks the stored key of the editor's provider for removal on save.
    pub(crate) fn clear_key(&mut self) {
        if let Some(draft) = &mut self.draft {
            draft.clear_key();
            self.bump();
        }
    }

    /// Validates the editor and applies it. `None` when it is invalid (the editor shows why) or
    /// closed.
    pub(crate) fn save_provider(&mut self) -> Option<AiSave> {
        let draft = self.draft.as_mut()?;
        let candidate = match draft.candidate(&self.config) {
            Ok(c) => c,
            Err(e) => {
                draft.set_error(e);
                return None;
            }
        };
        let key_ref = candidate.provider.id.key_ref();
        let secrets = match candidate.key {
            KeyPlan::Set(secret) => vec![SecretOp::Set(key_ref.secret_ref().clone(), secret)],
            KeyPlan::Clear => vec![SecretOp::Delete(key_ref.secret_ref().clone())],
            KeyPlan::Keep => Vec::new(),
        };
        draft.after_save(candidate.provider.api_key_in_keychain);
        self.config = candidate.config;
        self.bump();
        Some(AiSave {
            config: self.config.clone(),
            secrets,
        })
    }

    /// Removes a provider, the model choices that name it and its stored key.
    pub(crate) fn remove_provider(&mut self, id: &ProviderId) -> Option<AiSave> {
        let provider = self.config.provider(id)?;
        let mut secrets = Vec::new();
        if let Some(key_ref) = provider.key_ref() {
            secrets.push(SecretOp::Delete(key_ref.secret_ref().clone()));
        }
        self.config.providers.retain(|p| &p.id != id);
        let models = &mut self.config.models;
        for slot in super::models::slots_mut(models) {
            if slot.as_ref().is_some_and(|c| &c.provider == id) {
                *slot = None;
            }
        }
        if self.draft.as_ref().is_some_and(|d| d.id() == id.as_str()) {
            self.close_draft();
        }
        if self.test_provider.as_ref() == Some(id) {
            self.test = AiTestStatus::Idle;
            self.test_provider = None;
        }
        Some(AiSave {
            config: self.config.clone(),
            secrets,
        })
    }

    /// Chooses the model of a use case (`None` use case = the default); `None` choice clears it.
    ///
    /// # Errors
    /// A message when the provider or model is not configured.
    pub(crate) fn set_model(
        &mut self,
        use_case: Option<UseCase>,
        choice: Option<ModelChoice>,
    ) -> Result<AiSave, String> {
        let mut next = self.config.clone();
        *super::models::slot_mut(&mut next.models, use_case) = choice;
        next.validate()
            .map_err(|_| "That provider does not list that model.".to_owned())?;
        self.config = next;
        Ok(AiSave {
            config: self.config.clone(),
            secrets: Vec::new(),
        })
    }

    /// Lifts or restores the caps in the form.
    pub(crate) fn set_unlimited(&mut self, unlimited: bool) {
        self.limits.set_unlimited(unlimited);
    }

    /// Validates the redaction and limits form and applies it. `None` when it is invalid.
    pub(crate) fn save_limits(&mut self) -> Option<AiSave> {
        let (redaction, limits) = match self.limits.parse() {
            Ok(parsed) => parsed,
            Err(e) => {
                self.limits.set_error(e);
                return None;
            }
        };
        let mut next = self.config.clone();
        next.redaction = redaction;
        next.limits = limits;
        if let Err(problem) = next.validate() {
            self.limits.set_error(DraftError {
                field: None,
                message: problem.to_string(),
            });
            return None;
        }
        self.config = next;
        // Shows the normalised entries (lowercase, without duplicates).
        self.limits = LimitsDraft::from_config(&self.config);
        self.bump();
        Some(AiSave {
            config: self.config.clone(),
            secrets: Vec::new(),
        })
    }

    /// Puts the models the last test listed into the editor.
    pub(crate) fn use_listed_models(&mut self) {
        let AiTestStatus::Ok(models) = &self.test else {
            return;
        };
        let text = models.join(", ");
        if let Some(draft) = &mut self.draft {
            draft.set_text(AiField::Models, text);
            self.bump();
        }
    }

    fn start_test(&mut self, provider: ProviderConfig, key: Option<Secret>) -> TestRequest {
        self.next_test += 1;
        self.test = AiTestStatus::Running(self.next_test);
        TestRequest {
            request: self.next_test,
            provider,
            key,
        }
    }

    /// Starts a test of the editor's provider with the typed key. `None` when the editor is
    /// closed or invalid (it shows why).
    pub(crate) fn begin_draft_test(&mut self) -> Option<TestRequest> {
        let draft = self.draft.as_mut()?;
        let candidate = match draft.candidate(&self.config) {
            Ok(c) => c,
            Err(e) => {
                draft.set_error(e);
                return None;
            }
        };
        let key = match candidate.key {
            KeyPlan::Set(secret) => Some(secret),
            KeyPlan::Keep | KeyPlan::Clear => None,
        };
        self.test_provider = None;
        Some(self.start_test(candidate.provider, key))
    }

    /// Starts a test of a saved provider (its key is read from the keychain).
    pub(crate) fn begin_provider_test(&mut self, id: &ProviderId) -> Option<TestRequest> {
        let provider = self.config.provider(id)?.clone();
        self.test_provider = Some(id.clone());
        Some(self.start_test(provider, None))
    }

    /// Records the result of the test `request`. A result of an older test is ignored (`None`).
    pub(crate) fn finish_test(
        &mut self,
        request: u64,
        result: Result<Vec<String>, String>,
    ) -> Option<TestReport> {
        if self.test != AiTestStatus::Running(request) {
            return None;
        }
        let count = result.as_ref().map(Vec::len).map_err(Clone::clone);
        self.test = match result {
            Ok(models) => AiTestStatus::Ok(models),
            Err(message) => AiTestStatus::Failed(message),
        };
        Some(TestReport {
            provider: self.test_provider.clone(),
            result: count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiProviderFactory as _;
    use crate::ai::provider::NoProviders;
    use sqler_config::ProviderKind as K;

    fn id(s: &str) -> ProviderId {
        ProviderId::new(s).unwrap()
    }

    fn state() -> AiState {
        AiState::for_config(AiConfig::default())
    }

    fn add_ollama(s: &mut AiState) {
        assert!(s.open_new(K::Ollama));
        s.set_text(AiField::Models, "m1, m2".into());
        assert!(s.save_provider().is_some());
        s.close_draft();
    }

    #[test]
    fn from_startup_reads_kinds_and_config() {
        let mut config = AiConfig::default();
        config
            .providers
            .push(ProviderConfig::new(id("a"), K::Ollama));
        let startup = AiStartup {
            config: Ok(config),
            dir: None,
            factory: std::sync::Arc::new(NoProviders),
        };
        let s = AiState::from_startup(&startup);
        assert_eq!(s.config().providers.len(), 1);
        assert_eq!(s.kinds().len(), 0);
        assert!(!s.persistent());
        assert_eq!(s.load_error(), None);
        assert_eq!(NoProviders.kinds().len(), 0);
    }

    #[test]
    fn a_broken_file_gives_the_default_and_the_reason() {
        let startup = AiStartup {
            config: Err("line 1".into()),
            dir: None,
            factory: std::sync::Arc::new(NoProviders),
        };
        let s = AiState::from_startup(&startup);
        assert_eq!(s.load_error(), Some("line 1"));
        assert_eq!(s.config().providers.len(), 0);
    }

    #[test]
    fn opening_the_editor_bumps_the_generation_and_unknown_kinds_are_refused() {
        let mut s = AiState::new(AiConfig::default(), vec![K::Ollama], None);
        let g = s.inputs_generation();
        assert!(!s.open_new(K::OpenAi));
        assert!(s.draft().is_none());
        assert!(s.open_new(K::Ollama));
        assert!(s.inputs_generation() > g);
        let g = s.inputs_generation();
        s.close_draft();
        assert!(s.inputs_generation() > g);
        assert!(s.draft().is_none());
    }

    #[test]
    fn saving_a_provider_stores_the_key_only_as_a_secret_op() {
        let mut s = state();
        assert!(s.open_new(K::Anthropic));
        s.set_text(AiField::ProviderId, "claude".into());
        s.set_text(AiField::Models, "c1".into());
        assert!(s.save_provider().is_none(), "no key yet");
        assert_eq!(
            s.draft().unwrap().error().unwrap().message,
            "An API key is needed for Anthropic."
        );
        s.set_text(AiField::Key, "sk-test-123".into());
        let save = s.save_provider().unwrap();
        assert!(matches!(
            save.secrets.as_slice(),
            [SecretOp::Set(r, _)] if r.to_string() == "claude:ai-provider-key"
        ));
        assert!(save.config.providers[0].api_key_in_keychain);
        assert!(!format!("{save:?}").contains("sk-test-123"));
        // The key is masked afterwards.
        let d = s.draft().unwrap();
        assert!(d.is_editing() && d.key_kept() && !d.key_typed() && d.saved());
        assert!(!format!("{s:?} {d:?}").contains("sk-test-123"));
    }

    #[test]
    fn editing_without_a_new_key_keeps_the_keychain_alone() {
        let mut s = state();
        s.open_new(K::OpenAi);
        s.set_text(AiField::Key, "k".into());
        s.save_provider().unwrap();
        s.close_draft();
        assert!(s.open_edit(&id("openai")));
        s.set_text(AiField::Models, "gpt".into());
        let save = s.save_provider().unwrap();
        assert!(save.secrets.is_empty());
        assert_eq!(save.config.providers[0].models, ["gpt"]);
    }

    #[test]
    fn clearing_the_key_deletes_it() {
        let mut s = state();
        s.open_new(K::Ollama);
        s.set_text(AiField::Key, "k".into());
        s.save_provider().unwrap();
        s.clear_key();
        let save = s.save_provider().unwrap();
        assert!(matches!(save.secrets.as_slice(), [SecretOp::Delete(_)]));
        assert!(!save.config.providers[0].api_key_in_keychain);
    }

    #[test]
    fn removing_a_provider_clears_assignments_and_deletes_the_key() {
        let mut s = state();
        add_ollama(&mut s);
        s.open_new(K::Anthropic);
        s.set_text(AiField::Models, "c1".into());
        s.set_text(AiField::Key, "k".into());
        s.save_provider().unwrap();
        s.close_draft();
        s.set_model(None, Some(ModelChoice::new(id("claude-x"), "c1")))
            .unwrap_err();
        s.set_model(None, Some(ModelChoice::new(id("anthropic"), "c1")))
            .unwrap();
        s.set_model(
            Some(UseCase::Fix),
            Some(ModelChoice::new(id("ollama"), "m1")),
        )
        .unwrap();
        let save = s.remove_provider(&id("anthropic")).unwrap();
        assert!(matches!(save.secrets.as_slice(), [SecretOp::Delete(_)]));
        assert_eq!(save.config.providers.len(), 1);
        assert!(save.config.models.default.is_none());
        assert!(save.config.models.fix.is_some());
        assert!(s.remove_provider(&id("anthropic")).is_none());
    }

    #[test]
    fn removing_the_provider_in_the_editor_closes_it() {
        let mut s = state();
        add_ollama(&mut s);
        s.open_edit(&id("ollama"));
        s.remove_provider(&id("ollama")).unwrap();
        assert!(s.draft().is_none());
    }

    #[test]
    fn removing_a_listed_model_that_is_chosen_is_refused() {
        let mut s = state();
        add_ollama(&mut s);
        s.set_model(None, Some(ModelChoice::new(id("ollama"), "m1")))
            .unwrap();
        s.open_edit(&id("ollama"));
        s.set_text(AiField::Models, "m2".into());
        assert!(s.save_provider().is_none());
        assert_eq!(
            s.draft().unwrap().error().unwrap().field,
            Some(AiField::Models)
        );
    }

    #[test]
    fn limits_save_and_normalise() {
        let mut s = state();
        s.set_text(AiField::Redaction, "IBAN, iban, Phone".into());
        s.set_text(AiField::MaxRows, "50".into());
        s.set_unlimited(true);
        let g = s.inputs_generation();
        let save = s.save_limits().unwrap();
        assert_eq!(save.config.redaction.extra(), ["iban", "phone"]);
        assert_eq!(save.config.limits.max_rows, 50);
        assert!(save.config.limits.unlimited);
        assert_eq!(s.limits().text(AiField::Redaction), "iban, phone");
        assert!(s.inputs_generation() > g);
        s.set_text(AiField::TimeoutSecs, "0".into());
        assert!(s.save_limits().is_none());
        assert_eq!(
            s.limits().error().unwrap().field,
            Some(AiField::TimeoutSecs)
        );
        assert_eq!(
            s.config().limits.timeout_secs,
            30,
            "the saved value is kept"
        );
    }

    #[test]
    fn a_test_result_of_another_request_is_ignored() {
        let mut s = state();
        s.open_new(K::Ollama);
        let first = s.begin_draft_test().unwrap();
        assert_eq!(*s.test(), AiTestStatus::Running(first.request));
        let second = s.begin_draft_test().unwrap();
        assert_ne!(first.request, second.request);
        assert!(s.finish_test(first.request, Ok(vec!["a".into()])).is_none());
        assert_eq!(*s.test(), AiTestStatus::Running(second.request));
        let report = s
            .finish_test(second.request, Ok(vec!["a".into(), "b".into()]))
            .unwrap();
        assert_eq!(report.provider, None);
        assert_eq!(report.result, Ok(2));
        assert_eq!(*s.test(), AiTestStatus::Ok(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn the_draft_test_passes_the_typed_key_and_lists_models_into_the_editor() {
        let mut s = state();
        s.open_new(K::OpenAi);
        assert!(s.begin_draft_test().is_none(), "a key is needed");
        s.set_text(AiField::Key, "sk-test-123".into());
        let t = s.begin_draft_test().unwrap();
        assert_eq!(t.key.as_ref().map(Secret::expose), Some("sk-test-123"));
        s.finish_test(t.request, Ok(vec!["m1".into(), "m2".into()]));
        s.use_listed_models();
        assert_eq!(s.draft().unwrap().models(), "m1, m2");
    }

    #[test]
    fn closing_the_editor_forgets_its_test() {
        let mut s = state();
        s.open_new(K::Ollama);
        let t = s.begin_draft_test().unwrap();
        s.close_draft();
        assert_eq!(*s.test(), AiTestStatus::Idle);
        assert!(s.finish_test(t.request, Ok(vec![])).is_none());
    }

    #[test]
    fn a_saved_provider_test_reports_its_id() {
        let mut s = state();
        add_ollama(&mut s);
        let t = s.begin_provider_test(&id("ollama")).unwrap();
        assert!(t.key.is_none());
        let r = s.finish_test(t.request, Err("down".into())).unwrap();
        assert_eq!(r.provider, Some(id("ollama")));
        assert_eq!(r.result, Err("down".to_owned()));
        assert_eq!(*s.test(), AiTestStatus::Failed("down".into()));
        assert!(s.begin_provider_test(&id("nope")).is_none());
    }
}
