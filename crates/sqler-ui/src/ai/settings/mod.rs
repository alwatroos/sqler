// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The drafts behind the AI section of the settings page: the provider editor and the limits
//! and redaction form. Plain Rust: the view only renders them.
//!
//! A typed API key lives in a [`KeyPlan`] (a [`Secret`], whose `Debug` shows `***`) until it is
//! handed to the keychain; it never reaches `ai.toml`, a notice or a `Debug` output.

use std::collections::HashSet;

use sqler_config::{
    AI_MAX_ROWS_RANGE, AI_TIMEOUT_SECS_RANGE, AiConfig, AiConfigProblem, AiLimits, ProviderConfig,
    ProviderId, ProviderKind, RedactionList, Secret,
};

use super::kind_label;

#[cfg(feature = "gui")]
pub(crate) mod view;

/// A text field of the AI settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiField {
    /// The id of the provider being edited.
    ProviderId,
    /// The provider's endpoint.
    Endpoint,
    /// The provider's models, comma separated.
    Models,
    /// The API key (never shown again after a save).
    Key,
    /// The extra redaction entries.
    Redaction,
    /// The row cap for AI-issued queries.
    MaxRows,
    /// The time cap for AI-issued queries, in seconds.
    TimeoutSecs,
}

/// The state of a "Test connection" run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AiTestStatus {
    /// No test ran, or the result was discarded.
    #[default]
    Idle,
    /// A test is running; the number tells it from an older one.
    Running(u64),
    /// The provider answered with these models.
    Ok(Vec<String>),
    /// The test failed with this message (never a key).
    Failed(String),
}

/// What happens to the stored API key on save.
#[derive(Debug, Clone, Default)]
pub enum KeyPlan {
    /// Leave the keychain entry as it is.
    #[default]
    Keep,
    /// Store this key.
    Set(Secret),
    /// Delete the stored key.
    Clear,
}

/// Why a draft cannot be applied: the field to mark (if any) and a message that never echoes
/// the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftError {
    /// The field the problem belongs to.
    pub field: Option<AiField>,
    /// What is wrong.
    pub message: String,
}

impl DraftError {
    fn at(field: AiField, message: impl Into<String>) -> Self {
        Self {
            field: Some(field),
            message: message.into(),
        }
    }
}

/// A provider that passed validation: the provider, the key plan and the whole configuration
/// with the provider in place.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The provider as it will be saved.
    pub provider: ProviderConfig,
    /// What to do with the key.
    pub key: KeyPlan,
    /// The configuration with this provider added or replaced.
    pub config: AiConfig,
}

/// Whether a provider of this kind cannot work without an API key.
#[must_use]
pub fn needs_key(kind: ProviderKind) -> bool {
    kind != ProviderKind::Ollama
}

/// `base`, or `base-2`, `base-3`, ... the first id no provider of `config` has.
#[must_use]
pub fn unique_id(base: &str, config: &AiConfig) -> String {
    let taken: HashSet<&str> = config.providers.iter().map(|p| p.id.as_str()).collect();
    if !taken.contains(base) {
        return base.to_owned();
    }
    (2..=config.providers.len() + 2)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(id.as_str()))
        .unwrap_or_else(|| base.to_owned())
}

/// Splits a comma or newline separated list; entries are trimmed, empty ones dropped.
fn split_list(text: &str) -> Vec<String> {
    text.split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The models typed in the editor: trimmed, without empty entries and duplicates.
#[must_use]
pub fn parse_models(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    split_list(text)
        .into_iter()
        .filter(|m| seen.insert(m.clone()))
        .collect()
}

/// The provider editor: a new provider or an existing one being changed.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)] // independent flags: existing, typed id, stored key, saved
pub struct ProviderDraft {
    editing: bool,
    id: String,
    id_touched: bool,
    kind: ProviderKind,
    endpoint: String,
    models: String,
    key: KeyPlan,
    key_stored: bool,
    error: Option<DraftError>,
    saved: bool,
}

// The typed endpoint can carry credentials (`https://user:token@host`) and the model text is
// free input: the Debug output shows whether they are set, never what they say.
impl std::fmt::Debug for ProviderDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = match &self.key {
            KeyPlan::Keep => "Keep",
            KeyPlan::Set(_) => "Set(***)",
            KeyPlan::Clear => "Clear",
        };
        f.debug_struct("ProviderDraft")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("editing", &self.editing)
            .field("key", &format_args!("{key}"))
            .field("key_stored", &self.key_stored)
            .field("endpoint_set", &!self.endpoint.is_empty())
            .field("models_set", &!self.models.is_empty())
            .field("error", &self.error)
            .field("saved", &self.saved)
            .finish_non_exhaustive()
    }
}

impl ProviderDraft {
    /// An empty editor for a new provider of `kind`; the id is the kind's id made unique.
    #[must_use]
    pub fn new_provider(kind: ProviderKind, config: &AiConfig) -> Self {
        Self {
            editing: false,
            id: unique_id(kind.as_str(), config),
            id_touched: false,
            kind,
            endpoint: String::new(),
            models: String::new(),
            key: KeyPlan::Keep,
            key_stored: false,
            error: None,
            saved: false,
        }
    }

    /// An editor filled from a saved provider. The id cannot change (the key lives under it).
    #[must_use]
    pub fn edit(provider: &ProviderConfig) -> Self {
        Self {
            editing: true,
            id: provider.id.to_string(),
            id_touched: true,
            kind: provider.kind,
            endpoint: provider.endpoint.clone().unwrap_or_default(),
            models: provider.models.join(", "),
            key: KeyPlan::Keep,
            key_stored: provider.api_key_in_keychain,
            error: None,
            saved: false,
        }
    }

    /// Whether an existing provider is being edited (its id is read-only).
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// The id text.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The provider kind.
    #[must_use]
    pub fn kind(&self) -> ProviderKind {
        self.kind
    }

    /// The endpoint text; empty means the provider's default.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The models text.
    #[must_use]
    pub fn models(&self) -> &str {
        &self.models
    }

    /// What will happen to the key on save.
    #[must_use]
    pub fn key_plan(&self) -> &KeyPlan {
        &self.key
    }

    /// Whether a key is kept in the keychain (and not marked for removal).
    #[must_use]
    pub fn key_kept(&self) -> bool {
        self.key_stored && matches!(self.key, KeyPlan::Keep)
    }

    /// Whether a key was typed and is waiting for the save.
    #[must_use]
    pub fn key_typed(&self) -> bool {
        matches!(self.key, KeyPlan::Set(_))
    }

    /// The last validation error.
    #[must_use]
    pub fn error(&self) -> Option<&DraftError> {
        self.error.as_ref()
    }

    /// Whether the draft was saved and not changed since.
    #[must_use]
    pub fn saved(&self) -> bool {
        self.saved
    }

    /// The text a field of the editor shows (the key never).
    #[must_use]
    pub fn text(&self, field: AiField) -> &str {
        match field {
            AiField::ProviderId => &self.id,
            AiField::Endpoint => &self.endpoint,
            AiField::Models => &self.models,
            _ => "",
        }
    }

    /// Sets a text field; fields of the limits form are ignored.
    pub fn set_text(&mut self, field: AiField, text: String) {
        match field {
            AiField::ProviderId => {
                if !self.editing {
                    self.id = text;
                    self.id_touched = true;
                }
            }
            AiField::Endpoint => self.endpoint = text,
            AiField::Models => self.models = text,
            AiField::Key => {
                let typed = text.trim();
                self.key = if typed.is_empty() {
                    KeyPlan::Keep
                } else {
                    KeyPlan::Set(Secret::new(typed))
                };
            }
            _ => return,
        }
        self.error = None;
        self.saved = false;
    }

    /// Changes the kind of a new provider; an id the user did not type follows the kind.
    pub fn set_kind(&mut self, kind: ProviderKind, config: &AiConfig) {
        if self.editing {
            return;
        }
        if self.kind != kind {
            // A key typed for one provider kind must not be saved for another.
            self.key = KeyPlan::Keep;
        }
        self.kind = kind;
        if !self.id_touched {
            self.id = unique_id(kind.as_str(), config);
        }
        self.error = None;
        self.saved = false;
    }

    /// Marks the stored key for removal on save.
    pub fn clear_key(&mut self) {
        self.key = KeyPlan::Clear;
        self.error = None;
        self.saved = false;
    }

    /// Records a validation error.
    pub fn set_error(&mut self, error: DraftError) {
        self.error = Some(error);
    }

    /// After a successful save: the draft now edits the saved provider, the key is kept and the
    /// typed text is forgotten.
    pub(crate) fn after_save(&mut self, key_stored: bool) {
        self.editing = true;
        self.id_touched = true;
        self.key = KeyPlan::Keep;
        self.key_stored = key_stored;
        self.error = None;
        self.saved = true;
    }

    /// Checks the draft against `config` and returns the provider, the key plan and the new
    /// configuration. The messages never echo the input.
    ///
    /// # Errors
    /// A [`DraftError`] for the first thing wrong.
    pub fn candidate(&self, config: &AiConfig) -> Result<Candidate, DraftError> {
        let id = ProviderId::new(self.id.trim()).map_err(|_| {
            DraftError::at(
                AiField::ProviderId,
                "Use 1 to 64 lowercase letters, digits or '-', starting with a letter or digit.",
            )
        })?;
        if !self.editing && config.provider(&id).is_some() {
            return Err(DraftError::at(
                AiField::ProviderId,
                "A provider with this id already exists.",
            ));
        }
        let has_key = match self.key {
            KeyPlan::Set(_) => true,
            KeyPlan::Keep => self.key_stored,
            KeyPlan::Clear => false,
        };
        if needs_key(self.kind) && !has_key {
            return Err(DraftError::at(
                AiField::Key,
                format!("An API key is needed for {}.", kind_label(self.kind)),
            ));
        }
        let endpoint = self.endpoint.trim();
        let mut provider = ProviderConfig::new(id.clone(), self.kind)
            .with_models(parse_models(&self.models))
            .with_api_key_in_keychain(has_key);
        if !endpoint.is_empty() {
            provider = provider.with_endpoint(endpoint);
        }
        let mut next = config.clone();
        match next.providers.iter_mut().find(|p| p.id == id) {
            Some(slot) => *slot = provider.clone(),
            None => next.providers.push(provider.clone()),
        }
        next.validate().map_err(|p| problem_error(&p))?;
        Ok(Candidate {
            provider,
            key: self.key.clone(),
            config: next,
        })
    }
}

/// Maps a configuration problem to the field it belongs to, without echoing any input.
fn problem_error(problem: &AiConfigProblem) -> DraftError {
    match problem {
        AiConfigProblem::InvalidProviderId { .. } | AiConfigProblem::DuplicateProvider(_) => {
            DraftError::at(AiField::ProviderId, "This provider id cannot be used.")
        }
        AiConfigProblem::InvalidEndpoint { reason, .. } => {
            DraftError::at(AiField::Endpoint, format!("Endpoint: {reason}."))
        }
        AiConfigProblem::EmptyModelName { .. } => {
            DraftError::at(AiField::Models, "Model names must not be empty.")
        }
        AiConfigProblem::UnknownModel { use_case, .. } => DraftError::at(
            AiField::Models,
            format!(
                "The model chosen for {} is no longer listed; choose another model first.",
                use_case.map_or("the default", |u| u.as_str())
            ),
        ),
        other => DraftError {
            field: None,
            message: other.to_string(),
        },
    }
}

/// The redaction and limits form.
#[derive(Debug, Clone)]
pub struct LimitsDraft {
    redaction: String,
    max_rows: String,
    timeout_secs: String,
    unlimited: bool,
    error: Option<DraftError>,
}

impl Default for LimitsDraft {
    fn default() -> Self {
        Self::from_config(&AiConfig::default())
    }
}

impl LimitsDraft {
    /// The form filled from the saved configuration.
    #[must_use]
    pub fn from_config(config: &AiConfig) -> Self {
        Self {
            redaction: config.redaction.extra().join(", "),
            max_rows: config.limits.max_rows.to_string(),
            timeout_secs: config.limits.timeout_secs.to_string(),
            unlimited: config.limits.unlimited,
            error: None,
        }
    }

    /// The text of a field.
    #[must_use]
    pub fn text(&self, field: AiField) -> &str {
        match field {
            AiField::Redaction => &self.redaction,
            AiField::MaxRows => &self.max_rows,
            AiField::TimeoutSecs => &self.timeout_secs,
            _ => "",
        }
    }

    /// Whether both caps are lifted (never on `prod`).
    #[must_use]
    pub fn unlimited(&self) -> bool {
        self.unlimited
    }

    /// The last validation error.
    #[must_use]
    pub fn error(&self) -> Option<&DraftError> {
        self.error.as_ref()
    }

    /// Sets a text field; fields of the provider editor are ignored.
    pub fn set_text(&mut self, field: AiField, text: String) {
        match field {
            AiField::Redaction => self.redaction = text,
            AiField::MaxRows => self.max_rows = text,
            AiField::TimeoutSecs => self.timeout_secs = text,
            _ => return,
        }
        self.error = None;
    }

    /// Lifts or restores the caps (applied on save).
    pub fn set_unlimited(&mut self, unlimited: bool) {
        self.unlimited = unlimited;
        self.error = None;
    }

    /// Records a validation error.
    pub fn set_error(&mut self, error: DraftError) {
        self.error = Some(error);
    }

    /// Parses the form.
    ///
    /// # Errors
    /// A [`DraftError`] naming the field; the message never echoes the input.
    pub fn parse(&self) -> Result<(RedactionList, AiLimits), DraftError> {
        let redaction = RedactionList::new(split_list(&self.redaction)).map_err(|p| match p {
            AiConfigProblem::InvalidRedactionEntry { index, reason } => DraftError::at(
                AiField::Redaction,
                format!("Entry {}: {reason}.", index + 1),
            ),
            other => DraftError::at(AiField::Redaction, other.to_string()),
        })?;
        let max_rows = parse_in(&self.max_rows, &AI_MAX_ROWS_RANGE)
            .ok_or_else(|| range_error(AiField::MaxRows, "Max rows", &AI_MAX_ROWS_RANGE))?;
        let timeout_secs = parse_in(&self.timeout_secs, &AI_TIMEOUT_SECS_RANGE)
            .ok_or_else(|| range_error(AiField::TimeoutSecs, "Timeout", &AI_TIMEOUT_SECS_RANGE))?;
        let mut limits = AiLimits::default();
        limits.max_rows = max_rows;
        limits.timeout_secs = timeout_secs;
        limits.unlimited = self.unlimited;
        Ok((redaction, limits))
    }
}

fn parse_in(text: &str, range: &std::ops::RangeInclusive<u32>) -> Option<u32> {
    text.trim()
        .parse::<u32>()
        .ok()
        .filter(|v| range.contains(v))
}

fn range_error(field: AiField, what: &str, range: &std::ops::RangeInclusive<u32>) -> DraftError {
    DraftError::at(
        field,
        format!(
            "{what} must be a whole number from {} to {}.",
            range.start(),
            range.end()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> ProviderId {
        ProviderId::new(s).unwrap()
    }

    fn config() -> AiConfig {
        let mut c = AiConfig::default();
        c.providers.push(
            ProviderConfig::new(id("ollama"), ProviderKind::Ollama).with_models(["m1", "m2"]),
        );
        c.providers.push(
            ProviderConfig::new(id("claude"), ProviderKind::Anthropic)
                .with_models(["c1"])
                .with_api_key_in_keychain(true),
        );
        c
    }

    #[test]
    fn new_ids_are_unique() {
        let c = config();
        assert_eq!(unique_id("openai", &c), "openai");
        assert_eq!(unique_id("ollama", &c), "ollama-2");
        let mut c = c;
        c.providers
            .push(ProviderConfig::new(id("ollama-2"), ProviderKind::Ollama));
        assert_eq!(unique_id("ollama", &c), "ollama-3");
        let d = ProviderDraft::new_provider(ProviderKind::Ollama, &c);
        assert_eq!(d.id(), "ollama-3");
    }

    #[test]
    fn an_untouched_id_follows_the_kind_and_a_typed_one_stays() {
        let c = config();
        let mut d = ProviderDraft::new_provider(ProviderKind::Ollama, &c);
        d.set_kind(ProviderKind::OpenAi, &c);
        assert_eq!(d.id(), "openai");
        d.set_text(AiField::ProviderId, "mine".into());
        d.set_kind(ProviderKind::Mistral, &c);
        assert_eq!(d.id(), "mine");
        assert_eq!(d.kind(), ProviderKind::Mistral);
    }

    #[test]
    fn changing_the_kind_drops_a_typed_key() {
        let c = config();
        let mut d = ProviderDraft::new_provider(ProviderKind::OpenAi, &c);
        d.set_text(AiField::Key, "sk-typed".into());
        assert!(d.key_typed());
        d.set_kind(ProviderKind::OpenAi, &c);
        assert!(d.key_typed(), "the same kind keeps the key");
        d.set_kind(ProviderKind::Mistral, &c);
        assert!(!d.key_typed());
        assert!(matches!(d.key_plan(), KeyPlan::Keep));
    }

    #[test]
    fn the_id_of_an_existing_provider_is_read_only() {
        let c = config();
        let mut d = ProviderDraft::edit(&c.providers[0]);
        d.set_text(AiField::ProviderId, "other".into());
        d.set_kind(ProviderKind::OpenAi, &c);
        assert_eq!(d.id(), "ollama");
        assert_eq!(d.kind(), ProviderKind::Ollama);
        assert!(d.is_editing());
    }

    #[test]
    fn models_are_trimmed_and_deduplicated() {
        assert_eq!(parse_models(" a, b ,,a\nc "), ["a", "b", "c"]);
        assert_eq!(parse_models(" , ").len(), 0);
    }

    #[test]
    fn an_ollama_provider_needs_no_key_and_an_empty_endpoint_is_the_default() {
        let c = AiConfig::default();
        let mut d = ProviderDraft::new_provider(ProviderKind::Ollama, &c);
        d.set_text(AiField::Models, "qwen, llama".into());
        let cand = d.candidate(&c).unwrap();
        assert_eq!(cand.provider.endpoint, None);
        assert_eq!(cand.provider.models, ["qwen", "llama"]);
        assert!(!cand.provider.api_key_in_keychain);
        assert_eq!(cand.config.providers.len(), 1);
    }

    #[test]
    fn remote_kinds_need_a_stored_or_typed_key() {
        let c = AiConfig::default();
        for (kind, name) in [
            (ProviderKind::Anthropic, "Anthropic"),
            (ProviderKind::OpenAi, "OpenAI"),
            (ProviderKind::Mistral, "Mistral"),
        ] {
            let mut d = ProviderDraft::new_provider(kind, &c);
            let e = d.candidate(&c).unwrap_err();
            assert_eq!(e.field, Some(AiField::Key));
            assert_eq!(e.message, format!("An API key is needed for {name}."));
            d.set_text(AiField::Key, "sk-test-123".into());
            let cand = d.candidate(&c).unwrap();
            assert!(cand.provider.api_key_in_keychain);
            assert!(matches!(cand.key, KeyPlan::Set(_)));
        }
    }

    #[test]
    fn a_stored_key_is_kept_unless_cleared() {
        let c = config();
        let mut d = ProviderDraft::edit(&c.providers[1]);
        assert!(d.key_kept());
        let cand = d.candidate(&c).unwrap();
        assert!(matches!(cand.key, KeyPlan::Keep));
        assert!(cand.provider.api_key_in_keychain);
        d.clear_key();
        assert!(!d.key_kept());
        assert_eq!(d.candidate(&c).unwrap_err().field, Some(AiField::Key));
    }

    #[test]
    fn typing_then_emptying_the_key_goes_back_to_keep() {
        let c = config();
        let mut d = ProviderDraft::edit(&c.providers[1]);
        d.set_text(AiField::Key, " k ".into());
        assert!(d.key_typed());
        d.set_text(AiField::Key, String::new());
        assert!(d.key_kept());
    }

    #[test]
    fn problems_map_to_fields_without_echoing_input() {
        let c = AiConfig::default();
        let mut d = ProviderDraft::new_provider(ProviderKind::Ollama, &c);
        d.set_text(AiField::Endpoint, "ftp://secret-host".into());
        let e = d.candidate(&c).unwrap_err();
        assert_eq!(e.field, Some(AiField::Endpoint));
        assert!(!e.message.contains("secret-host"), "{}", e.message);
        d.set_text(AiField::Endpoint, String::new());
        d.set_text(AiField::ProviderId, "Bad Id!".into());
        let e = d.candidate(&c).unwrap_err();
        assert_eq!(e.field, Some(AiField::ProviderId));
        assert!(!e.message.contains("Bad Id"));
    }

    #[test]
    fn a_duplicate_id_is_refused_for_a_new_provider() {
        let c = config();
        let mut d = ProviderDraft::new_provider(ProviderKind::Ollama, &c);
        d.set_text(AiField::ProviderId, "ollama".into());
        assert_eq!(
            d.candidate(&c).unwrap_err().message,
            "A provider with this id already exists."
        );
    }

    #[test]
    fn an_api_key_over_plain_http_to_a_remote_host_is_refused() {
        let c = AiConfig::default();
        let mut d = ProviderDraft::new_provider(ProviderKind::OpenAi, &c);
        d.set_text(AiField::Key, "sk-test-123".into());
        d.set_text(AiField::Endpoint, "http://example.com".into());
        let e = d.candidate(&c).unwrap_err();
        assert_eq!(e.field, Some(AiField::Endpoint));
        assert!(!e.message.contains("sk-test-123"));
        d.set_text(AiField::Endpoint, "http://127.0.0.1:8080".into());
        assert!(d.candidate(&c).is_ok());
    }

    #[test]
    fn the_debug_output_never_holds_the_typed_key() {
        let c = AiConfig::default();
        let mut d = ProviderDraft::new_provider(ProviderKind::OpenAi, &c);
        d.set_text(AiField::Key, "sk-test-123".into());
        assert!(!format!("{d:?}").contains("sk-test-123"));
        let cand = d.candidate(&c).unwrap();
        assert!(!format!("{cand:?}").contains("sk-test-123"));
    }

    #[test]
    fn the_debug_output_never_holds_the_typed_endpoint_or_models() {
        let c = AiConfig::default();
        let mut d = ProviderDraft::new_provider(ProviderKind::OpenAi, &c);
        d.set_text(AiField::Endpoint, "https://u:tok-9@host".into());
        d.set_text(AiField::Models, "secret-model".into());
        let text = format!("{d:?}");
        assert!(!text.contains("tok-9") && !text.contains("secret-model"));
        assert!(text.contains("endpoint_set: true") && text.contains("models_set: true"));
    }

    #[test]
    fn limits_parse_within_the_ranges() {
        let mut l = LimitsDraft::default();
        assert_eq!(l.text(AiField::MaxRows), "1000");
        l.set_text(AiField::Redaction, "Cust_Email,\n iban ,, token".into());
        l.set_text(AiField::MaxRows, " 500 ".into());
        l.set_text(AiField::TimeoutSecs, "60".into());
        l.set_unlimited(true);
        let (red, lim) = l.parse().unwrap();
        assert_eq!(red.extra(), ["cust_email", "iban"]);
        assert_eq!(
            (lim.max_rows, lim.timeout_secs, lim.unlimited),
            (500, 60, true)
        );
    }

    #[test]
    fn bad_limits_name_their_field() {
        for (field, text, expect) in [
            (AiField::MaxRows, "0", AiField::MaxRows),
            (AiField::MaxRows, "1000001", AiField::MaxRows),
            (AiField::MaxRows, "abc", AiField::MaxRows),
            (AiField::TimeoutSecs, "0", AiField::TimeoutSecs),
            (AiField::TimeoutSecs, "3601", AiField::TimeoutSecs),
            (AiField::TimeoutSecs, "-1", AiField::TimeoutSecs),
        ] {
            let mut l = LimitsDraft::default();
            l.set_text(field, text.into());
            let e = l.parse().unwrap_err();
            assert_eq!(e.field, Some(expect), "{text}");
            assert!(!e.message.contains(text) || text.len() < 2);
        }
        let mut l = LimitsDraft::default();
        l.set_text(AiField::Redaction, "ok, ".to_owned() + &"x".repeat(65));
        let e = l.parse().unwrap_err();
        assert_eq!(e.field, Some(AiField::Redaction));
        assert!(e.message.starts_with("Entry 2"), "{}", e.message);
    }

    #[test]
    fn limits_start_from_the_saved_configuration() {
        let mut c = AiConfig::default();
        c.redaction = RedactionList::new(["iban".to_owned()]).unwrap();
        let l = LimitsDraft::from_config(&c);
        assert_eq!(l.text(AiField::Redaction), "iban");
        assert!(!l.unlimited());
    }
}
